//! The runner: a client of the execution handle. It opens an execution for a table
//! against its pinned plan, resolves each pull through the secret guard and the journal
//! under the step's retry schedule, stages each batch before the next pull, commits the
//! staged parts in one commit carrying the position, retires the owner, and closes the run row.

use crate::cancel::Keeper;
use crate::guard::{log_counts, Guarded};
use crate::execution::{Close, Execution, Tally};
use crate::journal::{Journal, Resolved};
use crate::project::Emitter;
use crate::stores::{FileBlobStore, FileJournalStore};
use contextful_core::coordinate::{Cas, Catalog, CursorRow, LeaseKey};
use contextful_core::run::advance::{admits, advance, clock, compare, frontier, open_watermark, resolve_concurrent, rewind, watermark, CursorKind};
use contextful_core::run::cancel::{mark, same_grain, Scope};
use contextful_core::run::journal::EntryKey;
use contextful_core::run::own::{admission_pin, resolve_pinned, segment_run_id, Artifacts, ConnectorPin, OwnerScope, Pins};
use contextful_core::run::plan::Plan;
use contextful_core::run::ports::{
    AwakeableStore, BlobStore, Cancellation, Commit, DeadLetter, Destination, JournalStore, Landed, OpenExecution, Part, Pull, PullRequest, Shape, Source, Stage, Types, Unshaped,
    STAGED_BYTES_PER_RUN,
};
use contextful_core::run::record::{HistoryPage, RunRow, RunStatus, Window, OWNER_LEASE_TTL_SECS};
use contextful_core::run::{Failure, FailureTag, RunError};
use contextful_core::store::reconcile::ColumnType;
use contextful_core::topology::TopologyError;
use serde_json::Value;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

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

/// The field of a member key's receive-scope position naming the digest of the group it
/// landed in (`run.land.receive-group`).
pub const RECEIVE_GROUP_FIELD: &str = "receive_group";

/// The field of a queued batch key's receive-scope position holding its recorded outcome
/// (`run.land.receive-queued-outcome`).
pub const RECEIVE_OUTCOME_FIELD: &str = "queued_outcome";

/// The run-id stem of batch `key`'s `table` in pipeline `pipeline_id`: `receive-<digest>.<table>`.
pub fn receive_stem(pipeline_id: &str, table: &str, key: &str) -> String {
    format!("receive-{}.{table}", contextful_core::run::own::receive_digest(pipeline_id, key))
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

/// The engine: the catalog behind its port, the journal over its row and blob stores,
/// the awakeable store when one is wired, and the one keeper every open execution
/// registers with.
#[derive(Clone)]
pub struct Engine<J = FileJournalStore, B = FileBlobStore> {
    pub catalog: Arc<dyn Catalog + Send + Sync>,
    pub journal: Journal<J, B>,
    /// Where awakeables persist; `None` leaves suspension unwired.
    pub awakeables: Option<Arc<dyn AwakeableStore>>,
    /// Renews every open execution's leases and feeds its token; clones share it.
    pub keeper: Keeper,
    /// The live projection's emitter; every event follows the durable change it reports.
    pub emitter: Option<Emitter>,
    /// The component worlds a linked component host answers, beside the native one; empty
    /// on a build linking no host (`topology.package.component-host`).
    pub worlds: Vec<String>,
}

fn json_bytes(v: &Option<Value>) -> Vec<u8> {
    serde_json::to_vec(v).unwrap_or_default()
}

/// Internal journal data, never a source Pull wire value.
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct RecordedPull {
    authority: String,
    prepared: Value,
    rows: u64,
    /// Rows that entered the transform chain; absent on a pull recorded before the count.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    fetched: Option<u64>,
    columns: BTreeSet<String>,
    types: BTreeMap<String, String>,
    next: Option<Value>,
    last: bool,
    more: bool,
    snapshot_complete: Option<bool>,
    skipped: u64,
    declined: BTreeMap<String, u64>,
    #[serde(default)]
    audit: Vec<String>,
    /// The position a monotonic walk's next pull asks (`run.advance.walk-position`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    walk: Option<Value>,
    /// Whether every fetched clock passed the frontier the pull opened on.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    strict: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    dead_letters: Vec<DeadLetter>,
}

/// Bytes a run stages before it commits a checkpoint and keeps pulling: 512 MiB
/// (`run.own.checkpoint-commit`).
pub const CHECKPOINT_BYTES: u64 = 512 * 1024 * 1024;

/// Whether `marker_run_id` names a commit of attempt `run_id`: its first or a later checkpoint's.
fn segment_of(marker_run_id: &str, run_id: &str) -> bool {
    marker_run_id == run_id || marker_run_id.strip_prefix(run_id).and_then(|rest| rest.strip_prefix('.')).is_some_and(|n| n.len() == 6 && n.bytes().all(|b| b.is_ascii_digit()))
}

/// Whether every clock of `rows` lies strictly past `at`: a pull at a strict frontier
/// advance, where no row shares the frontier's instant (`run.own.checkpoint-frontier`).
fn strictly_past(field: &str, rows: &[contextful_core::run::ports::Row], at: Option<&Value>) -> Result<bool, RunError> {
    if rows.is_empty() {
        return Ok(false);
    }
    let Some(at) = at else { return Ok(true) };
    for row in rows {
        match clock(row, field) {
            Some(v) if compare(v, at)? == std::cmp::Ordering::Greater => {}
            _ => return Ok(false),
        }
    }
    Ok(true)
}

fn recorded(bytes: &[u8]) -> Result<RecordedPull, Failure> {
    serde_json::from_slice(bytes).map_err(|e| Failure::deterministic(FailureTag::SchemaIncompatible, format!("prepared journal pull is malformed: {e}")))
}

/// One pull inside its journal step: the source settles the pull's outbound requests into
/// its run's request ledger before the step returns, so the ledger is durable before the
/// entry recording the batch commits (`run.journal.ledger-settles-first`). A failed pull
/// settles too, and its failure answers first.
fn pulled(source: &mut dyn Source, request: &PullRequest, cancel: &dyn Cancellation) -> Result<Vec<u8>, Failure> {
    let bytes = source.pull(request, cancel);
    let settled = source.settle();
    let bytes = bytes?;
    settled?;
    Ok(bytes)
}

fn preparation_failure(error: RunError) -> Failure { Failure::deterministic(FailureTag::Permanent, error.to_string()) }

enum TypeRefusal { Conflict(Failure), Late(Failure) }

impl TypeRefusal {
    fn failure(self) -> Failure { match self { Self::Conflict(failure) | Self::Late(failure) => failure } }
}

fn admit_types(incoming: &Types, held: &Types, undeclared: &BTreeSet<String>, ordinal: usize, run_id: &str) -> Result<(), TypeRefusal> {
    for (column, ty) in incoming {
        match held.get(column) {
            Some(existing) if existing != ty => return Err(TypeRefusal::Conflict(Failure::deterministic(FailureTag::SchemaIncompatible, format!("StoreSchemaIncompatible: column `{column}` is declared {} and {} by two pulls of one run", existing.name(), ty.name())))),
            None if undeclared.contains(column) => return Err(TypeRefusal::Late(Failure::deterministic(FailureTag::SchemaIncompatible, format!("PipelineTypeDeclaredLate: pull {ordinal} declares column `{column}` as {}, which an earlier staged batch of run `{run_id}` carried undeclared; declare it from the first pull", ty.name())))),
            _ => {}
        }
    }
    Ok(())
}

struct PullStep<'a> {
    parent: Option<&'a crate::drive::BodyOwner>,
    spec: &'a RunSpec,
    key: &'a EntryKey,
    request: &'a PullRequest,
    shape: &'a dyn Shape,
    authority: Option<&'a str>,
    at: Option<&'a Value>,
    /// The clock value the pull admits rows from: `at` less any allowed lateness.
    floor: Option<&'a Value>,
    types: &'a Types,
    undeclared: &'a BTreeSet<String>,
    ordinal: usize,
}

