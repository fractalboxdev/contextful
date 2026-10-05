//! Filesystem primitives the store and the engine share: a private staging sibling and an
//! exclusive create that publishes a staged file in one step, so a reader meets the whole
//! file or none, and exactly one of several racing creators wins.
//!
//! The create renames without replacing (`renameat2(RENAME_NOREPLACE)` on Linux,
//! `renamex_np(RENAME_EXCL)` on macOS, `MoveFileExW` on Windows), falls back to a hard link
//! where the volume refuses the flag, and to a check-then-rename under an advisory lock on
//! the parent directory where it refuses both, as exFAT and FAT32 do.

use std::fs;
use std::io::{self, ErrorKind};
use std::path::{Path, PathBuf};

#[cfg(target_os = "macos")]
#[doc(hidden)]
pub mod test_volume;

fn nonce() -> String {
    let mut bytes = [0u8; 8];
    // A platform without a randomness source still yields a distinct name per process.
    #[cfg(not(all(target_arch = "wasm32", target_os = "unknown")))]
    let random = getrandom::fill(&mut bytes).is_ok();
    #[cfg(all(target_arch = "wasm32", target_os = "unknown"))]
    let random = false;
    if !random {
        bytes = u64::from(std::process::id()).to_le_bytes();
    }
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// A private sibling path for staging a write to `path`: `.<name>.<16 hex>.tmp`.
pub fn tmp_sibling(path: &Path) -> PathBuf {
    let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    path.with_file_name(format!(".{name}.{}.tmp", nonce()))
}

/// Create `path` holding `bytes` only where nothing is there; `false` when something is.
/// The bytes land in a staging sibling that [`create_exclusive`] publishes.
pub fn create_new(path: &Path, bytes: &[u8]) -> io::Result<bool> {
    let tmp = tmp_sibling(path);
    fs::write(&tmp, bytes)?;
    create_exclusive(&tmp, path)
}

/// Publish the staged file `tmp` at `path` only where nothing is there: `true` when it
/// lands, `false` when `path` exists, whose content then stays untouched. `tmp` is gone
/// afterwards in every outcome.
pub fn create_exclusive(tmp: &Path, path: &Path) -> io::Result<bool> {
    match rename_noreplace(tmp, path) {
        Ok(()) => return Ok(true),
        Err(e) if !flag_refused(&e) => return settle(tmp, Err(e)),
        Err(_) => {}
    }
    match fs::hard_link(tmp, path) {
        Ok(()) => return settle(tmp, Ok(())),
        Err(e) if !link_refused(&e) => return settle(tmp, Err(e)),
        Err(_) => {}
    }
    create_exclusive_locked(tmp, path)
}

/// Open a directory handle for syncing entries created or renamed inside it.
pub fn open_dir_for_sync(path: &Path) -> io::Result<fs::File> {
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        use windows_sys::Win32::Storage::FileSystem::FILE_FLAG_BACKUP_SEMANTICS;
        return fs::OpenOptions::new().read(true).write(true).custom_flags(FILE_FLAG_BACKUP_SEMANTICS).open(path);
    }
    #[cfg(not(windows))]
    {
        fs::File::open(path)
    }
}

/// The volume and complete file identifier of an open handle. Windows identifiers
/// retain all 128 bits required by ReFS.
#[derive(Debug, PartialEq, Eq)]
pub struct FileIdentity {
    volume: u64,
    file: u128,
}

/// Identity of the file held open by `file`, independent of its path.
pub fn file_identity(file: &fs::File) -> io::Result<FileIdentity> {
    #[cfg(unix)]
    { Ok(unix_identity(&file.metadata()?)) }
    #[cfg(windows)]
    { windows_file_id(file) }
    #[cfg(not(any(unix, windows)))]
    { let _ = file; Err(io::Error::from(ErrorKind::Unsupported)) }
}

#[cfg(unix)]
fn unix_identity(metadata: &fs::Metadata) -> FileIdentity {
    use std::os::unix::fs::MetadataExt;
    FileIdentity { volume: metadata.dev(), file: u128::from(metadata.ino()) }
}

/// Whether `path` still names `file`, including after another process renames an open
/// file and creates a replacement at its old path.
pub fn names_file(path: &Path, file: &fs::File) -> io::Result<bool> {
    #[cfg(unix)]
    {
        let named = match fs::metadata(path) {
            Ok(m) => m,
            Err(e) if e.kind() == ErrorKind::NotFound => return Ok(false),
            Err(e) => return Err(e),
        };
        Ok(unix_identity(&named) == file_identity(file)?)
    }
    #[cfg(windows)]
    {
        let named = match fs::File::open(path) {
            Ok(f) => f,
            Err(e) if e.kind() == ErrorKind::NotFound => return Ok(false),
            Err(e) => return Err(e),
        };
        Ok(file_identity(&named)? == file_identity(file)?)
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = (path, file);
        Err(io::Error::from(ErrorKind::Unsupported))
    }
}

