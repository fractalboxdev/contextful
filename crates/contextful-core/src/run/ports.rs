//! The ports the runner drives: the source a pull reaches, the destination a run lands
//! through, and the cancellation every await observes.

use super::failure::{Failure, FailureTag};
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
    /// Land every batch as one atomic commit carrying `commit.cursor`.
    fn land(&mut self, commit: &Commit) -> Result<Landed, Failure>;
    /// The newest commit marker `pipeline_id` wrote to `table`.
    fn newest_marker(&self, pipeline_id: &str, table: &str) -> Result<Option<Marker>, Failure>;
}
