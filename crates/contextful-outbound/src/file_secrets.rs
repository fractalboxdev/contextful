//! The `file` adapter: one file per logical name under an operator directory, versioned by
//! the SHA-256 of its bytes, and the one linked adapter with a write half
//! (`connector.resolve.file-backend`, `connector.rotate.file-put`).

use contextful_core::connector::record::{Expiry, Observation, Observe, Unavailable};
use contextful_core::connector::reference::{Hydrated, SecretName};
use contextful_core::connector::resolve::{Answer, Provider};
use contextful_core::connector::rotate::{PutError, Version, VersionedStore};
use contextful_core::run::journal::sha256_hex;
use contextful_core::run::{Failure, FailureTag};
use contextful_core::time::Instant;
use std::collections::BTreeMap;
use std::io::{ErrorKind, Write};
use std::path::PathBuf;

/// The adapter's name, as `CONTEXTFUL_SECRETS_BACKEND` names it.
pub const NAME: &str = "file";
/// The directory holding one file per logical name.
pub const SECRETS_DIR: &str = "CONTEXTFUL_SECRETS_DIR";
/// The lock file a put holds while it compares and replaces; no logical name contains `.`.
const LOCK: &str = ".lock";

/// One file per logical name under one directory.
pub struct FileSecrets {
    dir: PathBuf,
}

/// The file's value, its trailing newline removed, and the version its bytes carry.
struct Stored {
    value: String,
    version: Version,
    read_only: bool,
}

fn storage(e: std::io::Error, what: &str) -> Failure {
    Failure::new(FailureTag::Storage, format!("{what}: {}", e.kind()))
}

/// Whether an I/O failure is the store refusing the write rather than failing it.
fn refusal(kind: ErrorKind) -> bool {
    matches!(kind, ErrorKind::PermissionDenied | ErrorKind::ReadOnlyFilesystem)
}

impl FileSecrets {
    pub fn new(dir: impl Into<PathBuf>) -> FileSecrets {
        FileSecrets { dir: dir.into() }
    }

    /// The adapter over `CONTEXTFUL_SECRETS_DIR`; selecting it with the variable unset refuses.
    pub fn from_vars(vars: &BTreeMap<String, String>) -> Result<FileSecrets, Failure> {
        match vars.get(SECRETS_DIR).filter(|d| !d.is_empty()) {
            Some(dir) => Ok(FileSecrets::new(dir)),
            None => Err(Failure::deterministic(FailureTag::Config, format!("the `{NAME}` backend is selected and `{SECRETS_DIR}` is unset"))),
        }
    }

    fn path(&self, name: &SecretName) -> PathBuf {
        self.dir.join(name.as_str())
    }

    fn read(&self, name: &SecretName) -> std::io::Result<Option<Stored>> {
        let path = self.path(name);
        let bytes = match std::fs::read(&path) {
            Ok(b) => b,
            Err(e) if e.kind() == ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(e),
        };
        let read_only = std::fs::metadata(&path)?.permissions().readonly();
        let version = sha256_hex(&bytes);
        let mut value = String::from_utf8(bytes).map_err(|_| std::io::Error::new(ErrorKind::InvalidData, "the file is not UTF-8"))?;
        if value.ends_with('\n') {
            value.pop();
        }
        Ok(Some(Stored { value, version, read_only }))
    }

    /// Write `value` beside the target with owner-only permissions, then rename it over the
    /// target, so a reader sees the old bytes or the new ones whole.
    fn replace(&self, name: &SecretName, value: &Hydrated) -> std::io::Result<Version> {
        let bytes = value.reveal().as_bytes();
        let temporary = self.dir.join(format!(".{name}.{}.tmp", std::process::id()));
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create(true).truncate(true);
        #[cfg(unix)]
        std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
        let written = (|| {
            let mut file = options.open(&temporary)?;
            file.write_all(bytes)?;
            file.sync_all()?;
            std::fs::rename(&temporary, self.path(name))
        })();
        if written.is_err() {
            let _ = std::fs::remove_file(&temporary);
        }
        written.map(|()| sha256_hex(bytes))
    }
}

impl Provider for FileSecrets {
    fn name(&self) -> &str {
        NAME
    }

    fn answer(&self, name: &SecretName) -> Result<Option<Answer>, Failure> {
        let stored = self.read(name).map_err(|e| storage(e, &format!("reading `secret://{name}` from the `{NAME}` backend")))?;
        Ok(stored.map(|s| Answer { value: Hydrated::new(s.value), expires_at: None, version: Some(s.version) }))
    }
}

/// Presence and version from the file, with no value built.
impl Observe for FileSecrets {
    fn adapter(&self) -> &str {
        NAME
    }

    fn observe(&self, name: &SecretName, at: Instant) -> Observation {
        match self.read(name) {
            Ok(stored) => {
                let version = stored.as_ref().map(|s| s.version.clone());
                Observation::Observed { adapter: NAME.into(), at, present: stored.is_some(), version, created: None, expires: Expiry::Unknown }
            }
            Err(e) => Observation::Unavailable { adapter: NAME.into(), at, reason: Unavailable::Failed(format!("{}", e.kind())) },
        }
    }
}

/// The conditional put: under an exclusive lock, the file's current digest must equal the
/// version the run hydrated, and a file without write permission refuses the put. The
/// expiry hint has no room in a plain file, so the adapter drops it.
impl VersionedStore for FileSecrets {
    fn adapter(&self) -> &str {
        NAME
    }

    fn put(&self, name: &SecretName, value: &Hydrated, if_version: Option<&str>, _expires_hint: Option<Instant>) -> Result<Version, PutError> {
        let failed = |e: std::io::Error| if refusal(e.kind()) { PutError::Denied(format!("{}", e.kind())) } else { PutError::Failed(format!("{}", e.kind())) };
        let lock = std::fs::OpenOptions::new().write(true).create(true).truncate(false).open(self.dir.join(LOCK)).map_err(failed)?;
        lock.lock().map_err(failed)?;
        let current = self.read(name).map_err(failed)?;
        if current.as_ref().is_some_and(|s| s.read_only) {
            return Err(PutError::Denied(format!("`secret://{name}` is read-only")));
        }
        let version = current.map(|s| s.version);
        if version.as_deref() != if_version {
            return Err(PutError::Conflict(version.unwrap_or_default()));
        }
        self.replace(name, value).map_err(failed)
    }
}
