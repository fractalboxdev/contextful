//! The awakeable registry over an [`AwakeableStore`], its resume payloads recorded
//! through the journal, so a restart drops no pending callback
//! (`run.suspend.survives-restart`). The file tree of [`crate::stores::file`] is the
//! default adapter.

use crate::journal::{Journal, StepError};
use crate::stores::{is_token, FileAwakeableStore, FileBlobStore, FileJournalStore};
use contextful_core::run::journal::{awakeable_key, EntryKey, Row};
use contextful_core::run::ports::{AwakeableStore, BlobStore, JournalStore};
use contextful_core::run::suspend::{unknown, Awakeable, AwakeableState, Resolution};
use contextful_core::run::{Failure, RunError};
use contextful_core::time::Instant;
use std::path::Path;

/// The registry of awakeables.
#[derive(Debug, Clone)]
pub struct Registry<A = FileAwakeableStore, J = FileJournalStore, B = FileBlobStore> {
    store: A,
    journal: Journal<J, B>,
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

/// The journal key a resume payload records under.
pub fn resume_key(execution_id: &str, token: &str) -> EntryKey {
    EntryKey { execution_id: execution_id.to_string(), step_label: format!("awakeable:{token}"), input_hash: awakeable_key(token) }
}

impl Registry {
    /// The registry over the file tree rooted at `root`.
    pub fn open(root: &Path, journal: Journal) -> Registry {
        Registry::over(FileAwakeableStore::open(root), journal)
    }
}

impl<A: AwakeableStore, J: JournalStore, B: BlobStore> Registry<A, J, B> {
    /// The registry over `store`, recording resume payloads through `journal`.
    pub fn over(store: A, journal: Journal<J, B>) -> Registry<A, J, B> {
        Registry { store, journal }
    }

    /// Mint a single-use token and persist its `pending` row.
    pub fn suspend(&self, execution_id: &str, step_label: &str, created_at: &str, ttl_secs: u64) -> Result<Awakeable, AwakeError> {
        let mut bytes = [0u8; 16];
        getrandom::fill(&mut bytes).map_err(|e| Failure::new(contextful_core::run::FailureTag::Storage, format!("minting a token: {e}")))?;
        let token: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
        let row = Awakeable::mint(&token, execution_id, step_label, created_at, ttl_secs).map_err(AwakeError::Refused)?;
        self.store.insert(&row)?;
        Ok(row)
    }

    /// The row under `token`, evaluated at `now`; an unknown token refuses and
    /// allocates nothing.
    pub fn state(&self, token: &str, now: Instant) -> Result<Awakeable, AwakeError> {
        let found = if is_token(token) { self.store.get(token)? } else { None };
        let mut row = found.ok_or_else(|| AwakeError::Refused(unknown(token)))?;
        row.evaluate(now);
        Ok(row)
    }

    /// Resolve `token` with `payload` at `now`, answering the recorded payload. The
    /// payload is recorded as the awaited step's output, a payload above the inline
    /// cutoff in the blob store, and the registry row references it.
    pub fn resolve(&self, token: &str, payload: &[u8], now: Instant) -> Result<Vec<u8>, AwakeError> {
        if !is_token(token) {
            return Err(AwakeError::Refused(unknown(token)));
        }
        let mut outcome = None;
        let mut refused = None;
        let row = self.store.update(token, &mut |row| {
            let before = row.state;
            outcome = Some(row.resolve(payload, now));
            if row.state == before {
                return Ok(false);
            }
            // A pending row read past its deadline persists as `timed_out`.
            if row.state == AwakeableState::Resolved {
                match self.journal.record(&resume_key(&row.execution_id, token), payload) {
                    Ok(_) => {}
                    Err(StepError::Journal(r)) => {
                        refused = Some(r);
                        return Ok(false);
                    }
                    Err(StepError::Failed(f) | StepError::Storage(f)) => return Err(f),
                }
            }
            Ok(true)
        })?;
        if let Some(r) = refused {
            return Err(AwakeError::Refused(r));
        }
        let (Some(row), Some(outcome)) = (row, outcome) else { return Err(AwakeError::Refused(unknown(token))) };
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
        if let Some(Row::Recorded { value, .. }) = self.journal.row(&resume_key(execution_id, token))? {
            return Ok(Awaited::Resumed(self.journal.load(&value)?));
        }
        Ok(match self.state(token, now)?.state {
            AwakeableState::TimedOut => Awaited::TimedOut,
            _ => Awaited::Pending,
        })
    }

    /// Blobs referenced by pending or resolved rows, marked by the journal's sweep.
    pub fn referenced_blobs(&self) -> Result<Vec<String>, Failure> {
        Ok(self.store.rows()?.iter().filter_map(|row| row.payload.as_ref().and_then(|p| p.blob()).map(str::to_string)).collect())
    }
}
