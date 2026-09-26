//! The runner: opens a run against its pinned plan, resolves each pull through the
//! journal under the step's retry schedule, lands every batch in one commit carrying the
//! position, retires the owner, and closes the run row.

use crate::cancel::{Cadence, CancelToken, Keeper};
use crate::project::Emitter;
use crate::fsutil::sleep_unless;
use crate::journal::{Journal, Resolved, StepError};
use contextful_core::coordinate::{Cas, Catalog, CursorRow, Lease, LeaseKey};
use contextful_core::run::advance::{admits, advance, frontier, open_watermark, resolve_concurrent, watermark, CursorKind};
use contextful_core::run::cancel::{mark, Scope};
use contextful_core::run::journal::{sha256_hex, EntryKey};
use contextful_core::run::own::{releases, ConnectorPin, ExecutionOwner, Pins};
use contextful_core::run::plan::{Plan, NATIVE_WORLD};
use contextful_core::run::ports::{Cancellation, Commit, Destination, Landed, Pull, PullRequest, Row, Shape, Source, Unshaped};
use contextful_core::run::project::{Change, StepPatch, StepStatus};
use contextful_core::run::record::{cap_error, select_history, HistoryPage, Owner, Phase, RunRow, RunStatus, Window, OWNER_LEASE_TTL_SECS};
use contextful_core::run::retry::{decide, Decision};
use contextful_core::run::{Failure, FailureTag, RunError};
use contextful_core::topology::TopologyError;
use serde_json::Value;
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// Why a run could not open, or a call on the engine refused.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum EngineError {
    #[error("{0}")]
    Refused(#[from] RunError),
    #[error("{0}")]
    Topology(#[from] TopologyError),
    #[error("{0}")]
    Failure(#[from] Failure),
}

/// One run to execute.
#[derive(Debug, Clone)]
pub struct RunSpec {
    pub plan: Plan,
    /// The connector build admitted for this run (`run.own.admission-pin`).
    pub connector: ConnectorPin,
    pub run_id: String,
    pub site_id: String,
    pub pid: u32,
    pub boot_id: String,
    pub trace_id: Option<String>,
}

/// The engine: the catalog behind its port, the journal, and the keeper's cadences.
#[derive(Clone)]
pub struct Engine {
    pub catalog: Arc<dyn Catalog + Send + Sync>,
    pub journal: Journal,
    pub cadence: Cadence,
    /// The live projection's emitter; every event follows the durable change it reports.
    pub emitter: Option<Emitter>,
}

/// A failure the body of a run closes on.
enum Close {
    Refused(RunError),
    Failed(Failure),
}

impl From<Failure> for Close {
    fn from(f: Failure) -> Close {
        Close::Failed(f)
    }
}

impl From<RunError> for Close {
    fn from(e: RunError) -> Close {
        Close::Refused(e)
    }
}

impl From<StepError> for Close {
    fn from(e: StepError) -> Close {
        match e {
            StepError::Failed(f) | StepError::Storage(f) => Close::Failed(f),
            StepError::Journal(r) => Close::Refused(r),
        }
    }
}

impl Close {
    /// The tag and message the run row records.
    fn recorded(&self) -> (FailureTag, String) {
        match self {
            Close::Failed(f) => (f.tag, f.to_string()),
            Close::Refused(RunError::StepFailed { failure, .. }) => (failure.tag, self.message()),
            Close::Refused(_) => (FailureTag::Permanent, self.message()),
        }
    }

    fn message(&self) -> String {
        match self {
            Close::Failed(f) => f.to_string(),
            Close::Refused(e) => e.to_string(),
        }
    }
}

/// A fresh opaque execution id.
fn fresh_id() -> Result<String, Failure> {
    let mut bytes = [0u8; 12];
    getrandom::fill(&mut bytes).map_err(|e| Failure::new(FailureTag::Storage, format!("minting an execution id: {e}")))?;
    Ok(format!("x-{}", bytes.iter().map(|b| format!("{b:02x}")).collect::<String>()))
}

fn json_bytes(v: &Option<Value>) -> Vec<u8> {
    serde_json::to_vec(v).unwrap_or_default()
}

/// A seed for the retry jitter, fixed per run.
fn seed(run_id: &str) -> u64 {
    let hex = sha256_hex(run_id.as_bytes());
    u64::from_str_radix(&hex[..16], 16).unwrap_or_default()
}

impl Engine {
    /// Mark `partial_failure` every non-terminal run whose owner lease has expired; a run
    /// a live process holds is left alone. Returns the reaped run ids.
    pub fn reap_orphans(&self) -> Result<Vec<String>, Failure> {
        let now = self.catalog.now()?;
        let mut reaped = Vec::new();
        for row in self.catalog.runs(None)? {
            if row.reaped(now).is_none() {
                continue;
            }
            let updated = self.catalog.update_run(&row.run_id, &mut |r| {
                if let Some(status) = r.reaped(now) {
                    r.status = status;
                    r.ended_at = Some(now);
                    r.error_kind = Some(FailureTag::Transient);
                    r.error_message = Some(format!("the owner lease lapsed at {}", r.owner.as_ref().map(|o| o.lease_expires_at.to_string()).unwrap_or_default()));
                    r.owner = None;
                }
                Ok(())
            })?;
            if updated.is_some() {
                reaped.push(row.run_id);
            }
        }
        Ok(reaped)
    }

    /// Whether the run holding a journal claim is alive: in flight under an unexpired owner lease.
    fn holder_live(&self, run_id: &str) -> Result<bool, Failure> {
        let now = self.catalog.now()?;
        Ok(match self.catalog.run(run_id)? {
            Some(row) => row.status.is_in_flight() && row.owner.as_ref().is_some_and(|o| !o.expired(now)),
            None => false,
        })
    }

    /// At run open, a commit marker newer than the catalog's cached position retires the
    /// pending owner that produced it, before any replay.
    pub fn reconcile(&self, pipeline_id: &str, table: &str, dest: &dyn Destination) -> Result<(), Failure> {
        let Some(marker) = dest.newest_marker(pipeline_id, table)? else { return Ok(()) };
        let cached = self.catalog.cursor(pipeline_id, table)?;
        let newer = cached.marker_run_id.as_deref() != Some(marker.run_id.as_str())
            && cached.marker_committed_at.is_none_or(|c| marker.committed_at > c);
        if !newer {
            return Ok(());
        }
        let producer = self.catalog.owner(pipeline_id, table)?.filter(|o| o.produced(&marker.run_id)).map(|o| o.execution_id);
        let cursor = CursorRow {
            position: marker.cursor.clone(),
            version: 0,
            marker_run_id: Some(marker.run_id.clone()),
            marker_committed_at: Some(marker.committed_at),
        };
        // A version moved by a concurrent writer means that writer reconciled or committed first.
        self.catalog.retire(pipeline_id, table, producer.as_deref().unwrap_or_default(), Some((cursor, cached.version)), None)?;
        if let Some(execution_id) = producer {
            self.journal.collect(&execution_id)?;
        }
        Ok(())
    }

    /// Execute one run. A run that opened closes on a status its row records; the row is
    /// returned whatever that status. A run that cannot open refuses with no row.
    pub fn run(&self, spec: &RunSpec, source: &mut dyn Source, dest: &mut dyn Destination) -> Result<RunRow, EngineError> {
        self.run_with(spec, source, &Unshaped, dest)
    }

    /// Execute one run, passing each recorded batch through `shape` before it lands.
    pub fn run_with(&self, spec: &RunSpec, source: &mut dyn Source, shape: &dyn Shape, dest: &mut dyn Destination) -> Result<RunRow, EngineError> {
        let plan = &spec.plan;
        plan.validate()?;
        if plan.spec.connector.world != NATIVE_WORLD {
            return Err(TopologyError::ComponentHostMissing(format!(
                "connector `{}` implements component world `{}` and this build links no component host; no native source stands in for it",
                plan.spec.connector.id, plan.spec.connector.world
            ))
            .into());
        }
        if self.catalog.run(&spec.run_id)?.is_some() {
            return Err(RunError::Invalid(format!("run `{}` is already recorded; a run id names one attempt", spec.run_id)).into());
        }
        let (pipeline_id, table) = (plan.spec.pipeline.as_str(), plan.spec.table.as_str());
        self.reconcile(pipeline_id, table, dest)?;
        let now = self.catalog.now()?;
        let pending = self.catalog.owner(pipeline_id, table)?;
        let execution_id = match &pending {
            Some(o) => o.execution_id.clone(),
            None => fresh_id()?,
        };

        // The row is written at open, before the first pull.
        let mut row = RunRow {
            run_id: spec.run_id.clone(),
            pipeline_id: pipeline_id.to_string(),
            table: table.to_string(),
            site_id: spec.site_id.clone(),
            status: RunStatus::Running,
            owner: Some(Owner::leased(spec.pid, spec.boot_id.clone(), now)),
            started_at: now,
            ended_at: None,
            rows: 0,
            bytes: 0,
            batches: 0,
            error_kind: None,
            error_message: None,
            connector_id: spec.connector.id.clone(),
            connector_version: spec.connector.version.clone(),
            connector_hash: spec.connector.hash.clone(),
            trace_id: spec.trace_id.clone(),
            phase: Phase::Plan,
            execution_id: execution_id.clone(),
            stop: None,
        };
        self.catalog.put_run(&row)?;
        self.emit(spec, Change::Status { status: RunStatus::Running, at: Some(now), error: None });

        let token = CancelToken::default();
        let held: Arc<Mutex<Option<Lease>>> = Arc::default();
        let keeper = Keeper::start_holding(self.catalog.clone(), &spec.run_id, token.clone(), self.cadence, held.clone());
        let mut owned = false;
        let outcome = self.body(spec, &execution_id, pending, &token, &held, &mut owned, source, shape, dest);
        drop(keeper);
        let lease = held.lock().unwrap_or_else(|e| e.into_inner()).take();
        if let Some(l) = &lease {
            self.catalog.release(l)?;
        }

        let now = self.catalog.now()?;
        let closed = self.catalog.update_run(&spec.run_id, &mut |r| {
            r.ended_at = Some(now);
            r.phase = Phase::Commit;
            r.owner = None;
            match &outcome {
                Ok((landed, batches)) => {
                    r.status = RunStatus::Success;
                    r.rows = landed.rows;
                    r.bytes = landed.bytes;
                    r.batches = *batches;
                }
                Err(close) => {
                    let (tag, message) = close.recorded();
                    r.status = if tag == FailureTag::Canceled { RunStatus::Canceled } else { RunStatus::Failed };
                    r.error_kind = Some(tag);
                    r.error_message = Some(cap_error(&message));
                }
            }
            Ok(())
        })?;
        row = match closed {
            Some(Ok(r)) => r,
            _ => row,
        };
        let error = row.error_kind.map(|tag| Failure::new(tag, row.error_message.clone().unwrap_or_default()));
        self.emit(spec, Change::Status { status: row.status, at: row.ended_at, error });
        if row.status != RunStatus::Success && owned && releases(row.status, self.journal.recorded(&execution_id)?) && !self.shared_with_a_live_attempt(pipeline_id, table, &execution_id, &spec.run_id)? {
            self.catalog.retire(pipeline_id, table, &execution_id, None, None)?;
            self.journal.collect(&execution_id)?;
        }
        Ok(row)
    }

    /// Emit one projection event; emission never blocks and is no journal step.
    fn emit(&self, spec: &RunSpec, change: Change) {
        if let Some(e) = &self.emitter {
            e.emit(&spec.run_id, &spec.plan.spec.pipeline, change);
        }
    }

    /// Whether the pending owner of the table holds `execution_id` under another attempt
    /// still in flight on a live owner lease; such an execution is never retired or collected.
    fn shared_with_a_live_attempt(&self, pipeline_id: &str, table: &str, execution_id: &str, run_id: &str) -> Result<bool, Failure> {
        let Some(owner) = self.catalog.owner(pipeline_id, table)? else { return Ok(false) };
        if owner.execution_id != execution_id {
            return Ok(true);
        }
        for attempt in owner.attempts.iter().filter(|a| a.as_str() != run_id) {
            if self.holder_live(attempt)? {
                return Ok(true);
            }
        }
        Ok(false)
    }

    #[allow(clippy::too_many_arguments)]
    fn body(
        &self,
        spec: &RunSpec,
        execution_id: &str,
        pending: Option<ExecutionOwner>,
        token: &CancelToken,
        held: &Arc<Mutex<Option<Lease>>>,
        owned: &mut bool,
        source: &mut dyn Source,
        shape: &dyn Shape,
        dest: &mut dyn Destination,
    ) -> Result<(Landed, u64), Close> {
        let plan = &spec.plan;
        let (pipeline_id, table) = (plan.spec.pipeline.as_str(), plan.spec.table.as_str());

        // The single-writer lease comes first: a run that cannot take it touches no owner.
        if plan.cursor_kind.single_writer() {
            let key = LeaseKey::Pipeline(format!("{pipeline_id}/{table}"));
            let lease = self.catalog.acquire(&key, &spec.run_id, OWNER_LEASE_TTL_SECS)?.ok_or_else(|| {
                Failure::new(FailureTag::Transient, format!("another run holds the `{}` cursor lease of `{pipeline_id}`/`{table}`", plan.cursor_kind.name()))
            })?;
            let fence = lease.fence;
            // The lease joins the slot first, so a refused fence record still releases it.
            *held.lock().unwrap_or_else(|e| e.into_inner()) = Some(lease);
            dest.open_fence(pipeline_id, table, fence)?;
        }

        let cached = self.catalog.cursor(pipeline_id, table)?;
        let pins = Pins { connector: spec.connector.clone(), content_hash: plan.content_hash.clone(), input_hash: sha256_hex(&json_bytes(&cached.position)) };
        let mut owner = match pending {
            Some(owner) => {
                owner.check_pins(&pins)?;
                owner
            }
            None => ExecutionOwner {
                execution_id: execution_id.to_string(),
                pipeline_id: pipeline_id.to_string(),
                table: table.to_string(),
                pins,
                attempts: Vec::new(),
                opened_at: self.catalog.now()?,
            },
        };
        owner.attempts.push(spec.run_id.clone());
        self.catalog.put_owner(&owner)?;
        *owned = true;

        let field = plan.spec.cursor.field.clone().unwrap_or_default();
        let mut position = cached.position.clone();
        let mut at = match plan.cursor_kind {
            CursorKind::Monotonic => open_watermark(position.as_ref(), &field)?.cloned(),
            _ => None,
        };
        let mut batches: Vec<Vec<Row>> = Vec::new();
        for ordinal in 0.. {
            if token.requested() {
                return Err(Close::Failed(Failure::canceled("stopped between pulls")));
            }
            let label = format!("pull-{ordinal}");
            let key = EntryKey::new(execution_id, &label, &json_bytes(&position));
            let request = PullRequest { step_label: label.clone(), position: position.clone(), idempotency_key: key.idempotency_key() };
            let resolved = self.step(spec, &key, &request, token, source)?;
            let pull = Pull::decode(resolved.bytes())?;
            let (rows, last) = match plan.cursor_kind {
                CursorKind::Monotonic => {
                    // The frontier counts every fetched row; the load admits those at or after the stored position.
                    let f = frontier(&field, &pull.rows)?;
                    let next = advance(at.as_ref(), f.as_ref())?;
                    let mut admitted = Vec::new();
                    for r in pull.rows {
                        if let Some(v) = r.get(&field) {
                            if admits(at.as_ref(), v)? {
                                admitted.push(r);
                            }
                        }
                    }
                    let advanced = next != at;
                    at = next;
                    position = at.clone().map(|v| watermark(&field, v));
                    (admitted, !(pull.more && advanced))
                }
                _ => {
                    if let Some(c) = pull.cursor {
                        position = Some(c);
                    }
                    (pull.rows, !pull.more)
                }
            };
            let rows = shape.shape(rows)?;
            if !rows.is_empty() {
                batches.push(rows);
            }
            if last {
                break;
            }
        }

        // The land path carries no stop check. Under a lease, the commit point re-reads the
        // lease: a writer a later acquisition fenced out lands nothing.
        let lease = held.lock().unwrap_or_else(|e| e.into_inner()).clone();
        let moved = position != cached.position;
        let batch_count = batches.len() as u64;
        let committed_at = self.catalog.now()?;
        let landed = if batch_count > 0 || moved {
            let precommit = || -> Result<(), Failure> {
                let Some(l) = held.lock().unwrap_or_else(|e| e.into_inner()).clone() else { return Ok(()) };
                if self.catalog.lease_holds(&l)? {
                    Ok(())
                } else {
                    Err(Failure::new(
                        FailureTag::Storage,
                        contextful_core::store::StoreError::LeaseFenced(format!("lease `{}` at fence {} no longer holds; the commit lands nothing", l.key, l.fence)).to_string(),
                    ))
                }
            };
            dest.land(
                Commit {
                    pipeline_id: pipeline_id.to_string(),
                    table: table.to_string(),
                    run_id: spec.run_id.clone(),
                    site_id: spec.site_id.clone(),
                    batches,
                    cursor: position.clone(),
                    committed_at,
                    fence: lease.as_ref().map(|l| l.fence),
                },
                &precommit,
            )?
        } else {
            Landed::default()
        };
        let committed = batch_count > 0 || moved;
        let mut next = CursorRow {
            position,
            version: 0,
            marker_run_id: if committed { Some(spec.run_id.clone()) } else { cached.marker_run_id.clone() },
            marker_committed_at: if committed { Some(committed_at) } else { cached.marker_committed_at },
        };
        let mut expected = cached.version;
        loop {
            match self.catalog.retire(pipeline_id, table, execution_id, Some((next.clone(), expected)), lease.as_ref())? {
                Cas::Applied => break,
                Cas::Fenced(e) => return Err(Close::Failed(Failure::new(FailureTag::Storage, e.to_string()))),
                Cas::VersionMoved if plan.cursor_kind == CursorKind::Monotonic => {
                    // A concurrent monotonic writer committed first: the highest position wins.
                    let current = self.catalog.cursor(pipeline_id, table)?;
                    if let (Some(theirs), Some(ours)) = (&current.position, &next.position) {
                        if resolve_concurrent(CursorKind::Monotonic, theirs, ours)? == *theirs && theirs != ours {
                            next = CursorRow { version: 0, ..current.clone() };
                        }
                    } else if next.position.is_none() {
                        next = CursorRow { version: 0, ..current.clone() };
                    }
                    expected = current.version;
                }
                Cas::VersionMoved => return Err(Close::Failed(Failure::new(FailureTag::Storage, "the cursor row moved under the retiring run's lease"))),
            }
        }
        self.journal.collect(execution_id)?;
        Ok((landed, batch_count))
    }

    /// Resolve one pull through the journal under the plan's retry schedule.
    fn step(&self, spec: &RunSpec, key: &EntryKey, request: &PullRequest, token: &CancelToken, source: &mut dyn Source) -> Result<Resolved, Close> {
        let journaled = spec.plan.spec.journal;
        // An empty pull is never journaled.
        let journal_it = |bytes: &[u8]| journaled && Pull::decode(bytes).is_ok_and(|p| !p.rows.is_empty());
        let holder_live = |run: &str| self.holder_live(run);
        let mut attempt = 1;
        let label = request.step_label.as_str();
        loop {
            self.emit(spec, Change::Step(StepPatch { attempts: Some(attempt), ..StepPatch::new(label).status(StepStatus::Running) }));
            let failure = match self.journal.step(key, &spec.run_id, &holder_live, token, &journal_it, &mut || source.pull(request, token)) {
                Ok(r) => {
                    self.emit(spec, Change::Step(StepPatch::new(label).status(StepStatus::Completed)));
                    return Ok(r);
                }
                Err(StepError::Failed(f)) => f,
                Err(e) => return Err(e.into()),
            };
            if failure.tag == FailureTag::Canceled {
                return Err(Close::Failed(failure));
            }
            match decide(&spec.plan.schedule, label, attempt, &failure, seed(&spec.run_id)) {
                Decision::Retry { delay_ms } => {
                    self.emit(spec, Change::Step(StepPatch { failure: Some(failure.clone()), ..StepPatch::new(label).status(StepStatus::Retrying) }));
                    if !sleep_unless(Duration::from_millis(delay_ms), &|| token.requested()) {
                        return Err(Close::Failed(Failure::canceled(format!("stopped during the retry sleep of `{label}`"))));
                    }
                    attempt += 1;
                }
                Decision::Fail { error, .. } | Decision::Reschedule { error, .. } => {
                    self.emit(spec, Change::Step(StepPatch { failure: Some(failure), ..StepPatch::new(label).status(StepStatus::Failed) }));
                    return Err(Close::Refused(error));
                }
            }
        }
    }

    /// Write a stop onto a run row, and under `pipeline` scope onto every in-flight run of
    /// its pipeline. Returns the marked run ids.
    pub fn cancel(&self, run_id: &str, scope: Scope, reason: Option<String>) -> Result<Vec<String>, EngineError> {
        let now = self.catalog.now()?;
        let not_found = || RunError::CancelTargetNotInFlight(format!("no run `{run_id}` is pending, running or waiting"));
        let target = match self.catalog.update_run(run_id, &mut |r| mark(r, scope, reason.clone(), now))? {
            None => return Err(not_found().into()),
            Some(r) => r?,
        };
        let mut marked = vec![target.run_id.clone()];
        if scope == Scope::Pipeline {
            for other in self.catalog.runs(Some(&target.pipeline_id))? {
                if other.run_id != target.run_id && other.status.is_in_flight() {
                    if let Some(Ok(_)) = self.catalog.update_run(&other.run_id, &mut |r| mark(r, scope, reason.clone(), now))? {
                        marked.push(other.run_id);
                    }
                }
            }
        }
        Ok(marked)
    }

    /// A pipeline's history through `window`, newest first.
    pub fn history(&self, pipeline_id: Option<&str>, window: &Window) -> Result<HistoryPage, Failure> {
        Ok(select_history(self.catalog.runs(pipeline_id)?, window))
    }
}
