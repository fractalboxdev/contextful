//! The journal: the claim, takeover and record rules over a [`JournalStore`], and value
//! placement over a [`BlobStore`] (`run.journal.storage-ports`). The file tree of
//! [`crate::stores::file`] is the default adapter pair.

use crate::fsutil::sleep_unless;
use crate::stores::{FileBlobStore, FileJournalStore};
use contextful_core::run::journal::{sweep_due, EntryKey, Row, Stored};
use contextful_core::run::ports::{BlobStore, Cancellation, JournalStore};
use contextful_core::run::{Failure, RunError};
use std::path::{Path, PathBuf};
use std::time::Duration;

/// Interval at which a caller waiting on another's claim re-reads it.
const CLAIM_POLL: Duration = Duration::from_millis(20);

/// What a step resolved to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Resolved {
    /// The journal held the value; the effect did not run.
    Replayed(Vec<u8>),
    /// The effect ran and its value is now recorded.
    Recorded(Vec<u8>),
    /// The effect ran and its value was not journaled.
    Unrecorded(Vec<u8>),
}

impl Resolved {
    pub fn bytes(&self) -> &[u8] {
        match self {
            Resolved::Replayed(b) | Resolved::Recorded(b) | Resolved::Unrecorded(b) => b,
        }
    }
}

/// Why a step produced no value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StepError {
    /// The effect failed; its claim is released and nothing is recorded.
    Failed(Failure),
    /// The journal could not produce the recorded value.
    Journal(RunError),
    /// The journal's storage failed.
    Storage(Failure),
}

impl From<Failure> for StepError {
    fn from(f: Failure) -> StepError {
        StepError::Storage(f)
    }
}

/// A journal over a row store and a blob store.
#[derive(Debug, Clone)]
pub struct Journal<J = FileJournalStore, B = FileBlobStore> {
    rows: J,
    blobs: B,
}

impl Journal {
    /// The journal over the file tree rooted at `root`.
    pub fn open(root: &Path) -> Journal {
        Journal::over(FileJournalStore::open(root), FileBlobStore::open(root))
    }

    /// The directory holding every blob.
    pub fn blob_dir(&self) -> PathBuf {
        self.blobs.dir()
    }

    /// The file a blob lives in.
    pub fn blob_path(&self, sha256: &str) -> PathBuf {
        self.blobs.path(sha256)
    }
}

impl<J: JournalStore, B: BlobStore> Journal<J, B> {
    /// The journal over `rows` and `blobs`.
    pub fn over(rows: J, blobs: B) -> Journal<J, B> {
        Journal { rows, blobs }
    }

    /// The row store.
    pub fn row_store(&self) -> &J {
        &self.rows
    }

    /// The blob store.
    pub fn blob_store(&self) -> &B {
        &self.blobs
    }

    /// Write a blob. Concurrent writers of one hash converge on one stored value without
    /// waiting or erroring (`run.journal.blob-write`).
    pub fn write_blob(&self, sha256: &str, bytes: &[u8]) -> Result<(), Failure> {
        self.blobs.put(sha256, bytes)
    }

    /// Place `value` for a row: inline, or a blob written ahead of the row naming it.
    pub fn store(&self, value: &[u8]) -> Result<Stored, Failure> {
        let stored = Stored::place(value);
        if let Some(sha) = stored.blob() {
            self.write_blob(sha, value)?;
        }
        Ok(stored)
    }

    /// The bytes a stored value holds. A blob reference resolving to no stored blob
    /// refuses with `BlobMissing`, never an empty value in the recorded one's place.
    pub fn load(&self, stored: &Stored) -> Result<Vec<u8>, StepError> {
        if let Some(bytes) = stored.inline_bytes() {
            return Ok(bytes);
        }
        let sha = stored.blob().unwrap_or_default();
        self.blobs.get(sha)?.ok_or_else(|| StepError::Journal(RunError::BlobMissing(format!("blob `{sha}` names nothing in the journal's blob store"))))
    }

    /// The row under `key`, if any.
    pub fn row(&self, key: &EntryKey) -> Result<Option<Row>, Failure> {
        self.rows.read(key)
    }

    /// Record `value` under `key` outside a claim: the first value stands.
    pub fn record(&self, key: &EntryKey, value: &[u8]) -> Result<Vec<u8>, StepError> {
        if let Some(Row::Recorded { value: stored, .. }) = self.rows.read(key)? {
            return self.load(&stored);
        }
        let stored = self.store(value)?;
        match self.rows.record(key, &stored)? {
            Some(standing) => self.load(&standing),
            None => Ok(value.to_vec()),
        }
    }