#[cfg(windows)]
fn windows_file_id(file: &fs::File) -> io::Result<FileIdentity> {
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::Foundation::{ERROR_INVALID_FUNCTION, ERROR_INVALID_PARAMETER, ERROR_NOT_SUPPORTED};
    use windows_sys::Win32::Storage::FileSystem::{
        FileIdInfo, GetFileInformationByHandle, GetFileInformationByHandleEx,
        GetVolumeInformationByHandleW, BY_HANDLE_FILE_INFORMATION, FILE_ID_INFO,
    };
    // SAFETY: FILE_ID_INFO is a plain C value filled through a correctly sized buffer.
    let mut full: FILE_ID_INFO = unsafe { std::mem::zeroed() };
    // SAFETY: the open handle and writable buffer remain valid throughout the call.
    if unsafe { GetFileInformationByHandleEx(file.as_raw_handle(), FileIdInfo,
        (&mut full as *mut FILE_ID_INFO).cast(), std::mem::size_of::<FILE_ID_INFO>() as u32) } != 0 {
        return Ok(FileIdentity { volume: full.VolumeSerialNumber, file: u128::from_le_bytes(full.FileId.Identifier) });
    }
    let error = io::Error::last_os_error();
    if !error.raw_os_error().is_some_and(|code| [ERROR_INVALID_FUNCTION, ERROR_INVALID_PARAMETER, ERROR_NOT_SUPPORTED].contains(&(code as u32))) {
        return Err(error);
    }
    // FAT-family volumes may expose only legacy IDs. ReFS never falls back to an ID
    // whose truncated width cannot distinguish every file on the volume.
    let mut filesystem = [0u16; 32];
    // SAFETY: optional outputs are null; the filesystem buffer has the stated capacity.
    if unsafe { GetVolumeInformationByHandleW(file.as_raw_handle(), std::ptr::null_mut(), 0,
        std::ptr::null_mut(), std::ptr::null_mut(), std::ptr::null_mut(), filesystem.as_mut_ptr(), filesystem.len() as u32) } == 0 {
        return Err(io::Error::last_os_error());
    }
    let end = filesystem.iter().position(|c| *c == 0).unwrap_or(filesystem.len());
    if !matches!(String::from_utf16_lossy(&filesystem[..end]).to_ascii_uppercase().as_str(), "FAT" | "FAT32" | "EXFAT") {
        return Err(error);
    }
    // SAFETY: the information structure is a plain C value that the call fills.
    let mut info: BY_HANDLE_FILE_INFORMATION = unsafe { std::mem::zeroed() };
    // SAFETY: `file` keeps this valid handle open throughout the call, and `info` is writable.
    if unsafe { GetFileInformationByHandle(file.as_raw_handle(), &mut info) } == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(FileIdentity { volume: u64::from(info.dwVolumeSerialNumber),
        file: (u128::from(info.nFileIndexHigh) << 32) | u128::from(info.nFileIndexLow) })
}

/// The create for a volume refusing both no-replace renames and hard links: under an
/// advisory lock on the parent directory, rename `tmp` onto `path` only while `path` is
/// absent. It excludes every other creator going through this primitive; a plain rename
/// onto `path` by other code is outside its reach.
#[cfg(unix)]
pub fn create_exclusive_locked(tmp: &Path, path: &Path) -> io::Result<bool> {
    let dir = match path.parent() {
        Some(d) if !d.as_os_str().is_empty() => d,
        _ => Path::new("."),
    };
    let held = fs::File::open(dir).and_then(|d| d.lock().map(|()| d));
    let _dir_lock = match held {
        Ok(d) => d,
        Err(e) => return settle(tmp, Err(e)),
    };
    match fs::symlink_metadata(path) {
        Ok(_) => settle(tmp, Err(io::Error::from(ErrorKind::AlreadyExists))),
        Err(e) if e.kind() == ErrorKind::NotFound => match fs::rename(tmp, path) {
            Ok(()) => Ok(true),
            Err(e) => settle(tmp, Err(e)),
        },
        Err(e) => settle(tmp, Err(e)),
    }
}

/// A platform without directory locks publishes through no check-then-rename.
#[cfg(not(unix))]
pub fn create_exclusive_locked(tmp: &Path, _path: &Path) -> io::Result<bool> {
    settle(tmp, Err(io::Error::new(ErrorKind::Unsupported, "no exclusive create on this volume")))
}

