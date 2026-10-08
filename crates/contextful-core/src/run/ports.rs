//! The ports the runner drives: the source a pull reaches, the destination a run lands
//! through, the cancellation every await observes, the journal, blob and awakeable stores
//! replay state persists through, and the substrate port a host opens executions on.

use super::failure::{Failure, FailureTag};
use super::journal::{EntryKey, Row as JournalRow, Stored};
use super::own::{ConnectorPin, OwnerPins, OwnerScope};
use super::record::RunRow;
use super::retry::Schedule;
use super::suspend::Awakeable;
use crate::store::reconcile::ColumnType;
use crate::time::Instant;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use std::collections::BTreeMap;

/// A row as a source hands it over.
pub type Row = Map<String, Value>;

/// Column types a producer declares, by column name.
pub type Types = BTreeMap<String, ColumnType>;

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
    /// The source examined its whole snapshot; a skipped input does not set this.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub snapshot_complete: Option<bool>,
    /// Column types the source declares, spelled as a declaration spells them
    /// (`run.land.typed-pull`).
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub types: BTreeMap<String, String>,
    /// Inputs the source declined to land whole, which the run row sums
    /// (`run.record.skipped-count`).
    #[serde(default, skip_serializing_if = "is_zero")]
    pub skipped: u64,
    /// The inputs a directory walk declined, tallied by extension
    /// (`connector.source.declined-tally`).
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub declined: BTreeMap<String, u64>,
}

fn is_zero(n: &u64) -> bool {
    *n == 0
}

impl Pull {
    /// Decode a pull. Bytes outside the shape are a deterministic `SchemaIncompatible`.
    pub fn decode(bytes: &[u8]) -> Result<Pull, Failure> {
        serde_json::from_slice(bytes).map_err(|e| Failure::deterministic(FailureTag::SchemaIncompatible, format!("a pull is `{{\"rows\", \"cursor\", \"more\", \"snapshot_complete\", \"types\", \"skipped\", \"declined\"}}`: {e}")))
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

impl<S: Source + ?Sized> Source for &mut S {
    fn pull(&mut self, request: &PullRequest, cancel: &dyn Cancellation) -> Result<Vec<u8>, Failure> {
        (**self).pull(request, cancel)
    }
}

/// The most bytes one run stages before its commit (`run.own.staged-bytes`).
pub const STAGED_BYTES_PER_RUN: u64 = 1024 * 1024 * 1024;

/// One shaped batch the runner hands the destination to stage as one part of its run,
/// before its next pull (`run.own.backpressure`).
#[derive(Debug, Clone, PartialEq)]
pub struct Stage {
    pub pipeline_id: String,
    pub table: String,
    pub run_id: String,
    pub site_id: String,
    /// The batch's ordinal among the run's staged batches.
    pub ordinal: u32,
    /// Rows the run staged before this batch.
    pub row_offset: u64,
    pub rows: Vec<Row>,
    /// The column types the run's pulls declared so far, carried through the shape stages.
    pub types: Types,
}

/// A handle on one staged part, measured at the destination. A part joins no file list
/// until a commit names it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Part {
    pub name: String,
    pub rows: u64,
    pub bytes: u64,
}

/// One run's commit to one table: every part it staged, in pull order, and the position
/// the rows behind them reach.
#[derive(Debug, Clone, PartialEq)]
pub struct Commit {
    pub pipeline_id: String,
    pub table: String,
    pub run_id: String,
    pub site_id: String,
    pub parts: Vec<Part>,
    pub cursor: Option<Value>,
    pub committed_at: Instant,
    /// The fence of the single-writer lease the commit runs under.
    pub fence: Option<u64>,
    /// A complete zero-row replacement, distinct from an unchanged pull.
    pub replace_frontier: bool,
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

/// The land path a run commits through: each batch stages as a part under the run's own
/// directory, and one commit publishes the staged parts together.
pub trait Destination {
    /// Canonical preparation authority bound into owner pins before an execution opens.
    fn recording_identity(&self, _plan: &super::plan::Plan) -> Result<Option<String>, Failure> { Ok(None) }
    /// Validate continuation metadata before a protected pull can be recorded.
    fn validate_recorded_control(&self, _plan: &super::plan::Plan, _pull: &Pull) -> Result<(), Failure> { Ok(()) }
    /// Admit complete pre-shape clock lineage before an execution can retain progress.
    fn validate_recorded_clock(&self, _plan: &super::plan::Plan, _columns: &std::collections::BTreeSet<String>) -> Result<(), Failure> {
        Err(Failure::deterministic(super::FailureTag::Permanent, "protected clock requires safe typed progress lineage"))
    }
    /// Rewrite a shaped batch through canonical authority, returning only internal journal data.
    fn prepare_recorded(&mut self, _table: &str, _rows: Vec<Row>, _types: Types, _load_id: &str) -> Result<Value, Failure> {
        Err(Failure::deterministic(FailureTag::Permanent, "destination admits no prepared recording"))
    }
    /// Stage an internal recorded value admitted by the same canonical preparation authority.
    fn stage_recorded(&mut self, _stage: Stage, _prepared: &Value) -> Result<Part, Failure> {
        Err(Failure::deterministic(FailureTag::Permanent, "destination admits no prepared staging"))
    }
    /// Whether the table replaces its complete source state.
    fn replaces(&self, _table: &str) -> bool {
        false
    }
    /// Write `stage.rows` as one part of run `stage.run_id`, joining no file list until a
    /// commit names it.
    fn stage_batch(&mut self, stage: Stage) -> Result<Part, Failure>;
    /// Commit `commit.parts` as one atomic commit carrying `commit.cursor`. `precommit` runs
    /// immediately before the commit point; a refusal there lands nothing.
    fn commit(&mut self, commit: Commit, precommit: &dyn Fn() -> Result<(), Failure>) -> Result<Landed, Failure>;
    /// Remove every part run `run_id` staged for `table`, which no commit names once the
    /// run fails (`run.own.stage-discard`). A run with nothing staged discards nothing.
    fn discard(&mut self, table: &str, run_id: &str) -> Result<(), Failure>;
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
    /// A deterministic shape identity; unknown adapters refuse protected recording.
    fn recording_identity(&self) -> Result<Option<String>, super::RunError> { Ok(None) }
    /// Every column reached by this input clock, including projected-away names; unknown lineage refuses.
    fn recording_clock_columns(&self, _field: &str) -> Result<Option<std::collections::BTreeSet<String>>, super::RunError> { Ok(None) }
    fn shape(&self, rows: Vec<Row>) -> Result<Vec<Row>, super::RunError>;

