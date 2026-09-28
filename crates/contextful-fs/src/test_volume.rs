//! A scratch exFAT volume for test suites on macOS: a disk image `hdiutil` creates and
//! mounts, detached and removed on drop. exFAT holds no hard links and refuses
//! `RENAME_EXCL` onto an absent target, so a create on it takes the locked fallback.

use std::path::{Path, PathBuf};
use std::process::Command;

/// A mounted exFAT image.
pub struct ExfatVolume {
    dir: PathBuf,
    mount: PathBuf,
}

fn hdiutil(args: &[&str]) {
    let out = Command::new("hdiutil").args(args).output().expect("hdiutil runs on macOS");
    assert!(out.status.success(), "hdiutil {args:?}: {}", String::from_utf8_lossy(&out.stderr));
}

impl ExfatVolume {
    /// Create and mount a 32 MiB exFAT image under the system temporary directory.
    pub fn mount() -> ExfatVolume {
        let dir = std::env::temp_dir().join(format!("contextful-exfat-{}", super::nonce()));
        std::fs::create_dir_all(&dir).expect("temporary directory is writable");
        let image = dir.join("volume.dmg");
        let mount = dir.join("mnt");
        hdiutil(&["create", "-quiet", "-fs", "ExFAT", "-size", "32m", "-volname", "cfx", &image.to_string_lossy()]);
        hdiutil(&["attach", "-quiet", "-nobrowse", "-mountpoint", &mount.to_string_lossy(), &image.to_string_lossy()]);
        ExfatVolume { dir, mount }
    }

    /// The volume's root directory.
    pub fn path(&self) -> &Path {
        &self.mount
    }
}

impl Drop for ExfatVolume {
    fn drop(&mut self) {
        let _ = Command::new("hdiutil").args(["detach", "-quiet", "-force"]).arg(&self.mount).status();
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}
