//! The filesystem primitives a snapshot directory and the engine's machine-local state
//! rest on: an atomic replace, an exclusive create, an advisory lock the kernel releases
//! when its holder exits, and the kind of filesystem holding a path.

use contextful_core::run::{Failure, FailureTag};
use contextful_fs::tmp_sibling;
use std::fs;
use std::path::Path;

/// A storage failure naming the path it met.
pub fn storage(path: &Path, e: impl std::fmt::Display) -> Failure {
    Failure::new(FailureTag::Storage, format!("{}: {e}", path.display()))
}

/// Replace `path` with `bytes` in one rename: a reader sees the old file or the new one.
pub fn replace(path: &Path, bytes: &[u8]) -> Result<(), Failure> {
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir).map_err(|e| storage(dir, e))?;
    }
    let tmp = tmp_sibling(path);
    fs::write(&tmp, bytes).map_err(|e| storage(&tmp, e))?;
    fs::rename(&tmp, path).map_err(|e| {
        let _ = fs::remove_file(&tmp);
        storage(path, e)
    })
}

/// Create `path` holding `bytes` only where none exists; `false` when one does.
pub fn create_new(path: &Path, bytes: &[u8]) -> Result<bool, Failure> {
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir).map_err(|e| storage(dir, e))?;
    }
    contextful_fs::create_new(path, bytes).map_err(|e| storage(path, e))
}

/// An OS advisory lock on a file. The kernel releases it when the holder exits, so a
/// crashed holder leaves no lock behind.
pub struct FileLock {
    _file: fs::File,
}

impl FileLock {
    /// Take the lock, waiting while another holder has it.
    pub fn acquire(path: &Path) -> Result<FileLock, Failure> {
        if let Some(dir) = path.parent() {
            fs::create_dir_all(dir).map_err(|e| storage(dir, e))?;
        }
        let file = fs::OpenOptions::new().read(true).write(true).create(true).truncate(false).open(path).map_err(|e| storage(path, e))?;
        file.lock().map_err(|e| storage(path, e))?;
        Ok(FileLock { _file: file })
    }
}

/// The kind of filesystem holding `path`, or its nearest existing ancestor: the mount's type
/// name on macOS and the BSDs, the name of a known filesystem magic on Linux, `None` where
/// neither reads.
pub fn filesystem_kind(path: &Path) -> Option<String> {
    let mut probe = path;
    while !probe.exists() {
        probe = probe.parent()?;
    }
    fs_kind(probe)
}

#[cfg(any(target_os = "macos", target_os = "freebsd", target_os = "openbsd"))]
fn fs_kind(path: &Path) -> Option<String> {
    use std::os::unix::ffi::OsStrExt;
    let c = std::ffi::CString::new(path.as_os_str().as_bytes()).ok()?;
    // SAFETY: an all-zero statfs is a valid value of the plain C struct.
    let mut st: libc::statfs = unsafe { std::mem::zeroed() };
    // SAFETY: `c` is a NUL-terminated path and `st` a writable statfs the call fills.
    if unsafe { libc::statfs(c.as_ptr(), &mut st) } != 0 {
        return None;
    }
    let name: Vec<u8> = st.f_fstypename.iter().take_while(|b| **b != 0).map(|b| *b as u8).collect();
    String::from_utf8(name).ok()
}

#[cfg(target_os = "linux")]
fn fs_kind(path: &Path) -> Option<String> {
    use std::os::unix::ffi::OsStrExt;
    let c = std::ffi::CString::new(path.as_os_str().as_bytes()).ok()?;
    // SAFETY: an all-zero statfs is a valid value of the plain C struct.
    let mut st: libc::statfs = unsafe { std::mem::zeroed() };
    // SAFETY: `c` is a NUL-terminated path and `st` a writable statfs the call fills.
    if unsafe { libc::statfs(c.as_ptr(), &mut st) } != 0 {
        return None;
    }
    #[allow(clippy::unnecessary_cast)]
    let name = match st.f_type as i64 {
        0x6969 => "nfs",
        0x517B => "smbfs",
        0xFF53_4D42 => "cifs",
        0xFE53_4D42 => "smb2",
        0x0102_1997 => "9p",
        0x6573_5546 => "fuse",
        _ => "local",
    };
    Some(name.to_string())
}

#[cfg(not(any(target_os = "macos", target_os = "freebsd", target_os = "openbsd", target_os = "linux")))]
fn fs_kind(_path: &Path) -> Option<String> {
    None
}
