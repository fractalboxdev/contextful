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
use contextful_core::run::record::{select_history, HistoryPage, RunRow, Window, OWNER_LEASE_TTL_SECS};
use contextful_core::run::{Failure, FailureTag, RunError};
use contextful_core::store::reconcile::ColumnType;
use contextful_core::topology::TopologyError;
use serde_json::Value;
use std::collections::BTreeSet;
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
        self.reconcile(pipeline_id, table, dest)?;
        let open = OpenExecution {
            scope: OwnerScope::table(pipeline_id, table),
            pins: Pins { connector: spec.connector.clone(), content_hash: plan.content_hash.clone(), input_hash: String::new() }.into(),
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
        let outcome = self.body(spec, &mut execution, &mut source, shape, dest);
        if outcome.is_err() {
            // A run id names one attempt, so no later commit names a failed run's staged
            // parts (`run.own.stage-discard`). The run closes on its own failure; a part the
            // discard leaves joins no file list.
            let _ = dest.discard(&plan.spec.table, &spec.run_id);
        }
        execution.close_with(outcome)
    }

    fn body(&self, spec: &RunSpec, execution: &mut Execution<'_, J, B>, source: &mut dyn Source, shape: &dyn Shape, dest: &mut dyn Destination) -> Result<(Landed, Tally), Close> {
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
        let mut snapshot_complete = false;
        let mut declined: std::collections::BTreeMap<String, u64> = std::collections::BTreeMap::new();
        // Columns a staged batch carried with no declared type: their parts hold the
        // inferred type, so a later declaration cannot retype them (`run.land.late-type`).
        let mut undeclared: BTreeSet<String> = BTreeSet::new();
        for ordinal in 0.. {
            if execution.token().requested() {
                return Err(Close::Failed(Failure::canceled("stopped between pulls")));
            }
            let label = format!("pull-{ordinal}");
            let key = EntryKey::new(&execution_id, &label, &json_bytes(&position));
            let request = PullRequest { step_label: label.clone(), position: position.clone(), idempotency_key: key.idempotency_key() };
            let resolved = self.step(spec, execution, &key, &request, source)?;
            let pull = Pull::decode(resolved.bytes())?;
            skipped = skipped.saturating_add(pull.skipped);
            for (extension, n) in &pull.declined {
                let held = declined.entry(extension.clone()).or_default();
                *held = held.saturating_add(*n);
            }
            for (column, ty) in shape.shape_types(pulled_types(&pull)?) {
                match types.get(&column) {
                    Some(held) if *held != ty => {
                        return Err(Failure::deterministic(
                            FailureTag::SchemaIncompatible,
                            format!("StoreSchemaIncompatible: column `{column}` is declared {} and {} by two pulls of one run", held.name(), ty.name()),
                        )
                        .into())
                    }
                    Some(_) => {}
                    None if undeclared.contains(&column) => {
                        execution.discard();
                        return Err(Failure::deterministic(
                            FailureTag::SchemaIncompatible,
                            format!(
                                "PipelineTypeDeclaredLate: pull {ordinal} declares column `{column}` as {}, which an earlier staged batch of run `{}` carried undeclared; declare it from the first pull",
                                ty.name(),
                                spec.run_id
                            ),
                        )
                        .into());
                    }
                    None => {
                        types.insert(column, ty);
                    }
                }
            }
            let (rows, last) = match plan.cursor_kind {
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
            };
            if last {
                snapshot_complete = !pull.more && pull.snapshot_complete;
            }
            let rows = shape.shape(rows)?;
            if !rows.is_empty() {
                let count = rows.len() as u64;
                undeclared.extend(rows.iter().flat_map(|r| r.keys()).filter(|c| !types.contains_key(*c)).cloned());
                let part = dest.stage_batch(Stage {
                    pipeline_id: pipeline_id.to_string(),
                    table: table.to_string(),
                    run_id: spec.run_id.clone(),
                    site_id: spec.site_id.clone(),
                    ordinal: u32::try_from(parts.len()).map_err(|_| RunError::Invalid(format!("run `{}` stages more batches than a part ordinal numbers", spec.run_id)))?,
                    row_offset: staged_rows,
                    rows,
                    types: types.clone(),
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
        let moved = position != cached.position;
        let batch_count = parts.len() as u64;
        let replace_frontier = batch_count == 0 && skipped == 0 && snapshot_complete && plan.cursor_kind != CursorKind::Monotonic && dest.replaces(table);
        let committed_at = self.catalog.now()?;
        let landed = if batch_count > 0 || moved || replace_frontier {
            let precommit = || -> Result<(), Failure> {
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
        Ok((landed, Tally { batches: batch_count, skipped, declined }))
    }

    /// Resolve one pull through the execution's journal under the plan's retry schedule.
    fn step(&self, spec: &RunSpec, execution: &Execution<'_, J, B>, key: &EntryKey, request: &PullRequest, source: &mut dyn Source) -> Result<Resolved, Close> {
        let journaled = spec.plan.spec.journal;
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
