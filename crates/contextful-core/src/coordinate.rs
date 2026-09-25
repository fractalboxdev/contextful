//! `topology.coordinate`: lease rows and their fence, the cursor compare-and-swap, and
//! the `Catalog` port every catalog backend sits behind. Code above the port names no
//! backend (`topology.coordinate.catalog-port`).

use crate::run::failure::Failure;
use crate::run::own::ExecutionOwner;
use crate::run::record::RunRow;
use crate::run::RunError;
use crate::store::StoreError;
use crate::time::Instant;
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Granted life of a cadence lease: 90 s (`topology.coordinate.cadence-lease-ttl`).
pub const CADENCE_LEASE_TTL_SECS: u64 = 90;

/// What a lease row is keyed by (`topology.coordinate.lease-row`).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum LeaseKey {
    Pipeline(String),
    Partition { pipeline: String, partition: String },
    Compaction(String),
    Cadence(String),
}

impl LeaseKey {
    /// The key's stored spelling, path-safe.
    pub fn spelling(&self) -> String {
        let clean = |s: &str| s.replace(['/', '\\'], "%2F");
        match self {
            LeaseKey::Pipeline(p) => format!("pipeline.{}", clean(p)),
            LeaseKey::Partition { pipeline, partition } => format!("partition.{}.{}", clean(pipeline), clean(partition)),
            LeaseKey::Compaction(t) => format!("compaction.{}", clean(t)),
            LeaseKey::Cadence(d) => format!("cadence.{}", clean(d)),
        }
    }
}

/// A lease row: a holder, an expiry instant and a fence.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct LeaseRow {
    #[serde(default)]
    pub holder: Option<String>,
    #[serde(default)]
    pub expires_at: Option<Instant>,
    pub fence: u64,
}

/// A held lease.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Lease {
    pub key: String,
    pub holder: String,
    pub fence: u64,
    pub expires_at: Instant,
}

impl LeaseRow {
    /// The conditional update an acquisition is: it matches a row with no holder or an
    /// expired one, evaluated against the catalog's own `now`, and increments the fence
    /// (`topology.coordinate.fence-advances`).
    pub fn acquire(&mut self, key: &str, holder: &str, now: Instant, ttl_secs: u64) -> Option<Lease> {
        let free = self.holder.is_none() || self.expires_at.is_none_or(|e| e <= now);
        if !free {
            return None;
        }
        self.fence += 1;
        self.holder = Some(holder.to_string());
        let expires_at = now.plus_secs(ttl_secs);
        self.expires_at = Some(expires_at);
        Some(Lease { key: key.to_string(), holder: holder.to_string(), fence: self.fence, expires_at })
    }

    /// Release: the holder clears and the fence stays, so no fence repeats for a key.
    pub fn release(&mut self, lease: &Lease) {
        if self.fence == lease.fence && self.holder.as_deref() == Some(lease.holder.as_str()) {
            self.holder = None;
            self.expires_at = None;
        }
    }

    /// Whether a commit predicated on `fence` matches this row
    /// (`topology.coordinate.fenced-commit`).
    pub fn admits(&self, fence: u64) -> bool {
        self.fence == fence
    }
}

/// A cursor row: the cached position and the version a compare-and-swap predicates on.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CursorRow {
    #[serde(default)]
    pub position: Option<Value>,
    pub version: u64,
    /// The run whose commit marker the cached position mirrors.
    #[serde(default)]
    pub marker_run_id: Option<String>,
    #[serde(default)]
    pub marker_committed_at: Option<Instant>,
}

/// What a compare-and-swap did, read back from its affected-row count.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Cas {
    /// One row matched.
    Applied,
    /// The stored version moved; zero rows matched.
    VersionMoved,
    /// A higher fence took the lease; the commit stays unapplied.
    Fenced(StoreError),
}

/// The `Catalog` port: the single-writer operations and the run-path rows they guard.
pub trait Catalog {
    /// The catalog's clock, against which every lease expiry is evaluated
    /// (`topology.coordinate.catalog-clock`).
    fn now(&self) -> Result<Instant, Failure>;

    /// Take the lease on `key` for `ttl_secs`, or `None` while another holder has it.
    fn acquire(&self, key: &LeaseKey, holder: &str, ttl_secs: u64) -> Result<Option<Lease>, Failure>;
    fn release(&self, lease: &Lease) -> Result<(), Failure>;
    fn lease_row(&self, key: &LeaseKey) -> Result<LeaseRow, Failure>;

    /// The cursor row of a pipeline's table.
    fn cursor(&self, pipeline_id: &str, table: &str) -> Result<CursorRow, Failure>;
    /// One conditional update predicated on the stored version and, under a lease, on
    /// the holder's fence (`topology.coordinate.cursor-cas`).
    fn cursor_cas(&self, pipeline_id: &str, table: &str, expected_version: u64, next: CursorRow, fence: Option<&Lease>) -> Result<Cas, Failure>;

    /// The pending execution owner of a pipeline's table.
    fn owner(&self, pipeline_id: &str, table: &str) -> Result<Option<ExecutionOwner>, Failure>;
    fn put_owner(&self, owner: &ExecutionOwner) -> Result<(), Failure>;
    /// Retire the owner holding `execution_id` and cache the position its commit reached,
    /// in one transaction (`run.own.retirement`). Its journal is unreachable once this
    /// returns.
    fn retire(&self, pipeline_id: &str, table: &str, execution_id: &str, cursor: CursorRow) -> Result<(), Failure>;

    fn put_run(&self, row: &RunRow) -> Result<(), Failure>;
    fn run(&self, run_id: &str) -> Result<Option<RunRow>, Failure>;
    fn runs(&self, pipeline_id: Option<&str>) -> Result<Vec<RunRow>, Failure>;
    /// Apply `f` to a run row under the catalog's write serialization.
    fn update_run(&self, run_id: &str, f: &mut dyn FnMut(&mut RunRow) -> Result<(), RunError>) -> Result<Result<RunRow, RunError>, Failure>;
}
