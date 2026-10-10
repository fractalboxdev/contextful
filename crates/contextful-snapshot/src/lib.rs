//! The local snapshot directory: immutable `manifest@v<N>.toml` versions and the
//! `manifest@current` pointer naming the applied one (`surface.apply.local-claim`). It sits
//! outside the run path, so the engine and the control plane claim through one home.
//!
//! A claim holds an advisory lock across the pointer read, the exclusive create of the
//! next version file and the pointer replace, so the pointer's compare-and-swap is
//! linearizable among processes on one machine (`surface.apply.version-race`).

pub mod fs;

use crate::fs::{create_new, filesystem_kind, replace, FileLock};
use contextful_core::surface::control::{admit_conditional, parse_pointer, receipt_file, snapshot_file, POINTER_FILE};
use contextful_core::surface::SurfaceError;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use serde::{Deserialize, Serialize};

/// The lock file serializing claims.
const LOCK_FILE: &str = "manifest.lock";
const DRAFT_FILE: &str = "manifest@draft.json";
const ATTESTATION_NONCES: &str = "attestation-nonces";

/// A validated store draft bound to the applied version it read.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Draft {
    pub expected: u64,
    pub document: String,
    pub operator: String,
    pub nonce: String,
}

impl Draft {
    pub fn new(expected: u64, document: String, operator: String) -> Result<Self, ControlError> {
        let mut nonce = [0u8; 24];
        getrandom::fill(&mut nonce).map_err(|e| ControlError::Storage(e.to_string()))?;
        let nonce = nonce.iter().map(|byte| format!("{byte:02x}")).collect();
        Ok(Self { expected, document, operator, nonce })
    }
}

type ReceiptBuilder<'a> = dyn Fn(u64, Option<&str>) -> Result<String, ControlError> + 'a;
type ReceiptVerifier<'a> = dyn Fn(&str) -> Result<(), ControlError> + 'a;

