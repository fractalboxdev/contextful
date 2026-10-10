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
use contextful_core::run::advance::{admits, advance, clock, frontier, open_watermark, resolve_concurrent, watermark, CursorKind};
use contextful_core::run::cancel::{mark, same_grain, Scope};
use contextful_core::run::journal::EntryKey;
use contextful_core::run::own::{ConnectorPin, OwnerScope, Pins};
use contextful_core::run::plan::Plan;
use contextful_core::run::ports::{
    AwakeableStore, BlobStore, Cancellation, Commit, Destination, JournalStore, Landed, OpenExecution, Part, Pull, PullRequest, Shape, Source, Stage, Types, Unshaped,
    STAGED_BYTES_PER_RUN,
};
use contextful_core::run::record::{select_history, HistoryPage, RunRow, RunStatus, Window, OWNER_LEASE_TTL_SECS};
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
}

fn recorded(bytes: &[u8]) -> Result<RecordedPull, Failure> {
    serde_json::from_slice(bytes).map_err(|e| Failure::deterministic(FailureTag::SchemaIncompatible, format!("prepared journal pull is malformed: {e}")))
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
    spec: &'a RunSpec,
    key: &'a EntryKey,
    request: &'a PullRequest,
    shape: &'a dyn Shape,
    authority: Option<&'a str>,
    at: Option<&'a Value>,
    types: &'a Types,
    undeclared: &'a BTreeSet<String>,
    ordinal: usize,
}

struct EmissionInput<'a> {
    owner: &'a crate::drive::BodyOwner,
    frames: &'a [contextful_core::run::effect::PreparedEmission],
}