    /// The column types after the stages: a stage renaming, dropping or retyping a
    /// column moves its declared type with it.
    fn shape_types(&self, types: Types) -> Types {
        types
    }
}

/// The shape that passes a batch through unchanged.
pub struct Unshaped;

impl Shape for Unshaped {
    fn recording_identity(&self) -> Result<Option<String>, super::RunError> { Ok(Some("unshaped-v1".into())) }
    fn recording_clock_columns(&self, field: &str) -> Result<Option<std::collections::BTreeSet<String>>, super::RunError> { Ok(Some(std::collections::BTreeSet::from([field.into()]))) }
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
    /// value, none erroring on another's write, and no partial value is ever readable
    /// (`run.journal.blob-write`). An adapter may serialize puts behind its own writes.
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

impl<T: JournalStore + ?Sized> JournalStore for std::sync::Arc<T> {
    fn create_pending(&self, key: &EntryKey, run_id: &str) -> Result<bool, Failure> { (**self).create_pending(key, run_id) }
    fn read(&self, key: &EntryKey) -> Result<Option<JournalRow>, Failure> { (**self).read(key) }
    fn replace_if_pending(&self, key: &EntryKey, holder: &str, run_id: &str) -> Result<bool, Failure> { (**self).replace_if_pending(key, holder, run_id) }
    fn record(&self, key: &EntryKey, value: &Stored) -> Result<Option<Stored>, Failure> { (**self).record(key, value) }
    fn release(&self, key: &EntryKey, run_id: &str) -> Result<(), Failure> { (**self).release(key, run_id) }
    fn rows(&self, execution_id: &str) -> Result<Vec<JournalRow>, Failure> { (**self).rows(execution_id) }
    fn retire(&self, execution_id: &str) -> Result<(), Failure> { (**self).retire(execution_id) }
    fn executions(&self) -> Result<Vec<String>, Failure> { (**self).executions() }
}

impl<T: BlobStore + ?Sized> BlobStore for std::sync::Arc<T> {
    fn put(&self, sha256: &str, bytes: &[u8]) -> Result<(), Failure> { (**self).put(sha256, bytes) }
    fn get(&self, sha256: &str) -> Result<Option<Vec<u8>>, Failure> { (**self).get(sha256) }
    fn sweep(&self, referenced: &[String], now_unix: i64) -> Result<Vec<String>, Failure> { (**self).sweep(referenced, now_unix) }
    fn swept_at(&self) -> Result<Option<i64>, Failure> { (**self).swept_at() }
    fn mark_swept(&self, at: i64) -> Result<(), Failure> { (**self).mark_swept(at) }
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

impl<T: AwakeableStore + ?Sized> AwakeableStore for std::sync::Arc<T> {
    fn insert(&self, row: &Awakeable) -> Result<(), Failure> {
        (**self).insert(row)
    }
    fn get(&self, token: &str) -> Result<Option<Awakeable>, Failure> {
        (**self).get(token)
    }
    fn update(&self, token: &str, edit: &mut dyn FnMut(&mut Awakeable) -> Result<bool, Failure>) -> Result<Option<Awakeable>, Failure> {
        (**self).update(token, edit)
    }
    fn rows(&self) -> Result<Vec<Awakeable>, Failure> {
        (**self).rows()
    }
}

/// What opening an execution names: its scope, the pins a pending owner is held to, the
/// attempt's run identity and the retry schedule its steps run under
/// (`run.journal.substrate-port`).
#[derive(Debug, Clone)]
pub struct OpenExecution {
    pub scope: OwnerScope,
    pub pins: OwnerPins,
    /// The catalog run id of this attempt; one id names one attempt.
    pub run_id: String,
    pub site_id: String,
    pub pid: u32,
    pub boot_id: String,
    pub trace_id: Option<String>,
    /// The connector build admitted for a table or chunk scope; `None` for a host scope.
    pub connector: Option<ConnectorPin>,
    /// The schedule every step of the execution retries under.
    pub schedule: Schedule,
}

/// What a substrate hosts: the connector worlds it runs and whether it wires an
/// awakeable store.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Capabilities {
    pub worlds: Vec<String>,
    pub awakeables: bool,
}

impl Capabilities {
    /// Whether a connector implementing `world` runs on this substrate.
    pub fn hosts(&self, world: &str) -> bool {
        self.worlds.iter().any(|w| w == world)
    }
}

/// What an execution reads for an awakeable it suspended on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Wake {
    /// Still suspended.
    Pending,
    /// The resume payload, read back from the journal.
    Resumed(Vec<u8>),
    TimedOut,
}

/// A step's effect: it runs under the execution's cancellation and answers the bytes to record.
pub type StepEffect<'a> = dyn FnMut(&dyn Cancellation) -> Result<Vec<u8>, Failure> + 'a;

/// The outcome an execution closes on.
#[derive(Debug, Clone, PartialEq)]
pub enum Outcome {
    Success { rows: u64, bytes: u64, batches: u64 },
    Failed(Failure),
}

/// The run-path substrate (`run.journal.substrate-port`): it describes what it hosts and
/// opens or resumes an execution under a scope for a content-hashed plan reference.
pub trait Substrate {
    type Error;
    type Execution<'s>: ExecutionPort<Error = Self::Error>
    where
        Self: 's;

