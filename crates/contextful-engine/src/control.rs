//! The local snapshot directory: immutable `manifest@v<N>.toml` versions and the
//! `manifest@current` pointer naming the applied one (`surface.apply.local-claim`).
//!
//! A claim holds an advisory lock across the pointer read, the exclusive create of the
//! next version file and the pointer replace, so the pointer's compare-and-swap is
//! linearizable among processes on one machine (`surface.apply.version-race`).

use crate::fsutil::{create_new, replace, FileLock};
use contextful_core::surface::control::{parse_pointer, snapshot_file, POINTER_FILE};
use contextful_core::surface::SurfaceError;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};

/// The lock file serializing claims.
const LOCK_FILE: &str = "manifest.lock";

/// A snapshot-directory refusal or a storage failure beneath it.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ControlError {
    #[error(transparent)]
    Surface(#[from] SurfaceError),
    #[error("{0}")]
    Storage(String),
}

/// One project's snapshot directory.
#[derive(Debug, Clone)]
pub struct SnapshotDir {
    root: PathBuf,
}

impl SnapshotDir {
    pub fn open(root: &Path) -> SnapshotDir {
        SnapshotDir { root: root.to_path_buf() }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// The applied version, or `None` while no apply has claimed one. An unreadable pointer
    /// raises `ControlSnapshotUnreadable`; a body not wholly a version raises
    /// `ControlPointerMalformed`.
    pub fn current(&self) -> Result<Option<u64>, ControlError> {
        let path = self.root.join(POINTER_FILE);
        match std::fs::read_to_string(&path) {
            Ok(body) => Ok(Some(parse_pointer(&body)?)),
            Err(e) if e.kind() == ErrorKind::NotFound => Ok(None),
            Err(e) => Err(SurfaceError::ControlSnapshotUnreadable(format!("{}: {e}", path.display())).into()),
        }
    }

    /// The text of applied version `version`.
    pub fn read(&self, version: u64) -> Result<String, ControlError> {
        let path = self.root.join(snapshot_file(version));
        std::fs::read_to_string(&path).map_err(|e| SurfaceError::ControlSnapshotUnreadable(format!("{}: {e}", path.display())).into())
    }

    /// Claim the version after `expected` holding `text` and advance the pointer to it. A
    /// pointer no longer at `expected` raises `ManifestVersionConflict` and writes nothing;
    /// a version file already present is never overwritten, the claim taking the next free one.
    pub fn claim(&self, expected: Option<u64>, text: &str) -> Result<u64, ControlError> {
        let storage = |e: contextful_core::run::Failure| ControlError::Storage(e.to_string());
        let _lock = FileLock::acquire(&self.root.join(LOCK_FILE)).map_err(storage)?;
        let current = self.current()?;
        if current != expected {
            let name = |v: Option<u64>| v.map(|v| format!("v{v}")).unwrap_or_else(|| "no version".into());
            return Err(SurfaceError::ManifestVersionConflict(format!(
                "the pointer names {} and this apply read {}; reload {} and reapply",
                name(current),
                name(expected),
                name(current)
            ))
            .into());
        }
        let mut version = current.unwrap_or(0) + 1;
        while !create_new(&self.root.join(snapshot_file(version)), text.as_bytes()).map_err(storage)? {
            version += 1;
        }
        replace(&self.root.join(POINTER_FILE), format!("{version}\n").as_bytes()).map_err(storage)?;
        Ok(version)
    }
}