/// A snapshot-directory refusal or a storage failure beneath it.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ControlError {
    #[error("ControlOperatorAttestationInvalid: {0}")]
    OperatorAttestationInvalid(String),
    #[error(transparent)]
    Authority(#[from] contextful_core::AuthorityError),
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

    /// Claim one signed control request under the same lock that serializes draft and
    /// applied-pointer mutations. A claim stays until its signed time is no longer
    /// admissible, including across a listener restart.
    pub fn claim_attestation_nonce(&self, nonce: &str, signed_at: i64, now: i64) -> Result<bool, ControlError> {
        self.claim_attestation_nonce_guarded(nonce, signed_at, now, &|| Ok(()))
    }

    /// Recheck authority under the nonce lock before changing replay state.
    pub fn claim_attestation_nonce_guarded(&self, nonce: &str, signed_at: i64, now: i64, boundary: &dyn Fn() -> Result<(), ControlError>) -> Result<bool, ControlError> {
        if nonce.len() != 32 || !nonce.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return Err(ControlError::Storage("an attestation nonce is 32 hexadecimal bytes".into()));
        }
        let storage = |e: contextful_core::run::Failure| ControlError::Storage(e.to_string());
        let _lock = FileLock::acquire(&self.root.join(LOCK_FILE)).map_err(storage)?;
        boundary()?;
        let directory = self.root.join(ATTESTATION_NONCES);
        std::fs::create_dir_all(&directory).map_err(|e| ControlError::Storage(format!("{}: {e}", directory.display())))?;
        let first_live = now.saturating_sub(60);
        for entry in std::fs::read_dir(&directory).map_err(|e| ControlError::Storage(format!("{}: {e}", directory.display())))? {
            let entry = entry.map_err(|e| ControlError::Storage(format!("{}: {e}", directory.display())))?;
            if !entry.file_type().map_err(|e| ControlError::Storage(format!("{}: {e}", entry.path().display())))?.is_dir() {
                return Err(ControlError::Storage(format!("{}: an attestation bucket is a directory", entry.path().display())));
            }
            let signed_second = entry.file_name().to_str().and_then(|name| name.parse::<i64>().ok())
                .ok_or_else(|| ControlError::Storage(format!("{}: malformed attestation bucket", entry.path().display())))?;
            if signed_second < first_live {
                std::fs::remove_dir_all(entry.path()).map_err(|e| ControlError::Storage(format!("{}: {e}", entry.path().display())))?;
            }
        }
        let marker = directory.join(signed_at.to_string()).join(nonce);
        let claimed = create_new(&marker, b"").map_err(storage)?;
        if claimed {
            std::fs::File::open(&marker).and_then(|file| file.sync_all())
                .map_err(|e| ControlError::Storage(format!("{}: {e}", marker.display())))?;
            #[cfg(unix)]
            for path in [marker.parent().expect("nonce marker has a bucket"), directory.as_path(), self.root.as_path()] {
                std::fs::File::open(path).and_then(|dir| dir.sync_all())
                    .map_err(|e| ControlError::Storage(format!("{}: {e}", path.display())))?;
            }
        }
        Ok(claimed)
    }

    /// Save one store draft only while the applied version still matches its base.
    pub fn save_draft(&self, draft: &Draft) -> Result<(), ControlError> {
        self.save_draft_guarded(draft, &|| Ok(()))
    }

    /// Recheck authority under the draft lock immediately before publication.
    pub fn save_draft_guarded(&self, draft: &Draft, boundary: &dyn Fn() -> Result<(), ControlError>) -> Result<(), ControlError> {
        let storage = |e: contextful_core::run::Failure| ControlError::Storage(e.to_string());
        let _lock = FileLock::acquire(&self.root.join(LOCK_FILE)).map_err(storage)?;
        self.expect_version(draft.expected)?;
        let body = serde_json::to_vec(draft).map_err(|e| ControlError::Storage(e.to_string()))?;
        boundary()?;
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
        self.claim_draft_guarded(draft, &|| Ok(()))
    }

    /// Recheck authority under the claim lock before an unsigned draft commit.
    pub fn claim_draft_guarded(&self, draft: &Draft, boundary: &dyn Fn() -> Result<(), ControlError>) -> Result<u64, ControlError> {
        self.claim_draft_inner(draft, None, boundary)
    }

    /// Claim a validated draft with its signed receipt before publishing the pointer.
    pub fn claim_draft_attested(&self, draft: &Draft, receipt: impl Fn(u64, Option<&str>) -> Result<String, ControlError>) -> Result<u64, ControlError> {
        self.claim_draft_attested_guarded(draft, receipt, &|| Ok(()))
    }

    /// Recheck the control boundary after signing and before publishing an attested draft.
    pub fn claim_draft_attested_guarded(&self, draft: &Draft, receipt: impl Fn(u64, Option<&str>) -> Result<String, ControlError>, boundary: &dyn Fn() -> Result<(), ControlError>) -> Result<u64, ControlError> {
        self.claim_draft_inner(draft, Some(&receipt), boundary)
    }

    fn claim_draft_inner(&self, draft: &Draft, receipt: Option<&ReceiptBuilder<'_>>, boundary: &dyn Fn() -> Result<(), ControlError>) -> Result<u64, ControlError> {
        let storage = |e: contextful_core::run::Failure| ControlError::Storage(e.to_string());
        let _lock = FileLock::acquire(&self.root.join(LOCK_FILE)).map_err(storage)?;
        self.expect_version(draft.expected)?;
        if self.read_draft()? != *draft {
            return Err(SurfaceError::ManifestVersionConflict("the validated draft changed before apply; reload and reapply".into()).into());
        }
        let version = self.claim_locked_guarded(Some(draft.expected), &draft.document, receipt, None, boundary)?;
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
        self.claim_locked(expected, text, receipt, recover)
    }

    fn claim_locked(&self, expected: Option<u64>, text: &str, receipt: Option<&ReceiptBuilder<'_>>, recover: Option<&ReceiptVerifier<'_>>) -> Result<u64, ControlError> {
        self.claim_locked_guarded(expected, text, receipt, recover, &|| Ok(()))
    }

    fn claim_locked_guarded(&self, expected: Option<u64>, text: &str, receipt: Option<&ReceiptBuilder<'_>>, recover: Option<&ReceiptVerifier<'_>>, boundary: &dyn Fn() -> Result<(), ControlError>) -> Result<u64, ControlError> {
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
            boundary()?;
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
