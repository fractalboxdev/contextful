//! The local snapshot directory: immutable `manifest@v<N>.toml` versions and the
//! `manifest@current` pointer naming the applied one (`surface.apply.local-claim`).
//!
//! A claim holds an advisory lock across the pointer read, the exclusive create of the
//! next version file and the pointer replace, so the pointer's compare-and-swap is
//! linearizable among processes on one machine (`surface.apply.version-race`).

use crate::fsutil::{create_new, filesystem_kind, replace, FileLock};
use contextful_core::surface::control::{admit_conditional, parse_pointer, receipt_file, snapshot_file, POINTER_FILE};
use contextful_core::surface::SurfaceError;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};

/// The lock file serializing claims.
const LOCK_FILE: &str = "manifest.lock";

type ReceiptBuilder<'a> = dyn Fn(u64, Option<&str>) -> Result<String, ControlError> + 'a;
type ReceiptVerifier<'a> = dyn Fn(&str) -> Result<(), ControlError> + 'a;

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

    /// Install a verified receipt chain under the claim lock and publish its head last.
    pub fn adopt(&self, expected: Option<u64>, chain: &[(u64, Vec<u8>, Vec<u8>)], head: u64) -> Result<(), ControlError> {
        let storage = |e: contextful_core::run::Failure| ControlError::Storage(e.to_string());
        let _lock = FileLock::acquire(&self.root.join(LOCK_FILE)).map_err(storage)?;
        if self.current()? != expected {
            return Err(SurfaceError::ManifestVersionConflict(format!(
                "the applied pointer changed while adopting v{head}; reload its head"
            )).into());
        }
        for (version, snapshot, receipt) in chain {
            for (path, bytes) in [(self.root.join(snapshot_file(*version)), snapshot), (self.root.join(receipt_file(*version)), receipt)] {
                if !create_new(&path, bytes).map_err(storage)? {
                    let held = std::fs::read(&path).map_err(|e| ControlError::Storage(format!("{}: {e}", path.display())))?;
                    if held.as_slice() != bytes.as_slice() {
                        return Err(ControlError::Storage(format!("{} differs from the verified control chain", path.display())));
                    }
                }
            }
        }
        if expected != Some(head) {
            replace(&self.root.join(POINTER_FILE), format!("{head}\n").as_bytes()).map_err(storage)?;
        }
        Ok(())
    }

    /// Claim the version after `expected` holding `text` and advance the pointer to it. A
    /// pointer no longer at `expected` raises `ManifestVersionConflict` and writes nothing;
    /// a version file already present is never overwritten, the claim taking the next free one.
    pub fn claim(&self, expected: Option<u64>, text: &str) -> Result<u64, ControlError> {
        self.claim_inner(expected, text, None, None)
    }

    /// Claim a synced version with a receipt written beside its immutable snapshot before
    /// the applied pointer advances. `receipt` sees the prior receipt under the claim lock.
    pub fn claim_attested(
        &self,
        expected: Option<u64>,
        text: &str,
        receipt: impl Fn(u64, Option<&str>) -> Result<String, ControlError>,
    ) -> Result<u64, ControlError> {
        self.claim_inner(expected, text, Some(&receipt), None)
    }

    /// Import v1, resuming an identical unpublished snapshot and a verified receipt.
    /// The verifier binds an existing receipt to this snapshot and the admitted signer.
    pub fn import_attested(
        &self,
        text: &str,
        receipt: impl Fn(u64, Option<&str>) -> Result<String, ControlError>,
        verify: impl Fn(&str) -> Result<(), ControlError>,
    ) -> Result<u64, ControlError> {
        self.claim_inner(None, text, Some(&receipt), Some(&verify))
    }

    fn claim_inner(
        &self,
        expected: Option<u64>,
        text: &str,
        receipt: Option<&ReceiptBuilder<'_>>,
        recover: Option<&ReceiptVerifier<'_>>,
    ) -> Result<u64, ControlError> {
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
        if let (Some(sign), Some(verify)) = (receipt, recover) {
            let snapshot = self.root.join(snapshot_file(1));
            let receipt_path = self.root.join(receipt_file(1));
            let read = |path: &Path| match std::fs::read(path) {
                Ok(bytes) => Ok(Some(bytes)),
                Err(error) if error.kind() == ErrorKind::NotFound => Ok(None),
                Err(error) => Err(ControlError::Storage(format!("{}: {error}", path.display()))),
            };
            let prior_snapshot = read(&snapshot)?;
            if prior_snapshot.as_deref().is_some_and(|bytes| bytes != text.as_bytes()) {
                return Err(SurfaceError::ControlAttestationUnavailable("unpublished v1 snapshot differs from this import".into()).into());
            }
            // Signing rechecks the admitted authority even when the receipt already exists.
            let signed = sign(1, None)?;
            let prior_receipt = read(&receipt_path)?;
            if let Some(bytes) = &prior_receipt {
                if prior_snapshot.is_none() {
                    return Err(SurfaceError::ControlAttestationUnavailable("unpublished v1 receipt has no snapshot".into()).into());
                }
                let existing = std::str::from_utf8(bytes)
                    .map_err(|error| ControlError::Storage(format!("{}: {error}", receipt_path.display())))?;
                verify(existing)?;
            }
            if prior_snapshot.is_none() && !create_new(&snapshot, text.as_bytes()).map_err(storage)? {
                return Err(ControlError::Storage("unpublished v1 snapshot appeared during import".into()));
            }
            if prior_receipt.is_none() && !create_new(&receipt_path, signed.as_bytes()).map_err(storage)? {
                return Err(ControlError::Storage("unpublished v1 receipt appeared during import".into()));
            }
            replace(&self.root.join(POINTER_FILE), b"1\n").map_err(storage)?;
            return Ok(1);
        }
        let parent = match (receipt, current) {
            (Some(_), Some(version)) => {
                let path = self.root.join(receipt_file(version));
                Some(std::fs::read_to_string(&path).map_err(|e| {
                    SurfaceError::ControlAttestationUnavailable(format!("previous receipt {}: {e}", path.display()))
                })?)
            }
            _ => None,
        };
        let mut version = current.unwrap_or(0) + 1;
        loop {
            if receipt.is_some() && self.root.join(receipt_file(version)).exists() {
                version += 1;
                continue;
            }
            let signed = receipt.map(|sign| sign(version, parent.as_deref())).transpose()?;
            let snapshot = self.root.join(snapshot_file(version));
            if create_new(&snapshot, text.as_bytes()).map_err(storage)? {
                if let Some(signed) = signed {
                    let receipt_error = match create_new(&self.root.join(receipt_file(version)), signed.as_bytes()) {
                        Ok(true) => None,
                        Ok(false) => Some(ControlError::Storage(format!("receipt for v{version} already exists"))),
                        Err(e) => Some(storage(e)),
                    };
                    if let Some(error) = receipt_error {
                        std::fs::remove_file(&snapshot).map_err(|cleanup| {
                            ControlError::Storage(format!("{error}; removing unpublished snapshot {}: {cleanup}", snapshot.display()))
                        })?;
                        return Err(error);
                    }
                }
                break;
            }
            version += 1;
        }
        replace(&self.root.join(POINTER_FILE), format!("{version}\n").as_bytes()).map_err(storage)?;
        Ok(version)
    }
}
