//! `run.backfill`: the `[pipeline.backfill]` block, the chunk plan it shapes by cursor kind,
//! one row per chunk, and the transitions a chunk row takes as a worker leases, renews,
//! commits, settles and rewinds it. The engine's scheduler persists the rows through the
//! `Catalog` port.

use super::advance::CursorKind;
use super::own::OwnerScope;
use super::record::RunStatus;
use super::RunError;
use crate::time::Instant;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::cmp::Ordering;
use std::path::PathBuf;

/// The phase a pipeline runs under (`run.backfill.phase`): `planning` computes the chunk
/// plan, `seeding` loads consumer-held history, `backfilling` runs chunks across ticks,
/// `streaming` pulls the incremental delta.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PipelinePhase {
    Planning,
    Seeding,
    Backfilling,
    Streaming,
}

impl PipelinePhase {
    pub fn name(self) -> &'static str {
        match self {
            PipelinePhase::Planning => "planning",
            PipelinePhase::Seeding => "seeding",
            PipelinePhase::Backfilling => "backfilling",
            PipelinePhase::Streaming => "streaming",
        }
    }
}

/// Life of an unrenewed chunk lease, in seconds (`run.backfill.chunk-lease`).
pub const CHUNK_LEASE_TTL_SECS: u64 = 300;

/// Renewal cadence of a held chunk lease, in seconds (`run.backfill.chunk-lease`).
pub const CHUNK_LEASE_RENEWAL_SECS: u64 = 60;

/// A `[pipeline.backfill]` block (`run.backfill.block`).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BackfillBlock {
    /// The bound on a plan's chunk count.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_chunks: Option<u32>,
    /// One clock-ranged chunk's width in cursor units.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub chunk_size: Option<i64>,
    /// The floor of the planned range.
    #[serde(default)]
    pub start_from: i64,
}

impl BackfillBlock {
    /// Read a declared block; an unknown key or a mistyped value raises `PipelineSpecInvalid`.
    pub fn from_value(v: &Value) -> Result<BackfillBlock, RunError> {
        serde_json::from_value(v.clone()).map_err(|e| RunError::PipelineSpecInvalid(format!("[pipeline.backfill] {v}: {e}")))
    }

    /// The width and chunk bound a clock-ranged plan needs, each declared and positive.
    fn clock_range(&self) -> Result<(i64, u32), RunError> {
        match (self.chunk_size, self.max_chunks) {
            (Some(size), Some(max)) if size > 0 && max > 0 => Ok((size, max)),
            _ => Err(RunError::PipelineSpecInvalid(format!(
                "[pipeline.backfill] declares chunk_size {:?} and max_chunks {:?}; a clock-ranged plan needs both, each above zero",
                self.chunk_size, self.max_chunks
            ))),
        }
    }
}

/// A table's chunk plan over `[start_from, upto)` (`run.backfill.chunk-plan`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChunkPlan {
    /// Whether a chunk waits on every earlier chunk of the plan.
    pub sequential: bool,
    /// Each chunk's name and half-open window, in plan order.
    pub chunks: Vec<(String, Window)>,
}

impl ChunkPlan {
    /// Shape the plan of one stream by its cursor kind: `monotonic` cuts independent
    /// `chunk_size`-wide windows from the floor, `opaque-token` walks the whole range as one
    /// sequential chunk, and `snapshot-id` cuts the same windows walked in order. An empty
    /// range plans nothing; a range needing more than `max_chunks` windows raises
    /// `PipelineSpecInvalid` and plans nothing.
    pub fn of(kind: CursorKind, block: &BackfillBlock, upto: i64) -> Result<ChunkPlan, RunError> {
        let floor = block.start_from;
        if kind == CursorKind::OpaqueToken {
            let chunks = if upto > floor { vec![("c0".to_string(), Window { start: Bound::Int(floor), end: Bound::Int(upto) })] } else { Vec::new() };
            return Ok(ChunkPlan { sequential: true, chunks });
        }
        let (size, max) = block.clock_range()?;
        let span = (upto as i128 - floor as i128).max(0);
        let count = (span + size as i128 - 1) / size as i128;
        if count > max as i128 {
            return Err(RunError::PipelineSpecInvalid(format!(
                "[pipeline.backfill] range [{floor}, {upto}) needs {count} chunks of width {size}, past max_chunks {max}"
            )));
        }
        let chunks = (0..count as i64)
            .map(|i| {
                let start = floor + i * size;
                (format!("c{i}"), Window { start: Bound::Int(start), end: Bound::Int(start.saturating_add(size).min(upto)) })
            })
            .collect();
        Ok(ChunkPlan { sequential: kind == CursorKind::SnapshotId, chunks })
    }
}

