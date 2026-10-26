//! `topology.coordinate`: lease rows and their fence, the cursor compare-and-swap, and
//! the `Catalog` port every catalog backend sits behind. Code above the port names no
//! backend (`topology.coordinate.catalog-port`).

use crate::run::backfill::ChunkRow;
use crate::run::failure::{Failure, FailureTag};
use crate::run::own::{ExecutionOwner, OwnerPins, OwnerScope};
use crate::run::record::{select_history, HistoryPage, RunRow, Window};
use crate::run::RunError;
use crate::store::StoreError;
use crate::time::Instant;
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Granted life of a cadence lease: 90 s (`topology.coordinate.cadence-lease-ttl`).
pub const CADENCE_LEASE_TTL_SECS: u64 = 90;

/// Interval at which the reconciler renews a held cadence lease: 30 s
/// (`topology.coordinate.cadence-lease-renewal`).
pub const CADENCE_LEASE_RENEWAL_SECS: u64 = 30;

/// What a lease row is keyed by (`topology.coordinate.lease-row`).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum LeaseKey {
    Pipeline(String),
    Partition { pipeline: String, partition: String },
    Compaction(String),
    Cadence(String),
    /// A unit's dispatch record: each dispatch takes this key for zero seconds, so the
    /// row's expiry is the unit's last dispatch instant (`surface.arm.next-fire-from-history`).
    Dispatch(String),
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
            // A scheduled job's unit id is `job:<name>`, and the file catalog names one file per
            // key, where Windows refuses ':'. Escaping '%' first keeps the spelling injective.
            LeaseKey::Dispatch(u) => format!("dispatch.{}", clean(&u.replace('%', "%25")).replace(':', "%3A")),
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

    /// Renew a held lease for `ttl_secs` from `now`: the conditional update matches only
    /// while `lease` still holds the row's current fence and holder, and keeps the fence.
    pub fn renew(&mut self, lease: &Lease, now: Instant, ttl_secs: u64) -> Option<Lease> {
        if self.fence != lease.fence || self.holder.as_deref() != Some(lease.holder.as_str()) {
            return None;
        }
        let expires_at = now.plus_secs(ttl_secs);
        self.expires_at = Some(expires_at);
        Some(Lease { expires_at, ..lease.clone() })
    }

    /// Whether `lease` still holds this row at `now`: its fence and holder current and its
    /// expiry ahead.
    pub fn held_by(&self, lease: &Lease, now: Instant) -> bool {
        self.fence == lease.fence && self.holder.as_deref() == Some(lease.holder.as_str()) && self.expires_at.is_some_and(|e| now < e)
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

/// One exact pending owner and active attempt admitted by a callback.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExecutionExpectation {
    pub scope: OwnerScope,
    pub execution_id: String,
    pub pins: OwnerPins,
    pub run_id: String,
}

/// Canonical lock order and exact identities for one bounded dispatch or publication.
#[derive(Debug, Clone)]
pub struct ExecutionAdmission {
    owners: Vec<ExecutionExpectation>,
    leases: Vec<Lease>,
    pub publication: bool,
}

impl ExecutionAdmission {
    pub fn new(mut owners: Vec<ExecutionExpectation>, mut leases: Vec<Lease>, publication: bool) -> Result<Self, Failure> {
        owners.sort_by_key(|o| serde_json::to_string(&o.scope).expect("scope serializes"));
        for pair in owners.windows(2) {
            if pair[0].scope == pair[1].scope && pair[0] != pair[1] { return Err(Failure::new(FailureTag::Permanent, "conflicting execution expectations")); }
        }
        owners.dedup();
        leases.sort_by(|a, b| a.key.cmp(&b.key));
        for pair in leases.windows(2) {
            if pair[0].key == pair[1].key && (pair[0].holder != pair[1].holder || pair[0].fence != pair[1].fence) { return Err(Failure::new(FailureTag::Permanent, "conflicting lease expectations")); }
        }
        leases.dedup_by(|a, b| a.key == b.key);
        Ok(Self { owners, leases, publication })
    }
    pub fn owners(&self) -> &[ExecutionExpectation] { &self.owners }
    pub fn leases(&self) -> &[Lease] { &self.leases }
}

/// The guard remains on its acquiring thread through the bounded callback. Callbacks
/// may update run metadata, but must not mutate their guarded owner or lease rows.
pub trait ExecutionGuard {}
struct CooperativeGuard;
impl ExecutionGuard for CooperativeGuard {}

