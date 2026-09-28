//! The ports the runner drives: the source a pull reaches, the destination a run lands
//! through, the cancellation every await observes, and the journal, blob and awakeable
//! stores replay state persists through.

use super::failure::{Failure, FailureTag};
use super::journal::{EntryKey, Row as JournalRow, Stored};
use super::suspend::Awakeable;
use crate::time::Instant;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

/// A row as a source hands it over.
pub type Row = Map<String, Value>;

/// What one pull asks the source for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PullRequest {
    pub step_label: String,
    /// The position the pull reads from; `None` reads from the source's start.
    pub position: Option<Value>,
    /// Derived from the journal entry key; identical on every re-entry of this pull.
    pub idempotency_key: String,
}

/// One pull, decoded from the bytes the source handed over.
#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Pull {
    #[serde(default)]
    pub rows: Vec<Row>,
    /// The source's continuation position after this pull, for a token or snapshot cursor.
    #[serde(default)]
    pub cursor: Option<Value>,
    /// Whether another pull follows in this run.
    #[serde(default)]
    pub more: bool,
}

impl Pull {
    /// Decode a pull. Bytes outside the shape are a deterministic `SchemaIncompatible`.
    pub fn decode(bytes: &[u8]) -> Result<Pull, Failure> {
        serde_json::from_slice(bytes).map_err(|e| Failure::deterministic(FailureTag::SchemaIncompatible, format!("a pull is `{{\"rows\", \"cursor\", \"more\"}}`: {e}")))
    }
}

/// Whether a stop has reached this run. Every await in the runner selects on it.
pub trait Cancellation {
    fn requested(&self) -> bool;
}

/// A source serving pulls. Its failure returns to the step, which owns the only retry
/// layer (`run.retry.one-layer`).
pub trait Source {
    /// The bytes one pull hands over, as the source handed them.
    fn pull(&mut self, request: &PullRequest, cancel: &dyn Cancellation) -> Result<Vec<u8>, Failure>;
}

impl<S: Source + ?Sized> Source for Box<S> {
    fn pull(&mut self, request: &PullRequest, cancel: &dyn Cancellation) -> Result<Vec<u8>, Failure> {
        (**self).pull(request, cancel)
    }
}

/// One run's commit to one table: every batch in pull order, and the position the rows
/// behind it reach.
#[derive(Debug, Clone, PartialEq)]
pub struct Commit {
    pub pipeline_id: String,
    pub table: String,
    pub run_id: String,
    pub site_id: String,
    pub batches: Vec<Vec<Row>>,
    pub cursor: Option<Value>,
    pub committed_at: Instant,
    /// The fence of the single-writer lease the commit runs under.
    pub fence: Option<u64>,
}

/// What a commit landed, measured at the destination.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Landed {
    pub rows: u64,
    pub bytes: u64,
}

/// The newest run commit marker the destination holds for a pipeline's table.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Marker {
    pub run_id: String,
    pub cursor: Option<Value>,
    pub committed_at: Instant,
}

/// The land path a run commits through.
pub trait Destination {
    /// Land every batch as one atomic commit carrying `commit.cursor`. `precommit` runs
    /// immediately before the commit point; a refusal there lands nothing.
    fn land(&mut self, commit: Commit, precommit: &dyn Fn() -> Result<(), Failure>) -> Result<Landed, Failure>;
    /// Record that a single-writer lease on `table` was taken under `fence`, so a commit
    /// carrying a lower fence loses its condition at the store.
    fn open_fence(&mut self, _pipeline_id: &str, _table: &str, _fence: u64) -> Result<(), Failure> {
        Ok(())
    }
    /// The newest commit marker `pipeline_id` wrote to `table`.
    fn newest_marker(&self, pipeline_id: &str, table: &str) -> Result<Option<Marker>, Failure>;
}

/// The stages a recorded batch passes between the journal and the land path: normalize,
/// the transform chain and write-path redaction (`run.land.stage-order`).
pub trait Shape {
    fn shape(&self, rows: Vec<Row>) -> Result<Vec<Row>, super::RunError>;
}

/// The shape that passes a batch through unchanged.
pub struct Unshaped;

impl Shape for Unshaped {
    fn shape(&self, rows: Vec<Row>) -> Result<Vec<Row>, super::RunError> {
        Ok(rows)
    }
}