/// Remove `tmp` and read `outcome`: `AlreadyExists` is the refused create.
fn settle(tmp: &Path, outcome: io::Result<()>) -> io::Result<bool> {
    let _ = fs::remove_file(tmp);
    match outcome {
        Ok(()) => Ok(true),
        Err(e) if e.kind() == ErrorKind::AlreadyExists => Ok(false),
        Err(e) => Err(e),
    }
}

#[cfg(unix)]
fn errno_in(e: &io::Error, codes: &[i32]) -> bool {
    e.raw_os_error().is_some_and(|c| codes.contains(&c))
}

/// Whether a no-replace rename failed because the kernel or the volume lacks the flag.
fn flag_refused(e: &io::Error) -> bool {
    #[cfg(unix)]
    {
        errno_in(e, &[libc::ENOTSUP, libc::EOPNOTSUPP, libc::EINVAL, libc::ENOSYS])
    }
    #[cfg(windows)]
    {
        use windows_sys::Win32::Foundation::{ERROR_INVALID_FUNCTION, ERROR_NOT_SUPPORTED};
        e.raw_os_error().is_some_and(|c| c == ERROR_INVALID_FUNCTION as i32 || c == ERROR_NOT_SUPPORTED as i32)
    }
    #[cfg(not(any(unix, windows)))]
    {
        e.kind() == ErrorKind::Unsupported
    }
}

/// Whether a hard link failed because the volume holds no links: Linux FAT drivers answer
/// `EPERM`, macOS exFAT `ENOTSUP`.
fn link_refused(e: &io::Error) -> bool {
    #[cfg(unix)]
    {
        errno_in(e, &[libc::ENOTSUP, libc::EOPNOTSUPP, libc::EPERM, libc::ENOSYS])
    }
    #[cfg(windows)]
    {
        use windows_sys::Win32::Foundation::{ERROR_INVALID_FUNCTION, ERROR_NOT_SUPPORTED};
        e.raw_os_error().is_some_and(|c| c == ERROR_INVALID_FUNCTION as i32 || c == ERROR_NOT_SUPPORTED as i32)
    }
    #[cfg(not(any(unix, windows)))]
    {
        e.kind() == ErrorKind::Unsupported
    }
}

#[cfg(unix)]
fn c_path(p: &Path) -> io::Result<std::ffi::CString> {
    use std::os::unix::ffi::OsStrExt;
    std::ffi::CString::new(p.as_os_str().as_bytes()).map_err(|_| io::Error::from(ErrorKind::InvalidInput))
}

#[cfg(any(target_os = "linux", target_os = "android"))]
fn rename_noreplace(from: &Path, to: &Path) -> io::Result<()> {
    let (from, to) = (c_path(from)?, c_path(to)?);
    // The raw call reaches the kernel whatever C library links, and answers ENOSYS on a
    // kernel predating renameat2.
    // SAFETY: both pointers are NUL-terminated strings alive for the call.
    let rc = unsafe {
        libc::syscall(libc::SYS_renameat2, libc::AT_FDCWD, from.as_ptr(), libc::AT_FDCWD, to.as_ptr(), libc::RENAME_NOREPLACE)
    };
    if rc == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

#[cfg(any(target_os = "macos", target_os = "ios"))]
fn rename_noreplace(from: &Path, to: &Path) -> io::Result<()> {
    let (from, to) = (c_path(from)?, c_path(to)?);
    // SAFETY: both pointers are NUL-terminated strings alive for the call.
    let rc = unsafe { libc::renamex_np(from.as_ptr(), to.as_ptr(), libc::RENAME_EXCL) };
    if rc == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

#[cfg(windows)]
fn rename_noreplace(from: &Path, to: &Path) -> io::Result<()> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Storage::FileSystem::MoveFileExW;
    let wide = |path: &Path| -> io::Result<Vec<u16>> {
        let mut value: Vec<u16> = path.as_os_str().encode_wide().collect();
        if value.contains(&0) {
            return Err(io::Error::from(ErrorKind::InvalidInput));
        }
        value.push(0);
        Ok(value)
    };
    let (from, to) = (wide(from)?, wide(to)?);
    // No replacement or cross-volume copy flag: a second creator loses on the same volume.
    // SAFETY: both vectors hold NUL-terminated paths alive throughout the call.
    if unsafe { MoveFileExW(from.as_ptr(), to.as_ptr(), 0) } != 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

#[cfg(not(any(target_os = "linux", target_os = "android", target_os = "macos", target_os = "ios", windows)))]
fn rename_noreplace(_: &Path, _: &Path) -> io::Result<()> {
    #[cfg(unix)]
    return Err(io::Error::from_raw_os_error(libc::ENOTSUP));
    #[cfg(not(unix))]
    return Err(io::Error::from(ErrorKind::Unsupported));
}
