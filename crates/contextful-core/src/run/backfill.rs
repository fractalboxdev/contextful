//! `run.backfill`: a table's chunk plan, one row per chunk, and the transitions a chunk
//! row takes as the scheduler claims, settles and rewinds it. The engine's scheduler
//! persists the rows through the `Catalog` port.

use super::own::OwnerScope;
use super::record::RunStatus;
use super::RunError;
use crate::time::Instant;
use serde::{Deserialize, Serialize};
use std::cmp::Ordering;

/// One end of a half-open chunk range on the cursor's scale: an integer position, or a
/// text stamp ordered as written.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Bound {
    Int(i64),
    Text(String),
}

impl Bound {
    /// The order of two bounds on one scale; `None` across scales.
    fn order(&self, other: &Bound) -> Option<Ordering> {
        match (self, other) {
            (Bound::Int(a), Bound::Int(b)) => Some(a.cmp(b)),
            (Bound::Text(a), Bound::Text(b)) => Some(a.cmp(b)),
            _ => None,
        }
    }
}

impl std::fmt::Display for Bound {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Bound::Int(n) => write!(f, "{n}"),
            Bound::Text(s) => write!(f, "`{s}`"),
        }
    }
}

/// A half-open range `[start, end)` on one cursor scale.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Window {
    pub start: Bound,
    pub end: Bound,
}

impl Window {
    /// A window from `start` up to `end`. An inverted, empty or scale-incomparable window
    /// raises `PipelineRewindWindowInvalid` (`run.backfill.rewind-invalid`).
    pub fn new(start: Bound, end: Bound) -> Result<Window, RunError> {
        match start.order(&end) {
            Some(Ordering::Less) => Ok(Window { start, end }),
            Some(_) => Err(RunError::PipelineRewindWindowInvalid(format!("the window [{start}, {end}) is inverted or empty; its start must precede its end"))),
            None => Err(RunError::PipelineRewindWindowInvalid(format!("the window [{start}, {end}) mixes an integer and a text bound, which order on no one scale"))),
        }
    }

    /// Whether two half-open windows share a position; windows on different scales share none.
    pub fn overlaps(&self, other: &Window) -> bool {
        self.start.order(&other.end) == Some(Ordering::Less) && other.start.order(&self.end) == Some(Ordering::Less)
    }

    /// Whether `other` sits on this window's scale.
    pub fn comparable(&self, other: &Window) -> bool {
        self.start.order(&other.start).is_some()
    }
}

/// Where a chunk stands in its plan.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChunkStatus {
    Pending,
    Running,
    Failed,
    Done,
}

/// One chunk of a table's backfill plan (`run.backfill.chunk`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChunkRow {
    pub pipeline_id: String,
    pub table: String,
    pub chunk: String,
    /// The chunk's place in its plan; a plan resumes at the lowest ordinal not done.
    pub ordinal: u32,
    /// The half-open range of cursor positions the chunk pulls.
    pub predicate: Window,
    pub status: ChunkStatus,
    /// How many times a worker claimed the chunk.
    pub attempts: u32,
    #[serde(default)]
    pub started_at: Option<Instant>,
    #[serde(default)]
    pub completed_at: Option<Instant>,
    /// Whether the chunk's commit cached its position.
    #[serde(default)]
    pub cursor_committed: bool,
}

impl ChunkRow {
    /// A pending chunk no worker has claimed.
    pub fn planned(pipeline_id: &str, table: &str, chunk: &str, ordinal: u32, predicate: Window) -> ChunkRow {
        ChunkRow {
            pipeline_id: pipeline_id.to_string(),
            table: table.to_string(),
            chunk: chunk.to_string(),
            ordinal,
            predicate,
            status: ChunkStatus::Pending,
            attempts: 0,
            started_at: None,
            completed_at: None,
            cursor_committed: false,
        }
    }

    /// The owner scope the chunk's executions run under.
    pub fn scope(&self) -> OwnerScope {
        OwnerScope::chunk(&self.pipeline_id, &self.table, &self.chunk)
    }

    /// Whether a worker may claim the chunk: it is neither running nor done.
    pub fn claimable(&self) -> bool {
        matches!(self.status, ChunkStatus::Pending | ChunkStatus::Failed)
    }

    /// A worker's claim: the chunk runs, and its attempt count rises by one.
    pub fn claim(&mut self, now: Instant) {
        self.status = ChunkStatus::Running;
        self.attempts += 1;
        self.started_at = Some(now);
    }

    /// The chunk's commit: done, its position cached (`run.own.retirement`).
    pub fn complete(&mut self, now: Instant) {
        self.status = ChunkStatus::Done;
        self.completed_at = Some(now);
        self.cursor_committed = true;
    }

    /// Settle a running chunk on the status its run closed on. A stopped chunk returns to
    /// pending with its attempt count unchanged (`run.cancel.resumable-remains`); a failed
    /// one reads failed until a later claim; a committed one stays done.
    pub fn settle(&mut self, status: RunStatus) {
        if self.status != ChunkStatus::Running {
            return;
        }
        match status {
            RunStatus::Canceled => self.status = ChunkStatus::Pending,
            RunStatus::Failed | RunStatus::PartialFailure => self.status = ChunkStatus::Failed,
            RunStatus::Success | RunStatus::Pending | RunStatus::Running | RunStatus::Waiting => {}
        }
    }

    /// A rewind: the chunk returns to pending and its committed position no longer counts
    /// (`run.backfill.rewind`).
    pub fn rewind(&mut self) {
        self.status = ChunkStatus::Pending;
        self.completed_at = None;
        self.cursor_committed = false;
    }
}