    fn capabilities(&self) -> Capabilities;
    /// Open an execution under `open.scope`, resuming the scope's pending owner when its
    /// pins hold; a moved pin refuses before any replay.
    fn open(&self, open: &OpenExecution) -> Result<Self::Execution<'_>, Self::Error>;
}

/// One open execution: it records step outputs, commits a cursor, suspends on awakeables
/// and closes. Dropped unclosed, it records nothing and its owner stays pending
/// (`run.own.unclosed-execution`).
pub trait ExecutionPort {
    type Error;

    /// The id the execution's recorded work keys on.
    fn execution_id(&self) -> &str;
    /// The content-hashed plan the execution resolves for its whole life.
    fn plan_ref(&self) -> &str;
    /// The position the scope's cursor held when the execution opened.
    fn position(&self) -> Option<&Value>;
    /// Resolve step `label` over `input`: the recorded value on replay, else `effect`
    /// under the execution's retry schedule, recorded once.
    fn step(&mut self, label: &str, input: &[u8], effect: &mut StepEffect<'_>) -> Result<Vec<u8>, Self::Error>;
    /// Commit `position` as the scope's cursor and retire the owner in one transaction.
    fn commit(&mut self, position: Option<Value>) -> Result<(), Self::Error>;
    /// Suspend step `label` on a fresh awakeable living `ttl_secs`; returns its token.
    fn suspend(&mut self, label: &str, ttl_secs: u64) -> Result<String, Self::Error>;
    /// What the awakeable under `token` holds for this execution.
    fn awaited(&mut self, token: &str) -> Result<Wake, Self::Error>;
    /// Close on `outcome`, returning the run row as closed.
    fn close(self, outcome: Outcome) -> Result<RunRow, Self::Error>
    where
        Self: Sized;
}
