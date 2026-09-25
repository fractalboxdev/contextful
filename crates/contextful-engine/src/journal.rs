//! The journal and its blob store: machine-local files beside the catalog.
//!
//! ```text
//! <root>/journal/<execution_id>/<entry digest>.json   a pending claim or a recorded value
//! <root>/blobs/<sha256>                               a value above the inline cutoff
//! ```

use crate::fsutil::{self, create_new, read_json, replace, sleep_unless, storage, to_json, FileLock};
use contextful_core::run::journal::{sweep_due, sweepable, EntryKey, Row, Stored};
use contextful_core::run::ports::Cancellation;
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

/// A journal rooted in the engine's machine-local directory.
#[derive(Debug, Clone)]
pub struct Journal {
    root: PathBuf,
}

impl Journal {
    pub fn open(root: &Path) -> Journal {
        Journal { root: root.to_path_buf() }
    }

    fn execution_dir(&self, execution_id: &str) -> PathBuf {
        self.root.join("journal").join(execution_id)
    }

    fn row_path(&self, key: &EntryKey) -> PathBuf {
        self.execution_dir(&key.execution_id).join(format!("{}.json", key.digest()))
    }

    pub fn blob_dir(&self) -> PathBuf {
        self.root.join("blobs")
    }

    pub fn blob_path(&self, sha256: &str) -> PathBuf {
        self.blob_dir().join(sha256)
    }

    /// Write a blob: stage a private temporary file and rename it over the destination,
    /// so concurrent writers of one hash converge on one file without waiting or erroring.
    pub fn write_blob(&self, sha256: &str, bytes: &[u8]) -> Result<(), Failure> {
        replace(&self.blob_path(sha256), bytes)
    }

    /// Place `value` for a row: inline, or a blob written ahead of the row naming it.
    pub fn store(&self, value: &[u8]) -> Result<Stored, Failure> {
        let stored = Stored::place(value);
        if let Some(sha) = stored.blob() {
            self.write_blob(sha, value)?;
        }
        Ok(stored)
    }