enum BodyInput<'a> {
    Source { authority: Option<&'a str> },
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
        self.run_input(spec, source, shape, dest, None)
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
        self.run_input(spec, &mut NoSource, &Unshaped, dest, Some(&EmissionInput { owner, frames:emissions }))
    }

    fn validate_body_owner(&self, owner: &crate::drive::BodyOwner) -> Result<(), Failure> {
        let active = self.catalog.owner_at(&owner.scope)?.ok_or_else(|| Failure::deterministic(FailureTag::Permanent, "prepared body owner is absent"))?;
        if active.execution_id != owner.execution_id || active.pins.plan_ref() != owner.identity || active.attempts.last() != Some(&owner.run_id) {
            return Err(Failure::deterministic(FailureTag::Permanent, "prepared body owner or pinned authority changed"));
        }
        let run = self.catalog.run(&owner.run_id)?.ok_or_else(|| Failure::deterministic(FailureTag::Permanent, "prepared body attempt is absent"))?;
        if run.execution_id != owner.execution_id {
            return Err(Failure::deterministic(FailureTag::Permanent, "prepared body attempt execution changed"));
        }
        if run.stop.is_some() || run.status == RunStatus::Canceled {
            return Err(Failure::canceled("prepared body attempt stopped"));
        }
        if !run.status.is_in_flight() {
            return Err(Failure::deterministic(FailureTag::Permanent, "prepared body attempt stopped or changed"));
        }
        Ok(())
    }

    fn run_input(&self, spec: &RunSpec, source: &mut dyn Source, shape: &dyn Shape, dest: &mut dyn Destination, emissions: Option<&EmissionInput<'_>>) -> Result<RunRow, EngineError> {
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
            scope: OwnerScope::table(pipeline_id, table),
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
        let input = match emissions { Some(emissions) => BodyInput::Effects(emissions), None => BodyInput::Source { authority:authority.as_deref() } };
        let outcome = self.body(spec, &mut execution, &mut source, shape, dest, input);
        if outcome.is_err() {
            // A run id names one attempt, so no later commit names a failed run's staged
            // parts (`run.own.stage-discard`). The run closes on its own failure; a part the
            // discard leaves joins no file list.
            let _ = dest.discard(&plan.spec.table, &spec.run_id);
        }
        execution.close_with(outcome)
    }

    fn body(&self, spec: &RunSpec, execution: &mut Execution<'_, J, B>, source: &mut dyn Source, shape: &dyn Shape, dest: &mut dyn Destination, input: BodyInput<'_>) -> Result<(Landed, Tally), Close> {
        let (authority, emitted) = match input { BodyInput::Source { authority } => (authority, None), BodyInput::Effects(emitted) => (None, Some(emitted)) };
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

        let cached = execution.cursor_row()?;
        execution.claim()?;
        let execution_id = execution.execution_id().to_string();

        let field = plan.spec.cursor.field.clone().unwrap_or_default();
        let mut position = cached.position.clone();
        let mut at = match plan.cursor_kind {
            CursorKind::Monotonic => open_watermark(position.as_ref(), &field)?.cloned(),
            _ => None,
        };
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
        // Columns a staged batch carried with no declared type: their parts hold the
        // inferred type, so a later declaration cannot retype them (`run.land.late-type`).
        let mut undeclared: BTreeSet<String> = BTreeSet::new();
        for ordinal in 0.. {
            if let Some(input) = emitted { self.validate_body_owner(input.owner)?; }
            if execution.token().requested() {
                return Err(Close::Failed(Failure::canceled("stopped between pulls")));
            }
            let label = format!("pull-{ordinal}");
            let key = EntryKey::new(&execution_id, &label, &json_bytes(&position));
            let request = PullRequest { step_label: label.clone(), position: position.clone(), idempotency_key: key.idempotency_key() };
            let emission = emissions.and_then(|emissions| emissions.get(ordinal));
            let effect_scope = emission.zip(emitted).map(|(emission, input)| input.owner.expected(emission.scope().key())).transpose()?;
            let effect_summary = emission.zip(effect_scope).map(|(emission, scope)| emission.admit_to(dest, scope)).transpose()?;
            let resolved = if emissions.is_none() { Some(self.step(execution, source, dest, PullStep { spec, key:&key, request:&request, shape, authority, at:at.as_ref(), types:&types, undeclared:&undeclared, ordinal })?) } else { None };
            let prepared = authority.zip(resolved.as_ref()).map(|(authority, resolved)| {
                let prepared = recorded(resolved.bytes())?;
                if prepared.authority != authority { return Err(Failure::deterministic(FailureTag::Permanent, "prepared journal authority changed")); }
                Ok(prepared)
            }).transpose()?;
            let pull = match (&prepared, &effect_summary) {
                (_, Some(summary)) => Pull { rows:Vec::new(), cursor:None, more:emissions.is_some_and(|all| ordinal + 1 < all.len()), snapshot_complete:None, types:summary.types.iter().map(|(column, ty)| (column.clone(), ty.name())).collect(), ..Pull::default() },
                (_, None) if emissions.is_some() => Pull { rows:Vec::new(), cursor:None, more:false, snapshot_complete:None, types:Default::default(), ..Pull::default() },
                (Some(prepared), _) => Pull { rows:Vec::new(), cursor:prepared.next.clone(), more:prepared.more, snapshot_complete:prepared.snapshot_complete, types:prepared.types.clone(), skipped:prepared.skipped, declined:prepared.declined.clone(), audit:prepared.audit.clone() },
                (None, _) => Pull::decode(resolved.as_ref().expect("ordinary input resolves a pull").bytes())?,
            };
            completion_reported |= pull.snapshot_complete.is_some();
            skipped = skipped.saturating_add(pull.skipped);
            audit.extend(pull.audit.iter().cloned());
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
            let (rows, last) = if let Some(prepared) = &prepared {
                position = prepared.next.clone();
                if plan.cursor_kind == CursorKind::Monotonic { at = open_watermark(position.as_ref(), &field)?.cloned(); }
                (Vec::new(), prepared.last)
            } else { match plan.cursor_kind {
                CursorKind::Monotonic => {
                    // The frontier counts every fetched row; the load admits those at or after the stored position.
                    let f = frontier(&field, &pull.rows)?;
                    let next = advance(at.as_ref(), f.as_ref())?;
                    let mut admitted = Vec::new();
                    for r in pull.rows {
                        if let Some(v) = clock(&r, &field) {
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
            if count > 0 {
                let columns = effect_summary.as_ref().map(|summary| summary.columns.clone()).unwrap_or_else(|| prepared.as_ref().map(|prepared| prepared.columns.clone()).unwrap_or_else(|| rows.iter().flat_map(|r| r.keys()).cloned().collect()));
                undeclared.extend(columns.into_iter().filter(|c| !types.contains_key(c)));
                let stage = Stage {
                    pipeline_id: pipeline_id.to_string(),
                    table: table.to_string(),
                    run_id: spec.run_id.clone(),
                    site_id: spec.site_id.clone(),
                    ordinal: u32::try_from(parts.len()).map_err(|_| RunError::Invalid(format!("run `{}` stages more batches than a part ordinal numbers", spec.run_id)))?,
                    row_offset: staged_rows,
                    rows,
                    types: types.clone(),
                };
                if let Some(input) = emitted { self.validate_body_owner(input.owner)?; }
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
                        "run `{}` staged {staged_bytes} bytes in {} parts of `{pipeline_id}`/`{table}`, past the {STAGED_BYTES_PER_RUN}-byte bound; it commits nothing and retires its owner",
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
        // A reported partial inventory cannot displace a complete replacement. Sources
        // predating the completion field retain their existing nonempty-run behavior.
        if dest.replaces(table) && plan.cursor_kind != CursorKind::Monotonic && (skipped > 0 || (completion_reported && !snapshot_complete)) {
            dest.discard(table, &spec.run_id)?;
            parts.clear();
            position = cached.position.clone();
        }
        let moved = position != cached.position;
        let batch_count = parts.len() as u64;
        let replace_frontier = batch_count == 0 && skipped == 0 && snapshot_complete && plan.cursor_kind != CursorKind::Monotonic && dest.replaces(table);
        let committed_at = self.catalog.now()?;
        let landed = if batch_count > 0 || moved || replace_frontier {
            let precommit = || -> Result<(), Failure> {
                if let Some(input) = emitted { self.validate_body_owner(input.owner)?; }
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
            dest.commit(
                Commit {
                    pipeline_id: pipeline_id.to_string(),
                    table: table.to_string(),
                    run_id: spec.run_id.clone(),
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
        let committed = batch_count > 0 || moved || replace_frontier;
        let mut next = CursorRow {
            position,
            version: 0,
            marker_run_id: if committed { Some(spec.run_id.clone()) } else { cached.marker_run_id.clone() },
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
        Ok((landed, Tally { batches: batch_count, skipped, fetched, kept, declined, audit }))
    }

    /// Resolve one pull through the execution's journal under the plan's retry schedule.
    fn step(&self, execution: &mut Execution<'_, J, B>, source: &mut dyn Source, dest: &mut dyn Destination, step: PullStep<'_>) -> Result<Resolved, Close> {
        let PullStep { spec, key, request, shape, authority, at, types:held, undeclared, ordinal } = step;
        let journaled = spec.plan.spec.journal;
        if let Some(authority) = authority {
            let journal_it = |bytes: &[u8]| recorded(bytes).is_ok_and(|pull| pull.rows > 0);
            let mut late = false;
            let result = execution.step_keyed(key, &journal_it, &mut |token| {
                let bytes = source.pull(request, token)?;
                let pull = Pull::decode(&bytes)?;
                dest.validate_recorded_control(&spec.plan, &pull)?;
                let types = shape.shape_types(pulled_types(&pull)?);
                if let Err(refusal) = admit_types(&types, held, undeclared, ordinal, &spec.run_id) {
                    late = matches!(&refusal, TypeRefusal::Late(_));
                    return Err(refusal.failure());
                }
                let field = spec.plan.spec.cursor.field.as_deref().unwrap_or_default();
                let (rows, next, last) = if spec.plan.cursor_kind == CursorKind::Monotonic {
                    let front = frontier(field, &pull.rows).map_err(preparation_failure)?;
                    let next = advance(at, front.as_ref()).map_err(preparation_failure)?;
                    let mut rows = Vec::new();
                    for row in pull.rows {
                        if let Some(clock) = clock(&row, field) { if admits(at, clock).map_err(preparation_failure)? { rows.push(row); } }
                    }
                    let advanced = next.as_ref() != at;
                    (rows, next.map(|value| watermark(field, value)), !(pull.more && advanced))
                } else {
                    (pull.rows, pull.cursor.or_else(|| request.position.clone()), !pull.more)
                };
                let fetched = Some(rows.len() as u64);
                let rows = shape.shape(rows).map_err(preparation_failure)?;
                let count = rows.len() as u64;
                let columns = rows.iter().flat_map(|row| row.keys()).cloned().collect();
                let prepared = if rows.is_empty() { Value::Null } else { dest.prepare_recorded(&spec.plan.spec.table, rows, types.clone(), execution.execution_id())? };
                let recorded = RecordedPull { authority:authority.into(), prepared, rows:count, fetched, columns, types:types.iter().map(|(name, ty)| (name.clone(), ty.name())).collect(), next, last, more:pull.more, snapshot_complete:pull.snapshot_complete, skipped:pull.skipped, declined:pull.declined, audit:pull.audit };
                serde_json::to_vec(&recorded).map_err(|e| Failure::deterministic(FailureTag::SchemaIncompatible, e.to_string()))
            });
            if late { execution.discard(); }
            return result;
        }
        // An empty pull is never journaled.
        let journal_it = |bytes: &[u8]| journaled && Pull::decode(bytes).is_ok_and(|p| !p.rows.is_empty());
        execution.step_keyed(key, &journal_it, &mut |token| source.pull(request, token))
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
        Ok(select_history(self.catalog.runs(pipeline_id)?, window))
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
