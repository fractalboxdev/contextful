//! The store-driven run kind: a client of the execution handle under a host scope. Its
//! first step records the input set, each input row runs the registered body with every
//! call journaled under a label scoped to the row, at most `max_in_flight` rows at once;
//! a row awaiting an unresolved awakeable parks and re-enters once it resolves, and the
//! emitted rows land before the owner retires (`run.journal.store-input`,
//! `run.journal.row-step`, `run.suspend.row-parks`, `run.journal.row-output`).

use crate::execution::{Close, Execution, Tally};
use crate::runner::{Engine, EngineError};
use contextful_core::run::drive::{row_label, CallEffect, Emitted, InputRow, InputSet, PreparedEmitted, RecordedRowCalls, RowBody, RowCalls, RowStop, StoreInput, Woken, INPUT_STEP};
use contextful_core::run::effect::{EffectAdmission, EffectScope, PreparedEmission, RecordedBodyPlan};
use contextful_core::run::journal::EntryKey;
use contextful_core::run::own::{OwnerPins, OwnerScope, PlanPins};
use contextful_core::run::ports::{BlobStore, Cancellation, JournalStore, Landed, OpenExecution, Wake};
use contextful_core::run::record::{RunRow, RunStatus};
use contextful_core::run::retry::Schedule;
use contextful_core::run::{Failure, FailureTag};
use std::collections::{BTreeMap, BTreeSet};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Mutex;
use std::time::Duration;

/// How often a run holding only parked rows re-checks their awakeables.
pub const PARKED_POLL: Duration = Duration::from_millis(500);

/// One store-driven fire: the job's host scope and attempt, its pinned input, its body and
/// its concurrency.
pub struct Drive<'a> {
    /// The job's name; the execution opens under host scope `job:<name>`.
    pub job: String,
    pub input: StoreInput,
    pub body: &'a dyn RowBody,
    pub max_in_flight: usize,
    pub run_id: String,
    pub site_id: String,
    pub pid: u32,
    pub boot_id: String,
    pub schedule: Schedule,
    /// The interval between re-checks of parked rows.
    pub poll: Duration,
}

/// The host scope a store-driven job's execution opens under.
pub fn job_scope(job: &str) -> OwnerScope {
    OwnerScope::host(&format!("job:{job}"))
}

/// Reads the input set at a resolved `as_of`; called at most once per execution.
pub type ReadInput<'a> = dyn FnMut(&str) -> Result<InputSet, Failure> + 'a;
/// Lands every emitted row, one run per output table.
pub type LandOutput<'a> = dyn FnMut(&Emitted) -> Result<Landed, Failure> + 'a;
type Output<T> = BTreeMap<String, Vec<T>>;
type Land<'a, T> = dyn FnMut(&Output<T>) -> Result<Landed, Failure> + 'a;

/// The live body execution whose row effects the shared runner can land.
/// Only a recorded Drive callback receives this admission context.
pub struct BodyOwner {
    pub(crate) scope: OwnerScope,
    pub(crate) execution_id: String,
    pub(crate) run_id: String,
    pub(crate) identity: String,
    effects: Vec<EffectScope>,
}

impl BodyOwner {
    pub(crate) fn expected(&self, key: &EntryKey) -> Result<&EffectScope, Failure> {
        self.effects.iter().find(|scope| scope.key() == key).ok_or_else(|| Failure::deterministic(FailureTag::Permanent, "body effect was not issued by this live execution"))
    }
}

/// The calls one row makes, through its execution.
struct Calls<'x, 'e, J: JournalStore, B: BlobStore> {
    x: &'x Execution<'e, J, B>,
    key: String,
    /// The refusal behind the last failed call, kept so the run closes on it.
    close: Mutex<Option<Close>>,
}

impl<J: JournalStore + Clone, B: BlobStore + Clone> Calls<'_, '_, J, B> {
    fn stop(&self, close: Close) -> RowStop {
        let failure = match &close {
            Close::Failed(f) => f.clone(),
            Close::Refused(e) => Failure::deterministic(FailureTag::Permanent, e.to_string()),
        };
        *self.close.lock().unwrap_or_else(|e| e.into_inner()) = Some(close);
        RowStop::Failed(failure)
    }

    fn record(&self, label: &str, input: &[u8], effect: &mut dyn FnMut(&str) -> Result<Vec<u8>, Failure>) -> Result<Vec<u8>, RowStop> {
        let key = EntryKey::new(self.x.execution_id(), &row_label(&self.key, label), input);
        self.record_key(&key, effect)
    }