/// Read access to landed tables for a source that derives from the store: every row of a
/// table's current file list. An unknown table reads as no rows.
pub trait TableReader {
    /// Every row of `table`, holding the named columns it carries.
    fn rows(&self, table: &str, columns: &[&str]) -> Result<Vec<Row>, Failure>;
}

/// Where journal rows persist (`run.journal.storage-ports`). Each call is atomic against
/// every other call on the same key, across threads and across processes sharing the
/// store. The claim, takeover and record rules live in the engine's journal, which reaches
/// storage only through this port.
pub trait JournalStore: Send + Sync {
    /// Write a pending row for `key` held by `run_id` only where no row exists; `false`
    /// when one does.
    fn create_pending(&self, key: &EntryKey, run_id: &str) -> Result<bool, Failure>;
    /// The row under `key`, if any.
    fn read(&self, key: &EntryKey) -> Result<Option<JournalRow>, Failure>;
    /// Pass `holder`'s pending claim on `key` to `run_id`; `false`, writing nothing, when
    /// the row is no longer `holder`'s pending claim.
    fn replace_if_pending(&self, key: &EntryKey, holder: &str, run_id: &str) -> Result<bool, Failure>;
    /// Record `value` under `key` unless a recorded row stands, whoever holds the claim.
    /// `None` when this call recorded; the standing value when an earlier record did.
    fn record(&self, key: &EntryKey, value: &Stored) -> Result<Option<Stored>, Failure>;
    /// Drop `run_id`'s pending claim on `key`, leaving a recorded row or another run's
    /// claim alone.
    fn release(&self, key: &EntryKey, run_id: &str) -> Result<(), Failure>;
    /// Every row under `execution_id`, in no particular order.
    fn rows(&self, execution_id: &str) -> Result<Vec<JournalRow>, Failure>;
    /// Delete every row under `execution_id` (`run.journal.collection`); an execution
    /// holding no row retires as a no-op.
    fn retire(&self, execution_id: &str) -> Result<(), Failure>;
    /// Every execution holding at least one row.
    fn executions(&self) -> Result<Vec<String>, Failure>;
}

/// Where values above the inline cutoff persist, content-addressed by sha256
/// (`run.journal.storage-ports`).
pub trait BlobStore: Send + Sync {
    /// Store `bytes` under `sha256`. Concurrent puts of one hash converge on one stored
    /// value without waiting or erroring, and no partial value is ever readable
    /// (`run.journal.blob-write`).
    fn put(&self, sha256: &str, bytes: &[u8]) -> Result<(), Failure>;
    /// The bytes under `sha256`; `None` when no blob holds it.
    fn get(&self, sha256: &str) -> Result<Option<Vec<u8>>, Failure>;
    /// Delete each blob outside `referenced` that [`sweepable`](super::journal::sweepable) admits at its age at
    /// `now_unix`, the age counted from its last put. Returns the deleted hashes.
    fn sweep(&self, referenced: &[String], now_unix: i64) -> Result<Vec<String>, Failure>;
    /// The instant, in Unix seconds, the last sweep pass ran; `None` before the first.
    fn swept_at(&self) -> Result<Option<i64>, Failure>;
    /// Persist `at` as the instant the last sweep pass ran.
    fn mark_swept(&self, at: i64) -> Result<(), Failure>;
}

/// Where awakeable registry rows persist (`run.journal.storage-ports`).
pub trait AwakeableStore: Send + Sync {
    /// Persist a freshly minted row under its token.
    fn insert(&self, row: &Awakeable) -> Result<(), Failure>;
    /// The row under `token`, if any.
    fn get(&self, token: &str) -> Result<Option<Awakeable>, Failure>;
    /// Apply `edit` to the row under `token`, exclusive of every other `update` of that
    /// token, persisting the edited row when `edit` answers `true`. Returns the row as it
    /// stands afterwards; `None`, allocating nothing, when no row holds the token
    /// (`run.suspend.unknown-token`).
    fn update(&self, token: &str, edit: &mut dyn FnMut(&mut Awakeable) -> Result<bool, Failure>) -> Result<Option<Awakeable>, Failure>;
    /// Every row, in no particular order.
    fn rows(&self) -> Result<Vec<Awakeable>, Failure>;
}