/// The single-writer operations and the run-path rows they guard.
pub trait Catalog {
    /// Strict adapters serialize owner replacement and lease takeover with dispatch.
    fn supports_strict_execution_guards(&self) -> bool { false }

    /// Local adapters check admission cooperatively. Strict adapters retain durable
    /// owner and lease locks until the returned token drops, including parent scopes.
    fn execution_guard(&self, admission: &ExecutionAdmission) -> Result<Box<dyn ExecutionGuard>, Failure> {
        for expected in admission.owners() {
            let owner = self.owner_at(&expected.scope)?;
            let row = self.run(&expected.run_id)?;
            if !owner.as_ref().is_some_and(|o| o.execution_id == expected.execution_id && o.pins == expected.pins && (expected.scope.host_id().is_none() || o.attempts.last() == Some(&expected.run_id))) ||
                !row.as_ref().is_some_and(|r| r.status.is_in_flight() && r.owner.is_some() && (admission.publication || r.stop.is_none())) {
                return Err(Failure::new(FailureTag::Transient, "execution admission was superseded"));
            }
        }
        for lease in admission.leases() {
            if !self.lease_holds(lease)? { return Err(Failure::new(FailureTag::Transient, "execution lease was superseded")); }
        }
        Ok(Box::new(CooperativeGuard))
    }
    /// The catalog's clock, against which every lease expiry is evaluated
    /// (`topology.coordinate.catalog-clock`).
    fn now(&self) -> Result<Instant, Failure>;

    /// Take the lease on `key` for `ttl_secs`, or `None` while another holder has it.
    fn acquire(&self, key: &LeaseKey, holder: &str, ttl_secs: u64) -> Result<Option<Lease>, Failure>;
    fn release(&self, lease: &Lease) -> Result<(), Failure>;
    /// Extend a held lease; `None` once a later acquisition took the row.
    fn renew(&self, lease: &Lease, ttl_secs: u64) -> Result<Option<Lease>, Failure>;
    /// Whether `lease` still holds its row on the catalog's clock.
    fn lease_holds(&self, lease: &Lease) -> Result<bool, Failure>;
    fn lease_row(&self, key: &LeaseKey) -> Result<LeaseRow, Failure>;

    /// The cursor row of a scope.
    fn cursor_at(&self, scope: &OwnerScope) -> Result<CursorRow, Failure>;
    /// One conditional update predicated on the stored version and, under a lease, on
    /// the holder's fence (`topology.coordinate.cursor-cas`).
    fn cursor_cas_at(&self, scope: &OwnerScope, expected_version: u64, next: CursorRow, fence: Option<&Lease>) -> Result<Cas, Failure>;

    /// The pending execution owner of a scope; every owner is keyed on its scope
    /// (`run.own.host-scope`).
    fn owner_at(&self, scope: &OwnerScope) -> Result<Option<ExecutionOwner>, Failure>;
    /// Persist `owner` under its own scope.
    fn put_owner(&self, owner: &ExecutionOwner) -> Result<(), Failure>;
    /// Retire the owner of `scope` holding `execution_id` and, given a cursor row and the
    /// version it was read at, cache the position its commit reached, in one transaction
    /// (`run.own.retirement`). The update is conditional on that version and, under a
    /// lease, on the holder's fence; the owner's journal is unreachable once it applies.
    /// Under a chunk scope holding a chunk row, a cursor commit also marks the chunk done in
    /// that transaction: a completed chunk retires the same way.
    /// The same transaction names the retired execution as the scope's last retirement,
    /// [`Catalog::retired_at`].
    fn retire_at(&self, scope: &OwnerScope, execution_id: &str, cursor: Option<(CursorRow, u64)>, fence: Option<&Lease>) -> Result<Cas, Failure>;
    /// The execution the scope's last applied retirement retired, whose journal rows a
    /// journal store apart from the catalog deletes after that commit and again at the
    /// scope's next open (`run.journal.collection`); `None` before any retirement.
    fn retired_at(&self, scope: &OwnerScope) -> Result<Option<String>, Failure>;

