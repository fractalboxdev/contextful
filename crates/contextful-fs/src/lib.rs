//! Filesystem primitives the store and the engine share: a private staging sibling and an
//! exclusive create that publishes a staged file in one step, so a reader meets the whole
//! file or none, and exactly one of several racing creators wins.
//!
//! The create renames without replacing (`renameat2(RENAME_NOREPLACE)` on Linux,
//! `renamex_np(RENAME_EXCL)` on macOS), falls back to a hard link where the volume refuses
//! the flag, and to a check-then-rename under an advisory lock on the parent directory where
//! it refuses both, as exFAT and FAT32 do.

use std::fs;
use std::io::{self, ErrorKind};
use std::path::{Path, PathBuf};

#[cfg(target_os = "macos")]
#[doc(hidden)]
pub mod test_volume;

fn nonce() -> String {
    let mut bytes = [0u8; 8];
    // A platform without a randomness source still yields a distinct name per process.
    if getrandom::fill(&mut bytes).is_err() {
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
    #[cfg(not(unix))]
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
    #[cfg(not(unix))]
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

#[cfg(not(any(target_os = "linux", target_os = "android", target_os = "macos", target_os = "ios")))]
fn rename_noreplace(_: &Path, _: &Path) -> io::Result<()> {
    #[cfg(unix)]
    return Err(io::Error::from_raw_os_error(libc::ENOTSUP));
    #[cfg(not(unix))]
    return Err(io::Error::from(ErrorKind::Unsupported));
}