    fn record_key(&self, key: &EntryKey, effect: &mut dyn FnMut(&str) -> Result<Vec<u8>, Failure>) -> Result<Vec<u8>, RowStop> {
        let idempotency = key.idempotency_key();
        match self.x.step_keyed(key, &|_| true, &mut |_| effect(&idempotency)) {
            Ok(r) => Ok(r.bytes().to_vec()),
            Err(c) => Err(self.stop(c)),
        }
    }
}

struct PreparedCalls<'a, 'x, 'e, J: JournalStore, B: BlobStore> {
    calls: &'a Calls<'x, 'e, J, B>,
    plan: &'a RecordedBodyPlan,
    admission: &'a dyn EffectAdmission,
    identity: &'a str,
    issued: Mutex<Vec<EffectScope>>,
}

impl<J: JournalStore + Clone, B: BlobStore + Clone> RecordedRowCalls for PreparedCalls<'_, '_, '_, J, B> {
    fn call(&self, label: &str, input: &[u8], effect: &mut CallEffect<'_>) -> Result<PreparedEmission, RowStop> {
        self.plan.effect(label).map_err(|e| self.calls.stop(Close::Failed(Failure::deterministic(FailureTag::Permanent, e.to_string()))))?;
        let key = EntryKey::new(self.calls.x.execution_id(), &row_label(&self.calls.key, label), input);
        let scope = EffectScope::new(&key, self.identity);
        let bytes = self.calls.record_key(&key, &mut |idempotency| {
            let raw = effect(idempotency)?;
            let prepared = PreparedEmission::prepare(self.plan, label, &scope, &raw, self.admission)?;
            crate::guard::log_counts(&key.step_label, prepared.masked_cells());
            prepared.encode()
        })?;
        let emission = PreparedEmission::replay(self.plan, label, &scope, &bytes, self.admission).map_err(|f| self.calls.stop(Close::Failed(f)))?;
        self.issued.lock().unwrap_or_else(|e| e.into_inner()).push(scope);
        Ok(emission)
    }
}

impl<J: JournalStore + Clone, B: BlobStore + Clone> RowCalls for Calls<'_, '_, J, B> {
    fn call(&self, label: &str, input: &[u8], effect: &mut CallEffect<'_>) -> Result<Vec<u8>, RowStop> {
        self.record(label, input, effect)
    }

    fn suspend(&self, label: &str, ttl_secs: u64) -> Result<String, RowStop> {
        let engine = self.x.engine();
        let registry = engine.registry(&format!("suspending row `{}` on `{label}`", self.key)).map_err(|e| self.stop(Close::Refused(e)))?;
        let execution_id = self.x.execution_id().to_string();
        let scoped = row_label(&self.key, label);
        let token = self.record(&format!("suspend:{label}"), &ttl_secs.to_be_bytes(), &mut |_| {
            let created_at = engine.catalog.now()?.to_string();
            registry.suspend(&execution_id, &scoped, &created_at, ttl_secs).map(|a| a.token.into_bytes()).map_err(|e| match e {
                crate::awake::AwakeError::Storage(f) => f,
                crate::awake::AwakeError::Refused(r) => Failure::deterministic(FailureTag::Permanent, r.to_string()),
            })
        })?;
        Ok(String::from_utf8_lossy(&token).into_owned())
    }

    fn awaited(&self, label: &str, token: &str) -> Result<Woken, RowStop> {
        let engine = self.x.engine();
        let registry = engine.registry("awaiting an awakeable").map_err(|e| self.stop(Close::Refused(e)))?;
        let now = engine.catalog.now().map_err(|f| self.stop(Close::Failed(f)))?;
        let woken = match registry.awaited(self.x.execution_id(), token, now) {
            Ok(Wake::Pending) => return Err(RowStop::Parked),
            Ok(Wake::Resumed(p)) => Woken::Resumed(p),
            Ok(Wake::TimedOut) => Woken::TimedOut,
            Err(crate::awake::AwakeError::Storage(f)) => return Err(self.stop(Close::Failed(f))),
            Err(crate::awake::AwakeError::Refused(r)) => return Err(self.stop(Close::Refused(r))),
        };
        // The first outcome read is the recorded one, a timeout included
        // (`run.suspend.recorded-timeout`).
        let recorded = self.record(&format!("wake:{label}"), token.as_bytes(), &mut |_| Ok(woken.encode()))?;
        Woken::decode(&recorded).map_err(RowStop::Failed)
    }
}

/// Where the rows of one pass stand.
struct Pass<T> {
    emitted: BTreeMap<usize, Output<T>>,
    parked: BTreeSet<usize>,
    failed: Option<Close>,
}

impl<T> Default for Pass<T> {
    fn default() -> Self { Self { emitted:BTreeMap::new(), parked:BTreeSet::new(), failed:None } }
}