/// A worker's hold on a chunk row (`run.backfill.chunk-lease`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChunkLease {
    pub node: String,
    /// The chunk's attempt count the claim raised it to; the commit fences on it.
    pub attempt: u32,
    pub expires_at: Instant,
}

impl ChunkLease {
    /// Whether the holder owes a renewal at `now`: [`CHUNK_LEASE_RENEWAL_SECS`] after the
    /// lease was taken or last renewed (`run.backfill.chunk-lease`).
    pub fn renew_due(&self, now: Instant) -> bool {
        now.plus_secs(CHUNK_LEASE_TTL_SECS - CHUNK_LEASE_RENEWAL_SECS) >= self.expires_at
    }
}

/// `s` as one path segment: `%` and `/` escaped.
pub fn path_segment(s: &str) -> String {
    s.replace('%', "%25").replace('/', "%2F")
}

/// The path, relative to the landing root, one attempt writes under: run, node, chunk and
/// attempt, each one segment (`run.backfill.chunk-lease`).
pub fn chunk_path(run_id: &str, node: &str, chunk: &str, attempt: u32) -> PathBuf {
    [path_segment(run_id), path_segment(node), path_segment(chunk), attempt.to_string()].iter().collect()
}

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
    /// Whether the chunk waits on every earlier chunk of its plan (`run.backfill.chunk-plan`).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub sequential: bool,
    /// The last lease a worker took on the chunk; a done row keeps the committing one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lease: Option<ChunkLease>,
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
            sequential: false,
            lease: None,
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

    /// Whether a worker may lease the chunk at `now`: it is claimable, or it runs under a
    /// lease that expired (`run.backfill.attempt-isolation`).
    pub fn leasable(&self, now: Instant) -> bool {
        self.claimable() || (self.status == ChunkStatus::Running && self.lease.as_ref().is_some_and(|l| l.expires_at <= now))
    }

    /// `node`'s lease at `now`: the chunk runs under the next attempt for
    /// [`CHUNK_LEASE_TTL_SECS`]. `None` while the chunk is done or another lease holds it.
    pub fn lease(&mut self, node: &str, now: Instant) -> Option<ChunkLease> {
        if !self.leasable(now) {
            return None;
        }
        self.claim(now);
        let lease = ChunkLease { node: node.to_string(), attempt: self.attempts, expires_at: now.plus_secs(CHUNK_LEASE_TTL_SECS) };
        self.lease = Some(lease.clone());
        Some(lease)
    }

    /// Extend `held` for another [`CHUNK_LEASE_TTL_SECS`] from `now`; `None` once the chunk
    /// left running or a later attempt superseded the holder.
    pub fn renew(&mut self, held: &ChunkLease, now: Instant) -> Option<ChunkLease> {
        let current = self.lease.as_mut().filter(|l| l.node == held.node && l.attempt == held.attempt)?;
        if self.status != ChunkStatus::Running || self.attempts != held.attempt {
            return None;
        }
        current.expires_at = now.plus_secs(CHUNK_LEASE_TTL_SECS);
        Some(current.clone())
    }

    /// The fenced commit of `attempt`: it applies only while `attempt = :mine AND status !=
    /// 'done'` and the running chunk's lease is that attempt's, marking the chunk done. A
    /// rewind or settle clears the lease, so a commit from before it applies nothing.
    /// Answers whether it applied (`run.backfill.fenced-commit`).
    pub fn commit_fenced(&mut self, attempt: u32, now: Instant) -> bool {
        let held = self.status == ChunkStatus::Running && self.lease.as_ref().is_some_and(|l| l.attempt == attempt);
        if self.attempts != attempt || !held {
            return false;
        }
        self.complete(now);
        true
    }

    /// The attempt path the done row commits under `run_id`; `None` while the row is not
    /// done or no lease committed it (`run.backfill.commit-signal`).
    pub fn committed_path(&self, run_id: &str) -> Option<PathBuf> {
        let lease = self.lease.as_ref().filter(|l| self.status == ChunkStatus::Done && l.attempt == self.attempts)?;
        Some(chunk_path(run_id, &lease.node, &self.chunk, lease.attempt))
    }

    /// Whether `attempt` is the running chunk's current attempt, which may still commit
    /// whether or not its lease expired, until a later attempt supersedes it.
    pub fn current_attempt(&self, attempt: u32) -> bool {
        self.status == ChunkStatus::Running && self.attempts == attempt
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
            RunStatus::Success | RunStatus::Pending | RunStatus::Running | RunStatus::Waiting => return,
        }
        self.lease = None;
    }

    /// A rewind: the chunk returns to pending, its committed position no longer counts, and
    /// its lease clears so no earlier attempt commits (`run.backfill.rewind`).
    pub fn rewind(&mut self) {
        self.status = ChunkStatus::Pending;
        self.lease = None;
        self.completed_at = None;
        self.cursor_committed = false;
    }
}
