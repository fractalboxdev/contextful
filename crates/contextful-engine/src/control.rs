//! The local snapshot directory: immutable `manifest@v<N>.toml` versions and the
//! `manifest@current` pointer naming the applied one (`surface.apply.local-claim`).
//!
//! A claim holds an advisory lock across the pointer read, the exclusive create of the
//! next version file and the pointer replace, so the pointer's compare-and-swap is
//! linearizable among processes on one machine (`surface.apply.version-race`).

use crate::fsutil::{create_new, filesystem_kind, replace, FileLock};
use contextful_core::surface::control::{admit_conditional, parse_pointer, snapshot_file, POINTER_FILE};
use contextful_core::surface::SurfaceError;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use serde::{Deserialize, Serialize};

/// The lock file serializing claims.
const LOCK_FILE: &str = "manifest.lock";
const DRAFT_FILE: &str = "manifest@draft.json";

/// A validated store draft bound to the applied version it read.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Draft {
    pub expected: u64,
    pub document: String,
}

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

    /// Refuse a directory on a filesystem whose exclusive create and lock are not
    /// linearizable (`surface.apply.weak-conditional-backend`).
    pub fn admit(&self) -> Result<(), ControlError> {
        let kind = filesystem_kind(&self.root);
        Ok(admit_conditional("the snapshot directory", &self.root.display().to_string(), kind.as_deref())?)
    }

    /// The applied version of a directory that took its guarded import. A directory holding
    /// no pointer, an empty one included, raises `StoreNotInitialized`
    /// (`surface.apply.uninitialized-store`).
    pub fn initialized(&self) -> Result<u64, ControlError> {
        self.current()?.ok_or_else(|| {
            SurfaceError::StoreNotInitialized(format!(
                "`{}` has taken no import; run `contextful pipeline import` once, then edit and apply",
                self.root.display()
            ))
            .into()
        })
    }

    /// The guarded import: claim v1 holding `text` only while the directory holds no pointer.
    /// A second import finds the pointer and claims nothing.
    pub fn import(&self, text: &str) -> Result<u64, ControlError> {
        if let Some(v) = self.current()? {
            return Err(ControlError::Storage(format!("`{}` took its import and stands at v{v}; `contextful pipeline apply` edits it", self.root.display())));
        }
        self.claim(None, text)
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

    /// Save one store draft only while the applied version still matches its base.
    pub fn save_draft(&self, draft: &Draft) -> Result<(), ControlError> {
        let storage = |e: contextful_core::run::Failure| ControlError::Storage(e.to_string());
        let _lock = FileLock::acquire(&self.root.join(LOCK_FILE)).map_err(storage)?;
        self.expect_version(draft.expected)?;
        let body = serde_json::to_vec(draft).map_err(|e| ControlError::Storage(e.to_string()))?;
        replace(&self.root.join(DRAFT_FILE), &body).map_err(storage)?;
        Ok(())
    }

    /// Read the saved draft without changing the applied pointer.
    pub fn read_draft(&self) -> Result<Draft, ControlError> {
        let path = self.root.join(DRAFT_FILE);
        let body = std::fs::read(&path).map_err(|e| match e.kind() {
            ErrorKind::NotFound => ControlError::Surface(SurfaceError::ControlDraftAbsent(format!("`{}` holds no validated draft", self.root.display()))),
            _ => ControlError::Storage(format!("{}: {e}", path.display())),
        })?;
        serde_json::from_slice(&body).map_err(|e| ControlError::Storage(format!("{}: {e}", path.display())))
    }

    /// Claim the exact draft revalidated by the caller under the existing pointer lock.
    pub fn claim_draft(&self, draft: &Draft) -> Result<u64, ControlError> {
        let storage = |e: contextful_core::run::Failure| ControlError::Storage(e.to_string());
        let _lock = FileLock::acquire(&self.root.join(LOCK_FILE)).map_err(storage)?;
        self.expect_version(draft.expected)?;
        if self.read_draft()? != *draft {
            return Err(SurfaceError::ManifestVersionConflict("the validated draft changed before apply; reload and reapply".into()).into());
        }
        let version = self.claim_locked(Some(draft.expected), &draft.document)?;
        let _ = std::fs::remove_file(self.root.join(DRAFT_FILE));
        Ok(version)
    }

    fn expect_version(&self, expected: u64) -> Result<(), ControlError> {
        let current = self.initialized()?;
        if current != expected {
            return Err(SurfaceError::ManifestVersionConflict(format!("the pointer names v{current} and the draft read v{expected}; reload v{current} and reapply")).into());
        }
        Ok(())
    }

    /// Claim the version after `expected` holding `text` and advance the pointer to it. A
    /// pointer no longer at `expected` raises `ManifestVersionConflict` and writes nothing;
    /// a version file already present is never overwritten, the claim taking the next free one.
    pub fn claim(&self, expected: Option<u64>, text: &str) -> Result<u64, ControlError> {
        let storage = |e: contextful_core::run::Failure| ControlError::Storage(e.to_string());
        let _lock = FileLock::acquire(&self.root.join(LOCK_FILE)).map_err(storage)?;
        self.claim_locked(expected, text)
    }

    fn claim_locked(&self, expected: Option<u64>, text: &str) -> Result<u64, ControlError> {
        let storage = |e: contextful_core::run::Failure| ControlError::Storage(e.to_string());
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