    /// Resolve one journaled step.
    ///
    /// A recorded value returns without running `effect`. Otherwise the caller claims the
    /// key with a pending row before entering the effect; a racing caller waits for the
    /// recorded value instead of computing, and takes the claim over once `holder_live`
    /// reads its holder dead. The effect's value is recorded exactly once when
    /// `journal_it` admits it; a failed effect releases its claim.
    pub fn step(
        &self,
        key: &EntryKey,
        run_id: &str,
        holder_live: &dyn Fn(&str) -> Result<bool, Failure>,
        cancel: &dyn Cancellation,
        journal_it: &dyn Fn(&[u8]) -> bool,
        effect: &mut dyn FnMut() -> Result<Vec<u8>, Failure>,
    ) -> Result<Resolved, StepError> {
        loop {
            if self.rows.create_pending(key, run_id)? {
                break;
            }
            match self.rows.read(key)? {
                None => continue,
                Some(Row::Recorded { value, .. }) => return self.load(&value).map(Resolved::Replayed),
                Some(Row::Pending { run_id: holder, .. }) if holder == run_id => break,
                Some(Row::Pending { run_id: holder, .. }) => {
                    if !holder_live(&holder)? {
                        if self.rows.replace_if_pending(key, &holder, run_id)? {
                            break;
                        }
                        // The holder recorded, or another caller took over, since the read.
                        continue;
                    }
                    if !sleep_unless(CLAIM_POLL, &|| cancel.requested()) {
                        return Err(StepError::Failed(Failure::canceled(format!("stopped waiting on step `{}`", key.step_label))));
                    }
                }
            }
        }

        let value = match effect() {
            Ok(v) => v,
            Err(f) => {
                self.rows.release(key, run_id)?;
                return Err(StepError::Failed(f));
            }
        };
        if !journal_it(&value) {
            self.rows.release(key, run_id)?;
            return Ok(Resolved::Unrecorded(value));
        }
        // A caller that took the claim over and recorded first: its value stands.
        if let Some(Row::Recorded { value: standing, .. }) = self.rows.read(key)? {
            return self.load(&standing).map(Resolved::Replayed);
        }
        let stored = self.store(&value)?;
        match self.rows.record(key, &stored)? {
            Some(standing) => self.load(&standing).map(Resolved::Replayed),
            None => Ok(Resolved::Recorded(value)),
        }
    }

    /// Recorded entries under an execution.
    pub fn recorded(&self, execution_id: &str) -> Result<usize, Failure> {
        Ok(self.rows.rows(execution_id)?.iter().filter(|r| matches!(r, Row::Recorded { .. })).count())
    }

    /// Delete every row of a retired execution (`run.journal.collection`).
    pub fn collect(&self, execution_id: &str) -> Result<(), Failure> {
        self.rows.retire(execution_id)
    }

    /// Every blob a journal row references.
    pub fn referenced_blobs(&self) -> Result<Vec<String>, Failure> {
        let mut refs = Vec::new();
        for execution_id in self.rows.executions()? {
            for row in self.rows.rows(&execution_id)? {
                if let Row::Recorded { value, .. } = row {
                    refs.extend(value.blob().map(str::to_string));
                }
            }
        }
        Ok(refs)
    }

    /// One mark-and-sweep pass: delete each blob that no journal row and no entry of
    /// `also_referenced` names, once it is at least the grace window old at `now_unix`.
    /// Returns the deleted hashes.
    pub fn sweep(&self, also_referenced: &[String], now_unix: i64) -> Result<Vec<String>, Failure> {
        let mut marked = self.referenced_blobs()?;
        marked.extend_from_slice(also_referenced);
        self.blobs.sweep(&marked, now_unix)
    }

    /// Run a sweep when the previous one ran at least 24 h before `now_unix`, recording
    /// this pass's instant. `None` when no pass was due.
    pub fn sweep_if_due(&self, also_referenced: &[String], now_unix: i64) -> Result<Option<Vec<String>>, Failure> {
        let last = self.blobs.swept_at()?;
        if !sweep_due(last.map(|l| u64::try_from(now_unix - l).unwrap_or(0))) {
            return Ok(None);
        }
        let deleted = self.sweep(also_referenced, now_unix)?;
        self.blobs.mark_swept(now_unix)?;
        Ok(Some(deleted))
    }
}