    /// The chunk row of a chunk scope; `None` when no plan holds it. A catalog keeping no
    /// chunk plans holds none.
    fn chunk_at(&self, _scope: &OwnerScope) -> Result<Option<ChunkRow>, Failure> {
        Ok(None)
    }
    /// Every chunk row of a pipeline's table, in plan order.
    fn chunks(&self, _pipeline_id: &str, _table: &str) -> Result<Vec<ChunkRow>, Failure> {
        Ok(Vec::new())
    }
    /// Write `row` under its chunk scope, keeping that scope's cursor and owner.
    fn put_chunk(&self, row: &ChunkRow) -> Result<(), Failure> {
        Err(Failure::new(FailureTag::Permanent, format!("this catalog keeps no chunk plan; chunk `{}` of `{}`/`{}` has no home", row.chunk, row.pipeline_id, row.table)))
    }
    /// Remove every chunk scope of a pipeline's table, its plan row, cursor, owner and last
    /// retirement, answering the removed plan rows in plan order. A catalog keeping no chunk
    /// plans holds none to remove.
    fn delete_chunks(&self, _pipeline_id: &str, _table: &str) -> Result<Vec<ChunkRow>, Failure> {
        Ok(Vec::new())
    }
    /// Apply `f` to the chunk row of `scope` and, given an execution id, retire the scope's
    /// owner while it holds that id, in one transaction; `None` when no row exists.
    fn update_chunk(&self, scope: &OwnerScope, _retire: Option<&str>, _f: &mut dyn FnMut(&mut ChunkRow)) -> Result<Option<ChunkRow>, Failure> {
        Err(Failure::new(FailureTag::Permanent, format!("this catalog keeps no chunk plan; {scope} has no chunk row")))
    }

    /// The cursor row of a pipeline's table.
    fn cursor(&self, pipeline_id: &str, table: &str) -> Result<CursorRow, Failure> {
        self.cursor_at(&OwnerScope::table(pipeline_id, table))
    }
    /// [`Catalog::cursor_cas_at`] on a pipeline's table.
    fn cursor_cas(&self, pipeline_id: &str, table: &str, expected_version: u64, next: CursorRow, fence: Option<&Lease>) -> Result<Cas, Failure> {
        self.cursor_cas_at(&OwnerScope::table(pipeline_id, table), expected_version, next, fence)
    }
    /// The pending execution owner of a pipeline's table.
    fn owner(&self, pipeline_id: &str, table: &str) -> Result<Option<ExecutionOwner>, Failure> {
        self.owner_at(&OwnerScope::table(pipeline_id, table))
    }
    /// [`Catalog::retire_at`] on a pipeline's table.
    fn retire(&self, pipeline_id: &str, table: &str, execution_id: &str, cursor: Option<(CursorRow, u64)>, fence: Option<&Lease>) -> Result<Cas, Failure> {
        self.retire_at(&OwnerScope::table(pipeline_id, table), execution_id, cursor, fence)
    }

    fn put_run(&self, row: &RunRow) -> Result<(), Failure>;
    fn run(&self, run_id: &str) -> Result<Option<RunRow>, Failure>;
    fn runs(&self, pipeline_id: Option<&str>) -> Result<Vec<RunRow>, Failure>;
    /// One window of `pipeline_id`'s history, or every pipeline's under `None`, newest
    /// first (`run.record.history-window`). A SQL catalog applies the bound in its `WHERE`
    /// over an indexed start column and the ceiling as its `LIMIT`; this default filters
    /// what [`Catalog::runs`] returns.
    fn history(&self, pipeline_id: Option<&str>, window: &Window) -> Result<HistoryPage, Failure> {
        Ok(select_history(self.runs(pipeline_id)?, window))
    }
    /// The start of `pipeline_id`'s newest journaled run, which the scheduler reads on
    /// every beat; a backend answers it without decoding the pipeline's run rows.
    fn last_run_start(&self, pipeline_id: &str) -> Result<Option<Instant>, Failure> {
        Ok(self.runs(Some(pipeline_id))?.iter().map(|r| r.started_at).max())
    }
    /// Every run row carrying an `unpushed` mark (`store.drain.mark`). A SQL catalog
    /// filters in its `WHERE`; this default filters what [`Catalog::runs`] returns.
    fn unpushed_runs(&self) -> Result<Vec<RunRow>, Failure> {
        Ok(self.runs(None)?.into_iter().filter(|r| r.unpushed.is_some()).collect())
    }
    /// Apply `f` to a run row under the catalog's write serialization; `None` when no
    /// row carries `run_id`, and the row unchanged when `f` refuses.
    fn update_run(&self, run_id: &str, f: &mut dyn FnMut(&mut RunRow) -> Result<(), RunError>) -> Result<Option<Result<RunRow, RunError>>, Failure>;
}