impl<J: JournalStore + Clone + Send + Sync, B: BlobStore + Clone + Send + Sync> Engine<J, B> {
    /// Run one store-driven fire. `read` reads the input set at the resolved `as_of` when the
    /// execution records none; `land` lands the emitted rows before the owner retires. A fire
    /// that opened closes on a status its row records, returned whatever that status.
    pub fn drive(&self, drive: &Drive<'_>, read: &mut ReadInput<'_>, land: &mut LandOutput<'_>) -> Result<RunRow, EngineError> {
        self.drive_using(drive, drive.input.pins(), read, land, &|row, calls| drive.body.run(row, calls))
    }

    /// The same row/owner machine records only canonical prepared effect results.
    pub fn drive_recorded(&self, drive: &Drive<'_>, plan: &RecordedBodyPlan, admission: &dyn EffectAdmission, read: &mut ReadInput<'_>, land: &mut dyn FnMut(&PreparedEmitted, &BodyOwner) -> Result<Landed, Failure>) -> Result<RunRow, EngineError> {
        let body = drive.body.recorded().ok_or_else(|| contextful_core::run::RunError::JournalRedactionConflict("body declares no protected result projection".into()))?;
        let outputs: Vec<_> = plan.effects().map(|effect| effect.table.clone()).collect::<BTreeSet<_>>().into_iter().collect();
        let registered = RecordedBodyPlan::compile(body.effects(), &outputs)?;
        if registered.identity() != plan.identity() {
            return Err(contextful_core::run::RunError::Invalid("caller projection differs from the registered body's declaration".into()).into());
        }
        let authorities: Vec<_> = plan.effects().map(|effect| admission.identity(effect).map(|identity| (effect.label.clone(), identity))).collect::<Result<_, _>>()?;
        let identity = contextful_core::run::journal::sha256_hex(&serde_json::to_vec(&("prepared-body-owner-v1", drive.input.plan_ref(), plan.identity(), authorities)).map_err(|_| contextful_core::run::RunError::Invalid("prepared body identity does not encode".into()))?);
        let pins = PlanPins { plan_ref:identity.clone(), identities:BTreeMap::from([("input".into(), drive.input.plan_ref()), ("body_projection".into(), plan.identity())]) }.into();
        let scopes = Mutex::new(Vec::new());
        let mut admitted_land = |outputs: &PreparedEmitted| {
            let owner = self.catalog.owner_at(&job_scope(&drive.job))?.ok_or_else(|| Failure::deterministic(FailureTag::Permanent, "prepared body owner is absent"))?;
            land(outputs, &BodyOwner { scope:job_scope(&drive.job), execution_id:owner.execution_id, run_id:drive.run_id.clone(), identity:identity.clone(), effects:scopes.lock().unwrap_or_else(|e| e.into_inner()).clone() })
        };
        self.drive_using(drive, pins, read, &mut admitted_land, &|row, calls| {
            let prepared = PreparedCalls { calls, plan, admission, identity:&identity, issued:Mutex::new(Vec::new()) };
            let outputs = body.run_recorded(row, &prepared)?;
            let issued = prepared.issued.lock().unwrap_or_else(|e| e.into_inner());
            for (table, emissions) in &outputs {
                for emission in emissions {
                    if emission.table() != table || !issued.contains(emission.scope()) {
                        return Err(calls.stop(Close::Failed(Failure::deterministic(FailureTag::Permanent, "body returned an effect outside this row's admitted calls"))));
                    }
                    // Re-admission also binds a returned handle to the current canonical writer.
                    PreparedEmission::replay(plan, emission.label(), emission.scope(), &emission.encode()?, admission).map_err(|f| calls.stop(Close::Failed(f)))?;
                }
            }
            scopes.lock().unwrap_or_else(|e| e.into_inner()).extend(issued.iter().cloned());
            Ok(outputs)
        })
    }

