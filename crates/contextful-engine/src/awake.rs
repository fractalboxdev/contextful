//! The awakeable registry, persisted beside the journal so a restart drops no pending
//! callback (`run.suspend.survives-restart`).
//!
//! ```text
//! <root>/awakeables/<token>.json   one registry row
//! ```

use crate::fsutil::{read_json, replace, to_json, FileLock};
use crate::journal::{Journal, StepError};
use contextful_core::run::journal::{awakeable_key, EntryKey};
use contextful_core::run::suspend::{unknown, Awakeable, AwakeableState, Resolution};
use contextful_core::run::{Failure, RunError};
use contextful_core::time::Instant;
use std::path::{Path, PathBuf};

/// The registry of awakeables.
#[derive(Debug, Clone)]
pub struct Registry {
    root: PathBuf,
    journal: Journal,
}

/// Why a registry call refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AwakeError {
    Refused(RunError),
    Storage(Failure),
}

impl From<Failure> for AwakeError {
    fn from(f: Failure) -> AwakeError {
        AwakeError::Storage(f)
    }
}

impl From<StepError> for AwakeError {
    fn from(e: StepError) -> AwakeError {
        match e {
            StepError::Journal(r) => AwakeError::Refused(r),
            StepError::Failed(f) | StepError::Storage(f) => AwakeError::Storage(f),
        }
    }
}

/// What a waiting run reads for its awakeable.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Awaited {
    /// Still suspended.
    Pending,
    /// The resume payload, read back from the journal.
    Resumed(Vec<u8>),
    TimedOut,
}

fn is_token(token: &str) -> bool {
    !token.is_empty() && token.len() <= 128 && token.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

/// The journal key a resume payload records under.
pub fn resume_key(execution_id: &str, token: &str) -> EntryKey {
    EntryKey { execution_id: execution_id.to_string(), step_label: format!("awakeable:{token}"), input_hash: awakeable_key(token) }
}

impl Registry {
    pub fn open(root: &Path, journal: Journal) -> Registry {
        Registry { root: root.to_path_buf(), journal }
    }

    fn path(&self, token: &str) -> Option<PathBuf> {
        is_token(token).then(|| self.root.join("awakeables").join(format!("{token}.json")))
    }

    /// Mint a single-use token and persist its `pending` row.
    pub fn suspend(&self, execution_id: &str, step_label: &str, created_at: &str, ttl_secs: u64) -> Result<Awakeable, AwakeError> {
        let mut bytes = [0u8; 16];
        getrandom::fill(&mut bytes).map_err(|e| Failure::new(contextful_core::run::FailureTag::Storage, format!("minting a token: {e}")))?;
        let token: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
        let row = Awakeable::mint(&token, execution_id, step_label, created_at, ttl_secs).map_err(AwakeError::Refused)?;
        let path = self.path(&token).ok_or_else(|| AwakeError::Refused(unknown(&token)))?;
        replace(&path, &to_json(&row)?)?;
        Ok(row)
    }

    /// The row under `token`, evaluated at `now`; an unknown token refuses and
    /// allocates nothing.
    pub fn state(&self, token: &str, now: Instant) -> Result<Awakeable, AwakeError> {
        let path = self.path(token).ok_or_else(|| AwakeError::Refused(unknown(token)))?;
        let mut row: Awakeable = read_json(&path)?.ok_or_else(|| AwakeError::Refused(unknown(token)))?;
        row.evaluate(now);
        Ok(row)
    }

    /// Resolve `token` with `payload` at `now`, answering the recorded payload. The
    /// payload is recorded as the awaited step's output, a payload above the inline
    /// cutoff in the blob store, and the registry row references it.
    pub fn resolve(&self, token: &str, payload: &[u8], now: Instant) -> Result<Vec<u8>, AwakeError> {
        let path = self.path(token).ok_or_else(|| AwakeError::Refused(unknown(token)))?;
        let _lock = FileLock::acquire(&path.with_extension("lock"))?;
        let mut row: Awakeable = read_json(&path)?.ok_or_else(|| AwakeError::Refused(unknown(token)))?;
        let before = row.state;
        let outcome = row.resolve(payload, now);
        if row.state != before {
            // A pending row read past its deadline persists as `timed_out`.
            if row.state == AwakeableState::Resolved {
                self.journal.record(&resume_key(&row.execution_id, token), payload)?;
            }
            replace(&path, &to_json(&row)?)?;
        }
        match outcome.map_err(AwakeError::Refused)? {
            Resolution::First | Resolution::Recorded => {
                let stored = row.payload.as_ref().ok_or_else(|| AwakeError::Refused(unknown(token)))?;
                Ok(self.journal.load(stored)?)
            }
        }
    }

    /// What the run awaiting `token` reads: the journal first, so a run that resumed and
    /// then crashed reads its payload back without suspending again.
    pub fn awaited(&self, execution_id: &str, token: &str, now: Instant) -> Result<Awaited, AwakeError> {
        if let Some(contextful_core::run::journal::Row::Recorded { value, .. }) = self.journal.row(&resume_key(execution_id, token))? {
            return Ok(Awaited::Resumed(self.journal.load(&value)?));
        }
        Ok(match self.state(token, now)?.state {
            AwakeableState::TimedOut => Awaited::TimedOut,
            _ => Awaited::Pending,
        })
    }

    /// Blobs referenced by pending or resolved rows, marked by the journal's sweep.
    pub fn referenced_blobs(&self) -> Result<Vec<String>, Failure> {
        let dir = self.root.join("awakeables");
        let entries = match std::fs::read_dir(&dir) {
            Ok(e) => e,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(crate::fsutil::storage(&dir, e)),
        };
        let mut refs = Vec::new();
        for entry in entries {
            let path = entry.map_err(|e| crate::fsutil::storage(&dir, e))?.path();
            if path.extension().is_some_and(|x| x == "json") {
                if let Some(row) = read_json::<Awakeable>(&path)? {
                    refs.extend(row.payload.as_ref().and_then(|p| p.blob()).map(str::to_string));
                }
            }
        }
        Ok(refs)
    }
}