struct EmissionInput<'a> {
    owner: &'a crate::drive::BodyOwner,
    frames: &'a [contextful_core::run::effect::PreparedEmission],
}

enum BodyInput<'a> {
    Source { authority: Option<&'a str>, parent:Option<&'a crate::drive::BodyOwner> },
    Effects(&'a EmissionInput<'a>),
}

impl<J: JournalStore, B: BlobStore> Engine<J, B> {
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
    pub(crate) fn holder_live(&self, run_id: &str) -> Result<bool, Failure> {
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
        let producer = self
            .catalog
            .owner(pipeline_id, table)?
            .filter(|o| o.produced(&marker.run_id) || o.attempts.iter().any(|a| segment_of(&marker.run_id, a)))
            .map(|o| o.execution_id);
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

    /// Admit the connector a run of `pipeline_id`'s `table` executes and resolve its
    /// artifact by pin: a pending owner's recorded build, so the replay runs the bytes it
    /// started on, else `admitted`. The answered pin is the [`RunSpec::connector`] the run
    /// carries, so a connector rebuilt later reaches no in-flight or replayed run
    /// (`run.own.admission-pin`).
    pub fn admit_connector(&self, pipeline_id: &str, table: &str, admitted: &ConnectorPin, artifacts: &dyn Artifacts) -> Result<(ConnectorPin, Vec<u8>), Failure> {
        let pending = self.catalog.owner(pipeline_id, table)?;
        let pin = admission_pin(pending.as_ref(), admitted).clone();
        let bytes = resolve_pinned(&pin, artifacts)?;
        Ok((pin, bytes))
    }

    /// Execute one run. A run that opened closes on a status its row records; the row is
    /// returned whatever that status. A run that cannot open refuses with no row.
    pub fn run(&self, spec: &RunSpec, source: &mut dyn Source, dest: &mut dyn Destination) -> Result<RunRow, EngineError> {
        self.run_with(spec, source, &Unshaped, dest)
    }

    /// Execute one run, passing each recorded batch through `shape` before it lands.
    pub fn run_with(&self, spec: &RunSpec, source: &mut dyn Source, shape: &dyn Shape, dest: &mut dyn Destination) -> Result<RunRow, EngineError> {
        self.run_input(spec, None, source, shape, dest, None, None)
    }

    /// Execute one run of a delivered batch under the owner scope its batch `key` fixes, so a
    /// resend under the key resumes that owner's recorded pull and no other key's
    /// (`run.land.receive-batch-key`).
    pub fn run_receive(&self, spec: &RunSpec, key: &str, source: &mut dyn Source, shape: &dyn Shape, dest: &mut dyn Destination) -> Result<RunRow, EngineError> {
        let scope = OwnerScope::receive(&spec.plan.spec.pipeline, &spec.plan.spec.table, key);
        self.run_input(spec, Some(scope), source, shape, dest, None, None)
    }

    /// Execute one run landing a group of delivered batches under the scope `group_key`
    /// fixes. Before the run opens, each `members` key's own scope records the group's
    /// digest, so a resend under any member replays the group run once it commits and
    /// lands again while it has not (`run.land.receive-batch-key`, `run.land.receive-group`).
    pub fn run_receive_group(&self, spec: &RunSpec, group_key: &str, members: &[String], source: &mut dyn Source, shape: &dyn Shape, dest: &mut dyn Destination) -> Result<RunRow, EngineError> {
        let (pipeline_id, table) = (spec.plan.spec.pipeline.as_str(), spec.plan.spec.table.as_str());
        let digest = contextful_core::run::own::receive_digest(pipeline_id, group_key);
        for member in members {
            self.name_receive_group(&OwnerScope::receive(pipeline_id, table, member), &digest)?;
        }
        self.run_receive(spec, group_key, source, shape, dest)
    }

    /// The digest of the group a batch `key` of `pipeline_id` landed `table` in, as its
    /// scope records it; `None` for a key that never joined a group.
    pub fn receive_group(&self, pipeline_id: &str, table: &str, key: &str) -> Result<Option<String>, Failure> {
        let cursor = self.catalog.cursor_at(&OwnerScope::receive(pipeline_id, table, key))?;
        Ok(cursor.position.as_ref().and_then(|p| p.get(RECEIVE_GROUP_FIELD)).and_then(Value::as_str).map(str::to_owned))
    }

    /// The run of `stem` that committed, or the id the next attempt takes: attempts are
    /// `stem`, `stem.2`, `stem.3`, … since a run id names one attempt.
    pub fn receive_attempt(&self, stem: &str) -> Result<Result<RunRow, String>, Failure> {
        for n in 1u32.. {
            let id = if n == 1 { stem.to_string() } else { format!("{stem}.{n}") };
            match self.catalog.run(&id)? {
                Some(row) if row.status == RunStatus::Success => return Ok(Ok(row)),
                Some(_) => continue,
                None => return Ok(Err(id)),
            }
        }
        unreachable!("the attempt numbers are unbounded")
    }

    /// The run that committed batch `key`'s `table`, under the key's own stem or under the
    /// group its scope names, or the id the key's next attempt takes
    /// (`run.land.receive-batch-key`).
    pub fn receive_landed(&self, pipeline_id: &str, table: &str, key: &str) -> Result<Result<RunRow, String>, Failure> {
        let next = match self.receive_attempt(&receive_stem(pipeline_id, table, key))? {
            Ok(row) => return Ok(Ok(row)),
            Err(next) => next,
        };
        if let Some(digest) = self.receive_group(pipeline_id, table, key)? {
            if let Ok(row) = self.receive_attempt(&format!("receive-{digest}.{table}"))? {
                return Ok(Ok(row));
            }
        }
        Ok(Err(next))
    }

    /// The first run that opened for batch `key`'s `table`: the key's own first attempt, or
    /// the first attempt of the group its scope names (`run.land.receive-key-expired`).
    pub fn receive_first(&self, pipeline_id: &str, table: &str, key: &str) -> Result<Option<RunRow>, Failure> {
        if let Some(row) = self.catalog.run(&receive_stem(pipeline_id, table, key))? {
            return Ok(Some(row));
        }
        match self.receive_group(pipeline_id, table, key)? {
            Some(digest) => self.catalog.run(&format!("receive-{digest}.{table}")),
            None => Ok(None),
        }
    }

    /// Close attempt `run_id` of the receive scope `digest` names over `table` — a batch key's or
    /// a group's — whose commit the store holds with `rows` while its run row never closed:
    /// the scope's pending owner that produced it retires with its journal, and the row closes
    /// `Success` carrying `rows`, so the attempt replays and no later attempt lands its rows
    /// again (`run.land.receive-torn`).
    pub fn receive_settle(&self, pipeline_id: &str, table: &str, digest: &str, run_id: &str, rows: u64) -> Result<RunRow, Failure> {
        let scope = OwnerScope::table(pipeline_id, &format!("{table}{}{digest}", contextful_core::run::own::RECEIVE_INFIX));
        if let Some(owner) = self.catalog.owner_at(&scope)?.filter(|o| o.produced(run_id)) {
            self.catalog.retire_at(&scope, &owner.execution_id, None, None)?;
            self.journal.collect(&owner.execution_id)?;
        }
        let now = self.catalog.now()?;
        let closed = self.catalog.update_run(run_id, &mut |r| {
            r.status = RunStatus::Success;
            r.phase = contextful_core::run::record::Phase::Commit;
            r.ended_at = Some(now);
            r.owner = None;
            r.rows = rows;
            Ok(())
        })?;
        match closed {
            Some(Ok(row)) => Ok(row),
            Some(Err(e)) => Err(Failure::deterministic(FailureTag::Permanent, e.to_string())),
            None => Err(Failure::new(FailureTag::Storage, format!("no run `{run_id}` is recorded to settle"))),
        }
    }

    /// Record `digest` as the group of a member's receive `scope`: one conditional write
    /// against a fresh scope; once its version moved, a read and a retry that keeps every
    /// other field of the recorded position.
    fn name_receive_group(&self, scope: &OwnerScope, digest: &str) -> Result<(), Failure> {
        self.set_receive_field(scope, RECEIVE_GROUP_FIELD, Value::String(digest.to_string()))
    }

    /// Record a queued batch `key`'s outcome on its receive scope over `table`, the first
    /// table its pipeline declares (`run.land.receive-queued-outcome`).
    pub fn receive_record_outcome(&self, pipeline_id: &str, table: &str, key: &str, outcome: Value) -> Result<(), Failure> {
        self.set_receive_field(&OwnerScope::receive(pipeline_id, table, key), RECEIVE_OUTCOME_FIELD, outcome)
    }

    /// The outcome [`Engine::receive_record_outcome`] recorded for batch `key`; `None` while
    /// none has.
    pub fn receive_outcome(&self, pipeline_id: &str, table: &str, key: &str) -> Result<Option<Value>, Failure> {
        let cursor = self.catalog.cursor_at(&OwnerScope::receive(pipeline_id, table, key))?;
        Ok(cursor.position.as_ref().and_then(|p| p.get(RECEIVE_OUTCOME_FIELD)).cloned())
    }

    /// Set `field` of a receive `scope`'s position to `value`: one conditional write against
    /// a fresh scope; once its version moved, a read and a retry that keeps every other field
    /// of the recorded position.
    fn set_receive_field(&self, scope: &OwnerScope, field: &str, value: Value) -> Result<(), Failure> {
        let mut held = CursorRow::default();
        for _ in 0..8 {
            let mut position = match held.position.clone() {
                Some(Value::Object(map)) => map,
                _ => serde_json::Map::new(),
            };
            position.insert(field.to_string(), value.clone());
            let next = CursorRow { position: Some(Value::Object(position)), version: held.version, ..held.clone() };
            match self.catalog.cursor_cas_at(scope, held.version, next, None)? {
                Cas::Applied => return Ok(()),
                Cas::VersionMoved => held = self.catalog.cursor_at(scope)?,
                Cas::Fenced(e) => return Err(Failure::new(FailureTag::Transient, e.to_string())),
            }
        }
        Err(Failure::new(FailureTag::Transient, format!("the receive scope {scope} kept moving while its `{field}` was recorded")))
    }

    /// Execute one run under `scope` in place of the plan's table scope: a seed chunk runs
    /// the one land path while its owner, cursor and chunk row stay under `<table>#seed`.
    pub(crate) fn run_scoped(&self, spec: &RunSpec, scope: OwnerScope, source: &mut dyn Source, shape: &dyn Shape, dest: &mut dyn Destination) -> Result<RunRow, EngineError> {
        self.run_input(spec, Some(scope), source, shape, dest, None, None)
    }

    /// Raw item rows retain their originating owner's entity fence through commit.
    pub fn run_body_rows(&self,spec:&RunSpec,owner:&crate::drive::BodyOwner,source:&mut dyn Source,dest:&mut dyn Destination)->Result<RunRow,EngineError> {
        self.validate_body_owner(owner)?;
        self.run_input(spec,None,source,&Unshaped,dest,None,Some(owner))
    }

    /// Land admitted body groups through the ordinary lease/stage/commit/discard machine.
    /// A live Drive context and the canonical destination both re-admit every handle.
    pub fn run_emissions(&self, spec: &RunSpec, owner: &crate::drive::BodyOwner, emissions: &[contextful_core::run::effect::PreparedEmission], dest: &mut dyn Destination) -> Result<RunRow, EngineError> {
        if spec.plan.spec.journal || spec.plan.cursor_kind != CursorKind::OpaqueToken {
            return Err(RunError::Invalid("body emissions land without a second journal or source cursor".into()).into());
        }
        self.validate_body_owner(owner)?;
        for emission in emissions {
            if emission.table() != spec.plan.spec.table || emission.scope().key().execution_id != owner.execution_id || emission.scope().plan_identity() != owner.identity {
                return Err(RunError::Invalid("prepared body result belongs to another output or owner".into()).into());
            }
            emission.admit_to(dest, owner.expected(emission.scope().key())?)?;
        }
        self.validate_body_owner(owner)?;
        struct NoSource;
        impl Source for NoSource {
            fn pull(&mut self, _: &PullRequest, _: &dyn Cancellation) -> Result<Vec<u8>, Failure> {
                Err(Failure::deterministic(FailureTag::Permanent, "prepared body landing has no source"))
            }
        }
        self.run_input(spec, None, &mut NoSource, &Unshaped, dest, Some(&EmissionInput { owner, frames:emissions }),Some(owner))
    }

    pub(crate) fn validate_body_owner(&self, owner: &crate::drive::BodyOwner) -> Result<(), Failure> {
        self.validate_body_attempt(owner,true)
    }

    fn validate_body_publication(&self, owner:&crate::drive::BodyOwner)->Result<(),Failure> {
        self.validate_body_attempt(owner,false)
    }

    fn validate_body_attempt(&self, owner:&crate::drive::BodyOwner,admit_effect:bool)->Result<(),Failure> {
        if let Some(lease)=&owner.fence { if !self.catalog.lease_holds(lease)? { return Err(Failure::new(FailureTag::Transient,"body entity fence was superseded")); } }
        let active = self.catalog.owner_at(&owner.scope)?.ok_or_else(|| Failure::deterministic(FailureTag::Permanent, "prepared body owner is absent"))?;
        if active.execution_id != owner.execution_id || active.pins.plan_ref() != owner.identity || active.attempts.last() != Some(&owner.run_id) {
            return Err(Failure::deterministic(FailureTag::Permanent, "prepared body owner or pinned authority changed"));
        }
        let run = self.catalog.run(&owner.run_id)?.ok_or_else(|| Failure::deterministic(FailureTag::Permanent, "prepared body attempt is absent"))?;
        if run.execution_id != owner.execution_id {
            return Err(Failure::deterministic(FailureTag::Permanent, "prepared body attempt execution changed"));
        }
        if admit_effect && (run.stop.is_some() || run.status == RunStatus::Canceled) {
            return Err(Failure::canceled("prepared body attempt stopped"));
        }
        if !run.status.is_in_flight() || run.owner.is_none() {
            return Err(Failure::deterministic(FailureTag::Permanent, "prepared body attempt stopped or changed"));
        }
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    fn run_input(&self, spec: &RunSpec, scope: Option<OwnerScope>, source: &mut dyn Source, shape: &dyn Shape, dest: &mut dyn Destination, emissions: Option<&EmissionInput<'_>>,parent:Option<&crate::drive::BodyOwner>) -> Result<RunRow, EngineError> {
        let plan = &spec.plan;
        plan.validate()?;
        if !self.capabilities().hosts(&plan.spec.connector.world) {
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
        let canonical = dest.recording_identity(plan)?;
        if canonical.is_some() && plan.cursor_kind == CursorKind::Monotonic {
            let field = plan.spec.cursor.field.as_deref().unwrap_or_default();
            let columns = shape.recording_clock_columns(field)?.ok_or_else(|| RunError::Invalid("protected clock requires safe typed progress lineage".into()))?;
            dest.validate_recorded_clock(plan, &columns)?;
        }
        let authority = if plan.spec.journal {
            canonical.map(|canonical| {
                let shape = shape.recording_identity()?.ok_or_else(|| RunError::Invalid("protected recording requires a declared shape identity".into()))?;
                let bytes = serde_json::to_vec(&("prepared-pull-owner-v1", &plan.content_hash, canonical, shape)).map_err(|e| RunError::Invalid(e.to_string()))?;
                Ok::<_, EngineError>(contextful_core::run::journal::sha256_hex(&bytes))
            }).transpose()?
        } else { None };
        if plan.spec.journal && !plan.spec.redaction.is_empty() && authority.is_none() {
            return Err(RunError::JournalRedactionConflict("typed removal requires canonical prepared destination admission before recording".into()).into());
        }
        self.reconcile(pipeline_id, table, dest)?;
        let open = OpenExecution {
            scope: scope.unwrap_or_else(|| OwnerScope::table(pipeline_id, table)),
            pins: Pins { connector: spec.connector.clone(), content_hash: authority.clone().unwrap_or_else(|| plan.content_hash.clone()), input_hash:String::new() }.into(),
            run_id: spec.run_id.clone(),
            site_id: spec.site_id.clone(),
            pid: spec.pid,
            boot_id: spec.boot_id.clone(),
            trace_id: spec.trace_id.clone(),
            connector: Some(spec.connector.clone()),
            schedule: plan.schedule.clone(),
        };
        let mut execution = self.begin(&open)?;
        // Every pull passes the secret guard before the journal records it (`run.guard-secrets.placement`).
        let mut source = Guarded { inner: source, report: log_counts };
        let input = match emissions { Some(emissions) => BodyInput::Effects(emissions), None => BodyInput::Source { authority:authority.as_deref(),parent } };
        // The run id the parts staged since the run's last commit carry.
        let mut staging = spec.run_id.clone();
        // What the run's checkpoints landed, which its row records on failure too
        // (`run.own.checkpoint-landed`).
        let mut checkpointed = Landed::default();
        let outcome = self.body(spec, &mut execution, &mut source, shape, dest, input, &mut staging, &mut checkpointed);
        if outcome.is_err() {
            execution.record_checkpointed(checkpointed);
            // Abandoned work, a stop's `Canceled` included, settles the request ledger on
            // this ordinary failure path and leaves the position alone
            // (`run.cancel.abandoned-work`); the run closes on its own failure.
            let _ = source.settle();
            // A run id names one attempt, so no later commit names a failed run's staged
            // parts (`run.own.stage-discard`). The run closes on its own failure; a part the
            // discard leaves joins no file list.
            let _ = dest.discard(&plan.spec.table, &staging);
        }
        execution.record_credentials(source.credential_providers());
        execution.close_with(outcome)
    }

    #[allow(clippy::too_many_arguments)]
    fn body(&self, spec: &RunSpec, execution: &mut Execution<'_, J, B>, source: &mut dyn Source, shape: &dyn Shape, dest: &mut dyn Destination, input: BodyInput<'_>, staging: &mut String, checkpointed: &mut Landed) -> Result<(Landed, Tally), Close> {
        let (authority, emitted,parent) = match input { BodyInput::Source { authority,parent } => (authority, None,parent), BodyInput::Effects(emitted) => (None, Some(emitted),Some(emitted.owner)) };
        let emissions = emitted.map(|input| input.frames);
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
            execution.hold(lease);
            dest.open_fence(pipeline_id, table, fence)?;
        }

        let mut cached = execution.cursor_row()?;
        execution.claim()?;
        let execution_id = execution.execution_id().to_string();

        let field = plan.spec.cursor.field.clone().unwrap_or_default();
        let lateness = plan.spec.cursor.allowed_lateness_secs()?;
        let mut position = cached.position.clone();
        let mut at = match plan.cursor_kind {
            CursorKind::Monotonic => open_watermark(position.as_ref(), &field)?.cloned(),
            _ => None,
        };
        // A live table's ordinary source run commits a checkpoint each time its staged
        // parts reach the bound; a replacement, a body's landing and a seeding or receiving
        // scope commit once (`run.own.checkpoint-commit`, `run.own.checkpoint-scope`).
        let checkpoints = emissions.is_none() && parent.is_none() && !dest.replaces(table) && *execution.scope() == OwnerScope::table(pipeline_id, table);
        let (mut commits, mut committed_batches) = (0u32, 0u64);
        // A monotonic walk's next position and the floor it admits rows from
        // (`run.advance.walk-position`).
        let (mut walk, mut walk_floor): (Option<Value>, Option<Value>) = (None, None);
        // Each shaped batch stages before the next pull, so the run holds one pulled batch
        // in memory; the commit names the staged parts (`run.own.backpressure`).
        let mut parts: Vec<Part> = Vec::new();
        let (mut staged_rows, mut staged_bytes) = (0u64, 0u64);
        let mut types = Types::new();
        let mut skipped = 0u64;
        let (mut fetched, mut kept) = (0u64, 0u64);
        let mut snapshot_complete = false;
        let mut completion_reported = false;
        let mut declined: std::collections::BTreeMap<String, u64> = std::collections::BTreeMap::new();
        let mut audit: Vec<String> = Vec::new();
        // Inputs the source could not read; they land in the dead-letter table with the
        // run's commit (`run.land.dead-letter`).
        let mut dead_letters: Vec<DeadLetter> = Vec::new();
        // Columns a staged batch carried with no declared type: their parts hold the
        // inferred type, so a later declaration cannot retype them (`run.land.late-type`).
        let mut undeclared: BTreeSet<String> = BTreeSet::new();
        for ordinal in 0.. {
            if let Some(owner) = parent { self.validate_body_owner(owner)?; }
            if execution.token().requested() {
                return Err(Close::Failed(Failure::canceled("stopped between pulls")));
            }
            let label = format!("pull-{ordinal}");
            // The poll's first pull re-reads `allowed_lateness` behind the stored position;
            // the committed position itself never rewinds (`run.advance.allowed-lateness`).
            let floor = match (plan.cursor_kind, ordinal, at.as_ref()) {
                (CursorKind::Monotonic, 0, Some(stored)) if lateness > 0 => Some(rewind(stored, lateness)?),
                (CursorKind::Monotonic, _, _) if walk.is_some() => walk_floor.clone(),
                _ => at.clone(),
            };
            let asked = match &walk {
                Some(walking) => Some(walking.clone()),
                None if floor != at => floor.clone().map(|v| watermark(&field, v)),
                None => position.clone(),
            };
            let key = EntryKey::new(&execution_id, &label, &json_bytes(&asked));
            let request = PullRequest { step_label: label.clone(), position: asked, idempotency_key: key.idempotency_key() };
            let emission = emissions.and_then(|emissions| emissions.get(ordinal));
            let effect_scope = emission.zip(emitted).map(|(emission, input)| input.owner.expected(emission.scope().key())).transpose()?;
            let effect_summary = emission.zip(effect_scope).map(|(emission, scope)| emission.admit_to(dest, scope)).transpose()?;
            let resolved = if emissions.is_none() { Some(self.step(execution, source, dest, PullStep { parent, spec, key:&key, request:&request, shape, authority, at:at.as_ref(), floor:floor.as_ref(), types:&types, undeclared:&undeclared, ordinal })?) } else { None };
            let prepared = authority.zip(resolved.as_ref()).map(|(authority, resolved)| {
                let prepared = recorded(resolved.bytes())?;
                if prepared.authority != authority { return Err(Failure::deterministic(FailureTag::Permanent, "prepared journal authority changed")); }
                Ok(prepared)
            }).transpose()?;
            let pull = match (&prepared, &effect_summary) {
                (_, Some(summary)) => Pull { rows:Vec::new(), cursor:None, more:emissions.is_some_and(|all| ordinal + 1 < all.len()), snapshot_complete:None, types:summary.types.iter().map(|(column, ty)| (column.clone(), ty.name())).collect(), ..Pull::default() },
                (_, None) if emissions.is_some() => Pull { rows:Vec::new(), cursor:None, more:false, snapshot_complete:None, types:Default::default(), ..Pull::default() },
                (Some(prepared), _) => Pull { rows:Vec::new(), cursor:prepared.next.clone(), more:prepared.more, snapshot_complete:prepared.snapshot_complete, types:prepared.types.clone(), skipped:prepared.skipped, declined:prepared.declined.clone(), audit:prepared.audit.clone(), dead_letters:prepared.dead_letters.clone() },
                (None, _) => Pull::decode(resolved.as_ref().expect("ordinary input resolves a pull").bytes())?,
            };
            completion_reported |= pull.snapshot_complete.is_some();
            skipped = skipped.saturating_add(pull.skipped);
            audit.extend(pull.audit.iter().cloned());
            dead_letters.extend(pull.dead_letters.iter().cloned());
            for (extension, n) in &pull.declined {
                let held = declined.entry(extension.clone()).or_default();
                *held = held.saturating_add(*n);
            }
            let shaped_types = if prepared.is_some() || emissions.is_some() { pulled_types(&pull)? } else { shape.shape_types(pulled_types(&pull)?) };
            if let Err(refusal) = admit_types(&shaped_types, &types, &undeclared, ordinal, &spec.run_id) {
                if matches!(&refusal, TypeRefusal::Late(_)) { execution.discard(); }
                return Err(refusal.failure().into());
            }
            for (column, ty) in shaped_types { types.entry(column).or_insert(ty); }
            // The position after the last staged pull, which a checkpoint commits.
            let before = position.clone();
            // A walk continuing under a monotonic cursor asks the position its pull carried.
            let mut continue_walk = |next: Option<Value>| match next {
                Some(next) => {
                    if walk.is_none() {
                        walk_floor = floor.clone();
                    }
                    walk = Some(next);
                }
                None => walk = None,
            };
            let (rows, last, strict) = if let Some(prepared) = &prepared {
                position = prepared.next.clone();
                if plan.cursor_kind == CursorKind::Monotonic {
                    at = open_watermark(position.as_ref(), &field)?.cloned();
                    continue_walk(prepared.walk.clone());
                }
                (Vec::new(), prepared.last, prepared.strict)
            } else { match plan.cursor_kind {
                CursorKind::Monotonic => {
                    // The frontier counts every fetched row; the load admits those at or after
                    // the stored position less the table's allowed lateness.
                    let strict = strictly_past(&field, &pull.rows, at.as_ref())?;
                    let f = frontier(&field, &pull.rows)?;
                    let next = advance(at.as_ref(), f.as_ref())?;
                    let mut admitted = Vec::new();
                    for r in pull.rows {
                        if let Some(v) = clock(&r, &field) {
                            if admits(floor.as_ref(), v)? {
                                admitted.push(r);
                            }
                        }
                    }
                    let advanced = next != at;
                    at = next;
                    position = at.clone().map(|v| watermark(&field, v));
                    // A walk continues only to a position other than the one this pull asked, so
                    // a source repeating its position ends the run (`run.advance.walk-position`).
                    let walking = pull.cursor.filter(|cursor| pull.more && Some(cursor) != request.position.as_ref());
                    let continues = walking.is_some();
                    continue_walk(walking);
                    (admitted, !(continues || (pull.more && advanced)), strict)
                }
                _ => {
                    if let Some(c) = pull.cursor {
                        position = Some(c);
                    }
                    (pull.rows, !pull.more, false)
                }
            }};
            if last {
                snapshot_complete = !pull.more && pull.snapshot_complete.unwrap_or(false);
            }
            let entering = rows.len() as u64;
            let rows = if prepared.is_some() || emissions.is_some() { rows } else { shape.shape(rows)? };
            let count = effect_summary.as_ref().map(|summary| summary.rows).unwrap_or_else(|| prepared.as_ref().map_or(rows.len() as u64, |prepared| prepared.rows));
            // A recorded pull carries the count that entered its chain; an emission has no chain.
            let entering = match (&prepared, &effect_summary) {
                (_, Some(_)) => count,
                (Some(prepared), _) => prepared.fetched.unwrap_or(prepared.rows),
                _ if emissions.is_some() => count,
                _ => entering,
            };
            fetched = fetched.saturating_add(entering);
            kept = kept.saturating_add(count);
            // Before staging more rows, a run whose staged parts reached the bound commits them
            // with the position after its last staged pull; under a monotonic cursor only where
            // this pull's every clock passes that position, so no commit splits an instant
            // (`run.own.checkpoint-commit`, `run.own.checkpoint-frontier`).
            let boundary = plan.cursor_kind != CursorKind::Monotonic || strict;
            if checkpoints && count > 0 && boundary && staged_bytes >= CHECKPOINT_BYTES && !parts.is_empty() {
                // The run id the parts after this checkpoint carry fits the store's bound, or the
                // run refuses before the checkpoint commits; a replay refuses identically
                // (`run.own.checkpoint-id-length`).
                let after = segment_run_id(&spec.run_id, commits + 1).inspect_err(|_| execution.discard())?;
                committed_batches += parts.len() as u64;
                self.checkpoint(execution, spec, dest, staging, std::mem::take(&mut parts), before, &mut cached, checkpointed)?;
                commits += 1;
                *staging = after;
                (staged_rows, staged_bytes) = (0, 0);
            }
            if count > 0 {
                let columns = effect_summary.as_ref().map(|summary| summary.columns.clone()).unwrap_or_else(|| prepared.as_ref().map(|prepared| prepared.columns.clone()).unwrap_or_else(|| rows.iter().flat_map(|r| r.keys()).cloned().collect()));
                undeclared.extend(columns.into_iter().filter(|c| !types.contains_key(c)));
                let stage = Stage {
                    pipeline_id: pipeline_id.to_string(),
                    table: table.to_string(),
                    run_id: staging.clone(),
                    site_id: spec.site_id.clone(),
                    ordinal: u32::try_from(parts.len()).map_err(|_| RunError::Invalid(format!("run `{}` stages more batches than a part ordinal numbers", spec.run_id)))?,
                    row_offset: staged_rows,
                    rows,
                    types: types.clone(),
                };
                if let Some(owner) = parent { self.validate_body_owner(owner)?; }
                let staged = match (emission.zip(effect_scope), &prepared) { (Some((emission, scope)), _) => emission.stage(dest, stage, scope), (_, Some(prepared)) => dest.stage_recorded(stage, &prepared.prepared), _ => dest.stage_batch(stage) };
                let part = staged.map_err(|failure| {
                    // Replaying an incompatible source batch cannot repair it. Prepared body
                    // emissions retain their paid-work owner and recovery policy.
                    if emitted.is_none() && failure.tag == FailureTag::SchemaIncompatible && failure.deterministic {
                        execution.discard();
                    }
                    failure
                })?;
                staged_rows += count;
                staged_bytes = staged_bytes.saturating_add(part.bytes);
                parts.push(part);
                if staged_bytes > STAGED_BYTES_PER_RUN {
                    // A replay stages the same recorded pulls past the bound again.
                    execution.discard();
                    return Err(RunError::RunStagedBytesExceeded(format!(
                        "run `{}` staged {staged_bytes} bytes in {} parts of `{pipeline_id}`/`{table}` since its last commit, past the {STAGED_BYTES_PER_RUN}-byte bound; it commits nothing more and retires its owner",
                        spec.run_id,
                        parts.len()
                    ))
                    .into());
                }
            }
            if last {
                break;
            }
        }

        // The land path carries no stop check. Under a lease, the commit point re-reads the
        // lease: a writer a later acquisition fenced out lands nothing.
        let lease = execution.lease();
        // A replacing table lands no partial inventory, and a run that committed its dead letters
        // alone would succeed while the table stays stale; the pull fails naming each input, and
        // the failure discards what it staged (`run.land.dead-letter-replace`). A replay reads the
        // same inputs, so the recording goes.
        if !dead_letters.is_empty() && dest.replaces(table) && plan.cursor_kind != CursorKind::Monotonic {
            execution.discard();
            let named: Vec<String> = dead_letters.iter().map(|letter| letter.reason.clone()).collect();
            return Err(Failure::deterministic(
                FailureTag::Permanent,
                RunError::PipelineUnreadableInput(format!("replacing table `{table}` lands no partial inventory; {} unreadable: {}", dead_letters.len(), named.join("; "))).to_string(),
            )
            .into());
        }
        // A reported partial inventory cannot displace a complete replacement. Sources
        // predating the completion field retain their existing nonempty-run behavior.
        if dest.replaces(table) && plan.cursor_kind != CursorKind::Monotonic && (skipped > 0 || (completion_reported && !snapshot_complete)) {
            dest.discard(table, staging)?;
            parts.clear();
            position = cached.position.clone();
        }
        if !dead_letters.is_empty() {
            dest.stage_dead_letters(table, staging, &spec.site_id, &dead_letters)?;
        }
        let moved = position != cached.position;
        let batch_count = parts.len() as u64;
        let replace_frontier = batch_count == 0 && skipped == 0 && snapshot_complete && plan.cursor_kind != CursorKind::Monotonic && dest.replaces(table);
        let committed_at = self.catalog.now()?;
        let committed = batch_count > 0 || moved || replace_frontier || !dead_letters.is_empty();
        let landed = if committed {
            let precommit = || -> Result<(), Failure> {
                if let Some(owner) = parent {
                    if emissions.is_some() { self.validate_body_owner(owner)?; }
                    else { self.validate_body_publication(owner)?; }
                }
                execution.assert_publication_current()?;
                let Some(l) = execution.lease() else { return Ok(()) };
                if self.catalog.lease_holds(&l)? {
                    Ok(())
                } else {
                    Err(Failure::new(
                        FailureTag::Storage,
                        contextful_core::store::StoreError::LeaseFenced(format!("lease `{}` at fence {} no longer holds; the commit lands nothing", l.key, l.fence)).to_string(),
                    ))
                }
            };
            precommit()?;
            let _guard = execution.guard(parent, true)?;
            dest.commit(
                Commit {
                    pipeline_id: pipeline_id.to_string(),
                    table: table.to_string(),
                    run_id: staging.clone(),
                    site_id: spec.site_id.clone(),
                    parts,
                    cursor: position.clone(),
                    cursor_kind: plan.cursor_kind,
                    committed_at,
                    fence: lease.as_ref().map(|l| l.fence),
                    replace_frontier,
                },
                &precommit,
            )?
        } else {
            Landed::default()
        };
        let mut next = CursorRow {
            position,
            version: 0,
            marker_run_id: if committed { Some(staging.clone()) } else { cached.marker_run_id.clone() },
            marker_committed_at: if committed { Some(committed_at) } else { cached.marker_committed_at },
        };
        let mut expected = cached.version;
        loop {
            match execution.retire(Some((next.clone(), expected)), lease.as_ref())? {
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
        self.journal.collect(&execution_id)?;
        let landed = Landed { rows: checkpointed.rows + landed.rows, bytes: checkpointed.bytes + landed.bytes };
        Ok((landed, Tally { batches: committed_batches + batch_count, skipped, fetched, kept, declined, audit }))
    }

    /// Commit the parts a run staged since its last commit, carrying `position`, the position
    /// after its last staged pull, and cache that position in the catalog while the owner
    /// stays pending. The journal's recorded pulls behind the commit are collected, so a
    /// resumption pulls afresh from the checkpoint (`run.own.checkpoint-commit`). What the
    /// commit lands adds to `checkpointed` before the cursor update.
    #[allow(clippy::too_many_arguments)]
    fn checkpoint(&self, execution: &mut Execution<'_, J, B>, spec: &RunSpec, dest: &mut dyn Destination, staging: &str, parts: Vec<Part>, position: Option<Value>, cached: &mut CursorRow, checkpointed: &mut Landed) -> Result<(), Close> {
        let plan = &spec.plan;
        let lease = execution.lease();
        let precommit = || -> Result<(), Failure> {
            execution.assert_publication_current()?;
            let Some(l) = execution.lease() else { return Ok(()) };
            if self.catalog.lease_holds(&l)? {
                Ok(())
            } else {
                Err(Failure::new(
                    FailureTag::Storage,
                    contextful_core::store::StoreError::LeaseFenced(format!("lease `{}` at fence {} no longer holds; the checkpoint lands nothing", l.key, l.fence)).to_string(),
                ))
            }
        };
        precommit()?;
        let committed_at = self.catalog.now()?;
        let landed = {
            let _guard = execution.guard(None, true)?;
            dest.commit(
                Commit {
                    pipeline_id: plan.spec.pipeline.clone(),
                    table: plan.spec.table.clone(),
                    run_id: staging.to_string(),
                    site_id: spec.site_id.clone(),
                    parts,
                    cursor: position.clone(),
                    cursor_kind: plan.cursor_kind,
                    committed_at,
                    fence: lease.as_ref().map(|l| l.fence),
                    replace_frontier: false,
                },
                &precommit,
            )?
        };
        // The commit has landed: its rows count on the run's row even when the cursor update
        // after it fails (`run.own.checkpoint-landed`).
        *checkpointed = Landed { rows: checkpointed.rows + landed.rows, bytes: checkpointed.bytes + landed.bytes };
        let scope = execution.scope().clone();
        let next = CursorRow { position: position.clone(), version: 0, marker_run_id: Some(staging.to_string()), marker_committed_at: Some(committed_at) };
        let mut expected = cached.version;
        loop {
            match self.catalog.cursor_cas_at(&scope, expected, next.clone(), lease.as_ref())? {
                Cas::Applied => break,
                Cas::Fenced(e) => return Err(Close::Failed(Failure::new(FailureTag::Storage, e.to_string()))),
                Cas::VersionMoved if plan.cursor_kind == CursorKind::Monotonic => {
                    // A concurrent monotonic writer committed first: the highest position wins.
                    let current = self.catalog.cursor_at(&scope)?;
                    match (&current.position, &position) {
                        (Some(theirs), Some(ours)) if resolve_concurrent(CursorKind::Monotonic, theirs, ours)? == *theirs && theirs != ours => break,
                        (Some(_), None) => break,
                        _ => expected = current.version,
                    }
                }
                Cas::VersionMoved => return Err(Close::Failed(Failure::new(FailureTag::Storage, "the cursor row moved under the checkpointing run's lease"))),
            }
        }
        *cached = self.catalog.cursor_at(&scope)?;
        self.journal.collect(execution.execution_id())?;
        Ok(())
    }

    /// Resolve one pull through the execution's journal under the plan's retry schedule.
    fn step(&self, execution: &mut Execution<'_, J, B>, source: &mut dyn Source, dest: &mut dyn Destination, step: PullStep<'_>) -> Result<Resolved, Close> {
        let PullStep { parent, spec, key, request, shape, authority, at, floor, types:held, undeclared, ordinal } = step;
        let journaled = spec.plan.spec.journal;
        // Each pull's guard records the window its connector declares (`connector.declare-capability.dedup-window`).
        let window = spec.plan.spec.connector.dedup_window_secs;
        if let Some(authority) = authority {
            let journal_it = |bytes: &[u8]| recorded(bytes).is_ok_and(|pull| pull.rows > 0);
            let mut late = false;
            let result = execution.step_keyed_with_parent(key, &journal_it, &mut |token| {
                let bytes = pulled(source, request, token)?;
                let pull = Pull::decode(&bytes)?;
                dest.validate_recorded_control(&spec.plan, &pull)?;
                let types = shape.shape_types(pulled_types(&pull)?);
                if let Err(refusal) = admit_types(&types, held, undeclared, ordinal, &spec.run_id) {
                    late = matches!(&refusal, TypeRefusal::Late(_));
                    return Err(refusal.failure());
                }
                let field = spec.plan.spec.cursor.field.as_deref().unwrap_or_default();
                let (rows, next, last, walk, strict) = if spec.plan.cursor_kind == CursorKind::Monotonic {
                    let strict = strictly_past(field, &pull.rows, at).map_err(preparation_failure)?;
                    let front = frontier(field, &pull.rows).map_err(preparation_failure)?;
                    let next = advance(at, front.as_ref()).map_err(preparation_failure)?;
                    let mut rows = Vec::new();
                    for row in pull.rows {
                        if let Some(clock) = clock(&row, field) { if admits(floor, clock).map_err(preparation_failure)? { rows.push(row); } }
                    }
                    let advanced = next.as_ref() != at;
                    let walk = pull.cursor.filter(|cursor| pull.more && Some(cursor) != request.position.as_ref());
                    let last = !(walk.is_some() || (pull.more && advanced));
                    (rows, next.map(|value| watermark(field, value)), last, walk, strict)
                } else {
                    (pull.rows, pull.cursor.or_else(|| request.position.clone()), !pull.more, None, false)
                };
                let fetched = Some(rows.len() as u64);
                let rows = shape.shape(rows).map_err(preparation_failure)?;
                let count = rows.len() as u64;
                let columns = rows.iter().flat_map(|row| row.keys()).cloned().collect();
                let prepared = if rows.is_empty() { Value::Null } else { dest.prepare_recorded(&spec.plan.spec.table, rows, types.clone(), execution.execution_id())? };
                let recorded = RecordedPull { authority:authority.into(), prepared, rows:count, fetched, columns, types:types.iter().map(|(name, ty)| (name.clone(), ty.name())).collect(), next, last, more:pull.more, snapshot_complete:pull.snapshot_complete, skipped:pull.skipped, declined:pull.declined, audit:pull.audit, walk, strict, dead_letters:pull.dead_letters };
                serde_json::to_vec(&recorded).map_err(|e| Failure::deterministic(FailureTag::SchemaIncompatible, e.to_string()))
            }, parent, window);
            if late { execution.discard(); }
            return result;
        }
        // An empty pull is never journaled.
        let journal_it = |bytes: &[u8]| journaled && Pull::decode(bytes).is_ok_and(|p| !p.rows.is_empty());
        execution.step_keyed_with_parent(key, &journal_it, &mut |token| pulled(source, request, token), parent, window)
    }

    /// Write a stop onto a run row, and under `pipeline` scope onto every in-flight run of
    /// its pipeline, or of its host scope for a host execution's run. Returns the marked run ids.
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
                if other.run_id != target.run_id && other.status.is_in_flight() && same_grain(&target, &other) {
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
        self.catalog.history(pipeline_id, window)
    }
}

/// The column types a pull declares (`run.land.typed-pull`); a spelling no landing reads
/// fails the pull.
fn pulled_types(pull: &Pull) -> Result<Types, Failure> {
    pull.types
        .iter()
        .map(|(column, spelled)| {
            ColumnType::parse(spelled).map(|t| (column.clone(), t)).ok_or_else(|| {
                Failure::deterministic(
                    FailureTag::SchemaIncompatible,
                    format!("StoreSchemaIncompatible: the pull declares column `{column}` as `{spelled}`, which no landing reads"),
                )
            })
        })
        .collect()
}