    /// The bytes a stored value holds. A blob reference resolving to no file refuses
    /// with `BlobMissing`, never an empty value in the recorded one's place.
    pub fn load(&self, stored: &Stored) -> Result<Vec<u8>, StepError> {
        if let Some(bytes) = stored.inline_bytes() {
            return Ok(bytes);
        }
        let sha = stored.blob().unwrap_or_default();
        let path = self.blob_path(sha);
        match std::fs::read(&path) {
            Ok(bytes) => Ok(bytes),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                Err(StepError::Journal(RunError::BlobMissing(format!("blob `{sha}` names no file under the journal's blob store"))))
            }
            Err(e) => Err(StepError::Storage(storage(&path, e))),
        }
    }

    /// The row under `key`, if any.
    pub fn row(&self, key: &EntryKey) -> Result<Option<Row>, Failure> {
        read_json(&self.row_path(key))
    }

    /// Record `value` under `key` outside a claim: the first value stands.
    pub fn record(&self, key: &EntryKey, value: &[u8]) -> Result<Vec<u8>, StepError> {
        let path = self.row_path(key);
        let _lock = FileLock::acquire(&path.with_extension("lock"))?;
        if let Some(Row::Recorded { value: stored, .. }) = read_json::<Row>(&path)? {
            return self.load(&stored);
        }
        let stored = self.store(value)?;
        replace(&path, &to_json(&Row::Recorded { key: key.clone(), value: stored })?)?;
        Ok(value.to_vec())
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
        let path = self.row_path(key);
        let pending = to_json(&Row::Pending { key: key.clone(), run_id: run_id.to_string() })?;
        loop {
            if create_new(&path, &pending)? {
                break;
            }
            match read_json::<Row>(&path)? {
                None => continue,
                Some(Row::Recorded { value, .. }) => return self.load(&value).map(Resolved::Replayed),
                Some(Row::Pending { run_id: holder, .. }) if holder == run_id => break,
                Some(Row::Pending { run_id: holder, .. }) => {
                    if !holder_live(&holder)? {
                        let _lock = FileLock::acquire(&path.with_extension("lock"))?;
                        // The holder may have recorded, or another caller taken over, since the read.
                        match read_json::<Row>(&path)? {
                            Some(Row::Pending { run_id: h, .. }) if h == holder => {
                                replace(&path, &pending)?;
                                break;
                            }
                            _ => continue,
                        }
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
                self.release(key, run_id)?;
                return Err(StepError::Failed(f));
            }
        };
        if !journal_it(&value) {
            self.release(key, run_id)?;
            return Ok(Resolved::Unrecorded(value));
        }
        let _lock = FileLock::acquire(&path.with_extension("lock"))?;
        match read_json::<Row>(&path)? {
            // A caller that took the claim over recorded first: its value stands.
            Some(Row::Recorded { value: stored, .. }) => self.load(&stored).map(Resolved::Replayed),
            _ => {
                let stored = self.store(&value)?;
                replace(&path, &to_json(&Row::Recorded { key: key.clone(), value: stored })?)?;
                Ok(Resolved::Recorded(value))
            }
        }
    }

    /// Drop `run_id`'s pending claim on `key`, leaving a recorded row alone.
    fn release(&self, key: &EntryKey, run_id: &str) -> Result<(), Failure> {
        let path = self.row_path(key);
        let _lock = FileLock::acquire(&path.with_extension("lock"))?;
        if let Some(Row::Pending { run_id: h, .. }) = read_json::<Row>(&path)? {
            if h == run_id {
                std::fs::remove_file(&path).map_err(|e| storage(&path, e))?;
            }
        }
        let _ = std::fs::remove_file(path.with_extension("lock"));
        Ok(())
    }

    /// Recorded entries under an execution.
    pub fn recorded(&self, execution_id: &str) -> Result<usize, Failure> {
        Ok(self.rows(execution_id)?.iter().filter(|r| matches!(r, Row::Recorded { .. })).count())
    }

    fn rows(&self, execution_id: &str) -> Result<Vec<Row>, Failure> {
        let dir = self.execution_dir(execution_id);
        let entries = match std::fs::read_dir(&dir) {
            Ok(e) => e,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(storage(&dir, e)),
        };
        let mut rows = Vec::new();
        for entry in entries {
            let path = entry.map_err(|e| storage(&dir, e))?.path();
            let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
            if name.ends_with(".json") && !name.starts_with('.') {
                if let Some(row) = read_json(&path)? {
                    rows.push(row);
                }
            }
        }
        Ok(rows)
    }

    /// Delete every row of a retired execution (`run.journal.collection`).
    pub fn collect(&self, execution_id: &str) -> Result<(), Failure> {
        let dir = self.execution_dir(execution_id);
        match std::fs::remove_dir_all(&dir) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(storage(&dir, e)),
        }
    }

    /// Every blob a journal row references.
    pub fn referenced_blobs(&self) -> Result<Vec<String>, Failure> {
        let dir = self.root.join("journal");
        let executions = match std::fs::read_dir(&dir) {
            Ok(e) => e,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(storage(&dir, e)),
        };
        let mut refs = Vec::new();
        for entry in executions {
            let entry = entry.map_err(|e| storage(&dir, e))?;
            for row in self.rows(&entry.file_name().to_string_lossy())? {
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
        let dir = self.blob_dir();
        let blobs = match std::fs::read_dir(&dir) {
            Ok(e) => e,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(storage(&dir, e)),
        };
        let mut deleted = Vec::new();
        for entry in blobs {
            let entry = entry.map_err(|e| storage(&dir, e))?;
            let name = entry.file_name().to_string_lossy().into_owned();
            if name.starts_with('.') {
                continue;
            }
            let path = entry.path();
            let modified = entry
                .metadata()
                .and_then(|m| m.modified())
                .map_err(|e| storage(&path, e))?
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs() as i64)
                .unwrap_or(0);
            let age = u64::try_from(now_unix - modified).unwrap_or(0);
            if sweepable(marked.contains(&name), age) {
                fsutil::remove_file(&path)?;
                deleted.push(name);
            }
        }
        Ok(deleted)
    }

    /// Run a sweep when the previous one ran at least 24 h before `now_unix`, recording
    /// this pass's instant. `None` when no pass was due.
    pub fn sweep_if_due(&self, also_referenced: &[String], now_unix: i64) -> Result<Option<Vec<String>>, Failure> {
        let stamp = self.blob_dir().join(".swept");
        let last: Option<i64> = read_json(&stamp)?;
        if !sweep_due(last.map(|l| u64::try_from(now_unix - l).unwrap_or(0))) {
            return Ok(None);
        }
        let deleted = self.sweep(also_referenced, now_unix)?;
        replace(&stamp, &to_json(&now_unix)?)?;
        Ok(Some(deleted))
    }
}