    fn drive_using<T: Send, F>(&self, drive: &Drive<'_>, pins: OwnerPins, read: &mut ReadInput<'_>, land: &mut Land<'_, T>, run: &F) -> Result<RunRow, EngineError>
    where F: Fn(&InputRow, &Calls<'_, '_, J, B>) -> Result<Output<T>, RowStop> + Sync {
        let open = OpenExecution {
            scope: job_scope(&drive.job),
            pins,
            run_id: drive.run_id.clone(),
            site_id: drive.site_id.clone(),
            pid: drive.pid,
            boot_id: drive.boot_id.clone(),
            trace_id: None,
            connector: None,
            schedule: drive.schedule.clone(),
        };
        let x = self.open_execution(&open)?;
        let outcome = self.drive_open(&x, drive, read, land, run);
        x.close_with(outcome)
    }

    fn drive_open<T: Send, F>(&self, x: &Execution<'_, J, B>, drive: &Drive<'_>, read: &mut ReadInput<'_>, land: &mut Land<'_, T>, run: &F) -> Result<(Landed, Tally), Close>
    where F: Fn(&InputRow, &Calls<'_, '_, J, B>) -> Result<Output<T>, RowStop> + Sync {
        // The owner's open instant, not this attempt's: an attempt that dies before the input
        // step records leaves its resume reading at the same instant (`run.journal.open-as-of`).
        let opened_at = match x.opened_at() {
            Some(at) => at,
            None => self.catalog.now()?,
        }
        .to_string();
        let key = EntryKey::new(x.execution_id(), INPUT_STEP, drive.input.plan_ref().as_bytes());
        let recorded = x.step_keyed(&key, &|_| true, &mut |_| {
            let as_of = drive.input.as_of.clone().unwrap_or_else(|| opened_at.clone());
            read(&as_of).map(|set| set.encode())
        })?;
        let input = InputSet::decode(recorded.bytes())?;
        let bounds = input.bounds();
        self.catalog.update_run(x.run_id(), &mut |r| {
            r.input = Some(bounds.clone());
            Ok(())
        })?;

        let rows = input.keyed();
        let mut pending: Vec<usize> = (0..rows.len()).collect();
        let mut emitted: BTreeMap<usize, Output<T>> = BTreeMap::new();
        let mut waiting = false;
        loop {
            let pass = self.pass(x, drive, &rows, &pending, run);
            if let Some(close) = pass.failed {
                return Err(close);
            }
            emitted.extend(pass.emitted);
            if pass.parked.is_empty() {
                break;
            }
            if !waiting {
                x.mark(RunStatus::Waiting).map_err(close_of)?;
                waiting = true;
            }
            if !x.token().wait_timeout(drive.poll) {
                return Err(Close::Failed(Failure::canceled(format!("stopped while {} rows awaited an awakeable", pass.parked.len()))));
            }
            pending = pass.parked.into_iter().collect();
        }
        if waiting {
            x.mark(RunStatus::Running).map_err(close_of)?;
        }

        let mut merged: Output<T> = BTreeMap::new();
        for (_, out) in emitted {
            for (table, rows) in out {
                merged.entry(table).or_default().extend(rows);
            }
        }
        let tables = merged.len() as u64;
        let landed = land(&merged)?;
        Ok((landed, Tally { batches: tables, ..Tally::default() }))
    }

    /// Run the body over `indices`, at most `max_in_flight` rows at once; a failed row
    /// admits no further row.
    fn pass<T: Send, F>(&self, x: &Execution<'_, J, B>, drive: &Drive<'_>, rows: &[InputRow], indices: &[usize], run: &F) -> Pass<T>
    where F: Fn(&InputRow, &Calls<'_, '_, J, B>) -> Result<Output<T>, RowStop> + Sync {
        let next = AtomicUsize::new(0);
        let state = Mutex::new(Pass::default());
        let lock = || state.lock().unwrap_or_else(|e| e.into_inner());
        let workers = drive.max_in_flight.max(1).min(indices.len());
        std::thread::scope(|s| {
            for _ in 0..workers {
                s.spawn(|| loop {
                    if lock().failed.is_some() || x.token().requested() {
                        return;
                    }
                    let Some(&i) = indices.get(next.fetch_add(1, Ordering::SeqCst)) else { return };
                    let row = &rows[i];
                    let calls = Calls { x, key: row.key.clone(), close: Mutex::new(None) };
                    let result = run(row, &calls);
                    let mut st = lock();
                    match result {
                        Ok(out) => {
                            st.emitted.insert(i, out);
                        }
                        Err(RowStop::Parked) => {
                            st.parked.insert(i);
                        }
                        Err(RowStop::Failed(f)) => {
                            let close = calls.close.lock().unwrap_or_else(|e| e.into_inner()).take().unwrap_or(Close::Failed(f));
                            st.failed.get_or_insert(close);
                        }
                    }
                });
            }
        });
        let mut pass = state.into_inner().unwrap_or_else(|e| e.into_inner());
        if pass.failed.is_none() && x.token().requested() {
            pass.failed = Some(Close::Failed(Failure::canceled("stopped between input rows")));
        }
        pass
    }
}

fn close_of(e: EngineError) -> Close {
    match e {
        EngineError::Refused(r) => Close::Refused(r),
        EngineError::Failure(f) => Close::Failed(f),
        EngineError::Topology(t) => Close::Failed(Failure::deterministic(FailureTag::Permanent, t.to_string())),
    }
}
