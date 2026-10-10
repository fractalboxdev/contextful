//! The execution handle (`run.journal.substrate-port`): one open execution under a scope,
//! its run row, owner claim, pin check, keeper and retirement. [`Engine::run_with`] is one
//! of its clients; a host opens its own through [`Engine::open_execution`].

use crate::awake::Registry;
use crate::cancel::{CancelToken, Registration};
use crate::journal::{Resolved, StepError};
use crate::runner::{Engine, EngineError};
use crate::stores::{FileBlobStore, FileJournalStore};
use contextful_core::coordinate::{Cas, CursorRow, Lease};
use contextful_core::run::journal::{sha256_hex, EntryKey};
use contextful_core::run::own::{releases, ExecutionOwner, OwnerPins, OwnerScope};
use contextful_core::run::plan::NATIVE_WORLD;
use contextful_core::run::ports::{BlobStore, Capabilities, ExecutionPort, JournalStore, Landed, OpenExecution, Outcome, StepEffect, Substrate, Wake};
use contextful_core::run::project::{Change, StepPatch, StepStatus};
use contextful_core::run::record::{cap_error, Owner, Phase, RunRow, RunStatus};
use contextful_core::run::retry::{decide, Decision, Schedule};
use contextful_core::run::{Failure, FailureTag, RunError};
use contextful_core::time::Instant;
use serde_json::Value;
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// What a successful run counted beside its destination counts.
#[derive(Debug, Clone, Default)]
pub(crate) struct Tally {
    pub batches: u64,
    /// The sum of every pull's `skipped` count (`run.record.skipped-count`).
    pub skipped: u64,
    /// Rows entering the transform chain and rows it kept, summed (`run.land.ingest-tally`).
    pub fetched: u64,
    pub kept: u64,
    /// Every pull's declined tally, summed by extension (`connector.source.declined-tally`).
    pub declined: std::collections::BTreeMap<String, u64>,
    /// Every pull's audit entries, in pull order (`run.exec.audit-entries`).
    pub audit: Vec<String>,
}

/// A failure an execution closes on.
pub(crate) enum Close {
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

impl From<Close> for EngineError {
    fn from(c: Close) -> EngineError {
        match c {
            Close::Refused(e) => EngineError::Refused(e),
            Close::Failed(f) => EngineError::Failure(f),
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

/// A seed for the retry jitter, fixed per run.
fn seed(run_id: &str) -> u64 {
    let hex = sha256_hex(run_id.as_bytes());
    u64::from_str_radix(&hex[..16], 16).unwrap_or_default()
}

/// One open execution, registered with its engine's keeper, which polls the stop mark and
/// renews the owner lease until it closes; dropped unclosed, it deregisters, records no
/// status, and its owner stays pending until the lease lapses (`run.own.unclosed-execution`).
pub struct Execution<'e, J: JournalStore = FileJournalStore, B: BlobStore = FileBlobStore> {
    engine: &'e Engine<J, B>,
    scope: OwnerScope,
    run_id: String,
    execution_id: String,
    /// The scope's owner as it stood at open, before this attempt claimed it.
    pending: Option<ExecutionOwner>,
    pins: OwnerPins,
    /// The scope's cursor row, read at claim.
    cursor: Option<CursorRow>,
    /// When the claimed owner opened: the first attempt's claim, carried by every resume.
    opened_at: Option<Instant>,
    schedule: Schedule,
    token: CancelToken,
    held: Arc<Mutex<Option<Lease>>>,
    registration: Option<Registration>,
    owned: bool,
    retired: bool,
    /// Whether the failure it closes on replays identically, so closing retires the owner
    /// whatever the journal holds.
    discarded: bool,
}

impl<J: JournalStore, B: BlobStore> Engine<J, B> {
    /// What this engine hosts: the native connector world, and awakeables when an
    /// awakeable store is wired.
    pub fn capabilities(&self) -> Capabilities {
        Capabilities { worlds: std::iter::once(NATIVE_WORLD.to_string()).chain(self.worlds.iter().cloned()).collect(), awakeables: self.awakeables.is_some() }
    }

    /// Open an execution under `open.scope`: write the run row, register with the keeper, then
    /// resume the scope's pending owner when its pins hold or claim a fresh one. A moved
    /// pin closes the row `failed` and refuses before any replay.
    pub fn open_execution(&self, open: &OpenExecution) -> Result<Execution<'_, J, B>, EngineError> {
        let mut execution = self.begin(open)?;
        if let Err(close) = execution.claim() {
            let refusal = match &close {
                Close::Refused(e) => EngineError::Refused(e.clone()),
                Close::Failed(f) => EngineError::Failure(f.clone()),
            };
            execution.close_with(Err(close))?;
            return Err(refusal);
        }
        Ok(execution)
    }

    /// Write the attempt's run row and register it with the keeper; the owner is untouched until
    /// [`Execution::claim`].
    pub(crate) fn begin(&self, open: &OpenExecution) -> Result<Execution<'_, J, B>, EngineError> {
        if self.catalog.run(&open.run_id)?.is_some() {
            return Err(RunError::Invalid(format!("run `{}` is already recorded; a run id names one attempt", open.run_id)).into());
        }
        let now = self.catalog.now()?;
        let pending = self.catalog.owner_at(&open.scope)?;
        let execution_id = match &pending {
            Some(o) => o.execution_id.clone(),
            None => fresh_id()?,
        };
        let (pipeline_id, table) = open.scope.pipeline_table().unwrap_or_default();
        let connector = open.connector.clone().unwrap_or_else(|| contextful_core::run::own::ConnectorPin {
            id: String::new(),
            version: String::new(),
            world: String::new(),
            hash: String::new(),
        });
        // The row is written at open, before the first step.
        let row = RunRow {
            run_id: open.run_id.clone(),
            pipeline_id: pipeline_id.to_string(),
            table: table.to_string(),
            site_id: open.site_id.clone(),
            status: RunStatus::Running,
            owner: Some(Owner::leased(open.pid, open.boot_id.clone(), now)),
            started_at: now,
            ended_at: None,
            rows: 0,
            bytes: 0,
            batches: 0,
            skipped: 0,
            fetched: 0,
            kept: 0,
            declined: Default::default(),
            audit: Vec::new(),
            error_kind: None,
            error_message: None,
            connector_id: connector.id,
            connector_version: connector.version,
            connector_hash: connector.hash,
            trace_id: open.trace_id.clone(),
            phase: Phase::Plan,
            execution_id: execution_id.clone(),
            stop: None,
            host_scope: open.scope.host_id().map(str::to_string),
            input: None,
        };
        self.catalog.put_run(&row)?;
        let token = CancelToken::default();
        let held: Arc<Mutex<Option<Lease>>> = Arc::default();
        let mut execution = Execution {
            engine: self,
            scope: open.scope.clone(),
            run_id: open.run_id.clone(),
            execution_id,
            pending,
            pins: open.pins.clone(),
            cursor: None,
            opened_at: None,
            schedule: open.schedule.clone(),
            token: token.clone(),
            held: held.clone(),
            registration: None,
            owned: false,
            retired: false,
            discarded: false,
        };
        execution.emit(Change::Status { status: RunStatus::Running, at: Some(now), error: None });
        execution.registration = Some(self.keeper.register(self.catalog.clone(), &open.run_id, token, held));
        Ok(execution)
    }

    /// Whether the pending owner of `scope` holds `execution_id` under another attempt
    /// still in flight on a live owner lease; such an execution is never retired or collected.
    fn shared_with_a_live_attempt(&self, scope: &OwnerScope, execution_id: &str, run_id: &str) -> Result<bool, Failure> {
        let Some(owner) = self.catalog.owner_at(scope)? else { return Ok(false) };
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
}

impl<'e, J: JournalStore, B: BlobStore> Execution<'e, J, B> {
    /// The engine the execution runs on.
    pub(crate) fn engine(&self) -> &'e Engine<J, B> {
        self.engine
    }

    pub fn scope(&self) -> &OwnerScope {
        &self.scope
    }

    pub fn run_id(&self) -> &str {
        &self.run_id
    }

    /// The id the execution's recorded work keys on.
    pub fn execution_id(&self) -> &str {
        &self.execution_id
    }

    /// The instant the execution opened: its owner's first claim, identical across resumes.
    /// `None` before [`Execution::claim`].
    pub fn opened_at(&self) -> Option<Instant> {
        self.opened_at
    }

    /// Whether this attempt resumed a pending owner.
    pub fn resumed(&self) -> bool {
        self.pending.is_some()
    }

    /// The token the keeper fires on a stop.
    pub fn token(&self) -> &CancelToken {
        &self.token
    }

    /// Hold `lease` for the execution's life: the keeper renews it and close releases it.
    pub(crate) fn hold(&self, lease: Lease) {
        *self.held.lock().unwrap_or_else(|e| e.into_inner()) = Some(lease);
    }

    /// The single-writer lease the execution holds, if any.
    pub(crate) fn lease(&self) -> Option<Lease> {
        self.held.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    /// The scope's cursor row, read once, at or before claim.
    pub(crate) fn cursor_row(&mut self) -> Result<CursorRow, Failure> {
        if self.cursor.is_none() {
            self.cursor = Some(self.engine.catalog.cursor_at(&self.scope)?);
        }
        Ok(self.cursor.clone().unwrap_or_default())
    }

    /// Resume the pending owner when its pins hold, or claim a fresh one, and add this
    /// attempt to it. A build pin's input hash is the hash of the opening position.
    pub(crate) fn claim(&mut self) -> Result<(), Close> {
        let cached = self.cursor_row()?;
        if let OwnerPins::Build(p) = &mut self.pins {
            p.input_hash = sha256_hex(&serde_json::to_vec(&cached.position).unwrap_or_default());
        }
        let mut owner = match self.pending.clone() {
            Some(owner) => {
                self.refuse_a_live_attempt(&owner)?;
                owner.check_pins(&self.pins)?;
                owner
            }
            None => ExecutionOwner {
                execution_id: self.execution_id.clone(),
                scope: self.scope.clone(),
                pins: self.pins.clone(),
                attempts: Vec::new(),
                opened_at: self.engine.catalog.now()?,
            },
        };
        owner.attempts.push(self.run_id.clone());
        self.engine.catalog.put_owner(&owner)?;
        self.pins = owner.pins;
        self.opened_at = Some(owner.opened_at);
        self.owned = true;
        Ok(())
    }

    /// Under a host scope, fail `Transient` while another attempt of the pending owner holds
    /// an unexpired owner lease (`run.own.live-owner`). Table scopes share an owner across
    /// concurrent writers, which their cursor kind and single-writer lease govern.
    fn refuse_a_live_attempt(&self, owner: &ExecutionOwner) -> Result<(), Failure> {
        if self.scope.host_id().is_none() {
            return Ok(());
        }
        for attempt in owner.attempts.iter().filter(|a| a.as_str() != self.run_id) {
            if self.engine.holder_live(attempt)? {
                return Err(Failure::new(
                    FailureTag::Transient,
                    format!("attempt `{attempt}` holds execution `{}` of {} on an unexpired owner lease", self.execution_id, self.scope),
                ));
            }
        }
        Ok(())
    }

    /// Mark the failure this execution is about to close on as one a replay of its recorded
    /// steps reproduces, so the close retires the owner and collects its journal.
    pub(crate) fn discard(&mut self) {
        self.discarded = true;
    }

    /// Retire the owner and, given a cursor row and the version it was read at, cache the
    /// position in the same transaction (`run.own.retirement`).
    pub(crate) fn retire(&mut self, cursor: Option<(CursorRow, u64)>, fence: Option<&Lease>) -> Result<Cas, Failure> {
        let cas = self.engine.catalog.retire_at(&self.scope, &self.execution_id, cursor, fence)?;
        if cas == Cas::Applied {
            self.retired = true;
        }
        Ok(cas)
    }

    /// Emit one projection event; emission never blocks and is no journal step.
    pub(crate) fn emit(&self, change: Change) {
        if let Some(e) = &self.engine.emitter {
            e.emit(&self.run_id, self.scope.workflow(), change);
        }
    }

    /// Resolve one step under `key` through the journal under the execution's retry
    /// schedule. `journal_it` decides whether a computed value is recorded.
    pub(crate) fn step_keyed(
        &self,
        key: &EntryKey,
        journal_it: &dyn Fn(&[u8]) -> bool,
        effect: &mut StepEffect<'_>,
    ) -> Result<Resolved, Close> {
        let engine = self.engine;
        let holder_live = |run: &str| engine.holder_live(run);
        let token = &self.token;
        let label = key.step_label.as_str();
        let mut attempt = 1;
        loop {
            self.emit(Change::Step(StepPatch { attempts: Some(attempt), ..StepPatch::new(label).status(StepStatus::Running) }));
            let failure = match engine.journal.step(key, &self.run_id, &holder_live, token, journal_it, &mut || effect(token)) {
                Ok(r) => {
                    self.emit(Change::Step(StepPatch::new(label).status(StepStatus::Completed)));
                    return Ok(r);
                }
                Err(StepError::Failed(f)) => f,
                Err(e) => return Err(e.into()),
            };
            if failure.tag == FailureTag::Canceled {
                return Err(Close::Failed(failure));
            }
            match decide(&self.schedule, label, attempt, &failure, seed(&self.run_id)) {
                Decision::Retry { delay_ms } => {
                    self.emit(Change::Step(StepPatch { failure: Some(failure.clone()), ..StepPatch::new(label).status(StepStatus::Retrying) }));
                    if !token.wait_timeout(Duration::from_millis(delay_ms)) {
                        return Err(Close::Failed(Failure::canceled(format!("stopped during the retry sleep of `{label}`"))));
                    }
                    attempt += 1;
                }
                Decision::Fail { error, .. } | Decision::Reschedule { error, .. } => {
                    self.emit(Change::Step(StepPatch { failure: Some(failure), ..StepPatch::new(label).status(StepStatus::Failed) }));
                    return Err(Close::Refused(error));
                }
            }
        }
    }

    /// Refuse a step or commit once the execution retired its owner.
    fn live(&self, what: &str) -> Result<(), EngineError> {
        if self.retired {
            return Err(RunError::Invalid(format!("execution `{}` committed its cursor and retired; {what} opens a fresh execution", self.execution_id)).into());
        }
        Ok(())
    }

    /// Set the run row's status, keeping every other field.
    pub(crate) fn mark(&self, status: RunStatus) -> Result<(), EngineError> {
        self.engine.catalog.update_run(&self.run_id, &mut |r| {
            r.status = status;
            Ok(())
        })?;
        self.emit(Change::Status { status, at: None, error: None });
        Ok(())
    }

    /// Close on `outcome`: deregister from the keeper, release the held lease, record the status,
    /// then retire the owner where the status releases it (`run.own.pin-release`).
    pub(crate) fn close_with(mut self, outcome: Result<(Landed, Tally), Close>) -> Result<RunRow, EngineError> {
        let engine = self.engine;
        drop(self.registration.take());
        let lease = self.held.lock().unwrap_or_else(|e| e.into_inner()).take();
        if let Some(l) = &lease {
            engine.catalog.release(l)?;
        }

        let now = engine.catalog.now()?;
        let closed = engine.catalog.update_run(&self.run_id, &mut |r| {
            r.ended_at = Some(now);
            r.phase = Phase::Commit;
            r.owner = None;
            match &outcome {
                Ok((landed, tally)) => {
                    r.status = RunStatus::Success;
                    r.rows = landed.rows;
                    r.bytes = landed.bytes;
                    r.batches = tally.batches;
                    r.skipped = tally.skipped;
                    r.fetched = tally.fetched;
                    r.kept = tally.kept;
                    r.declined = tally.declined.clone();
                    r.audit = tally.audit.clone();
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
        let row = match closed {
            Some(Ok(r)) => r,
            _ => engine.catalog.run(&self.run_id)?.ok_or_else(|| Failure::new(FailureTag::Storage, format!("run `{}` has no row to close", self.run_id)))?,
        };
        let error = row.error_kind.map(|tag| Failure::new(tag, row.error_message.clone().unwrap_or_default()));
        self.emit(Change::Status { status: row.status, at: row.ended_at, error });
        let execution_id = self.execution_id.clone();
        // A success that committed retired already; any other releasing status retires
        // unless another live attempt shares the execution.
        let discarded = self.discarded && row.status == RunStatus::Failed;
        if self.owned && (discarded || releases(row.status, engine.journal.recorded(&execution_id)?)) {
            let retires = if row.status == RunStatus::Success { !self.retired } else { !engine.shared_with_a_live_attempt(&self.scope, &execution_id, &self.run_id)? };
            if retires {
                self.retire(None, None)?;
                engine.journal.collect(&execution_id)?;
            }
        }
        Ok(row)
    }
}

impl<J: JournalStore + Clone, B: BlobStore + Clone> Engine<J, B> {
    /// The awakeable registry over the wired store; `CapabilityUnwired` without one
    /// (`run.journal.unwired-capability`).
    pub(crate) fn registry(&self, reach: &str) -> Result<Registry<Arc<dyn contextful_core::run::ports::AwakeableStore>, J, B>, RunError> {
        let store = self.awakeables.clone().ok_or_else(|| RunError::CapabilityUnwired(format!("{reach} needs an awakeable store and this engine wires none")))?;
        Ok(Registry::over(store, self.journal.clone()))
    }
}

impl<J: JournalStore + Clone, B: BlobStore + Clone> Substrate for Engine<J, B> {
    type Error = EngineError;
    type Execution<'s>
        = Execution<'s, J, B>
    where
        Self: 's;

    fn capabilities(&self) -> Capabilities {
        Engine::capabilities(self)
    }

    fn open(&self, open: &OpenExecution) -> Result<Execution<'_, J, B>, EngineError> {
        self.open_execution(open)
    }
}

impl<J: JournalStore + Clone, B: BlobStore + Clone> ExecutionPort for Execution<'_, J, B> {
    type Error = EngineError;

    fn execution_id(&self) -> &str {
        Execution::execution_id(self)
    }

    fn plan_ref(&self) -> &str {
        self.pins.plan_ref()
    }

    fn position(&self) -> Option<&Value> {
        self.cursor.as_ref().and_then(|c| c.position.as_ref())
    }

    fn step(&mut self, label: &str, input: &[u8], effect: &mut StepEffect<'_>) -> Result<Vec<u8>, EngineError> {
        self.live(&format!("step `{label}`"))?;
        let key = EntryKey::new(&self.execution_id, label, input);
        Ok(self.step_keyed(&key, &|_| true, effect)?.bytes().to_vec())
    }

    fn r#unsafe(&mut self, label: &str, effect: &mut StepEffect<'_>) -> Result<Vec<u8>, EngineError> {
        self.live(&format!("unsafe read `{label}`"))?;
        let key = EntryKey::new(&self.execution_id, &format!("unsafe:{label}"), &[]);
        Ok(self.step_keyed(&key, &|_| false, effect)?.bytes().to_vec())
    }

    fn commit(&mut self, position: Option<Value>) -> Result<(), EngineError> {
        self.live("a commit")?;
        let cached = self.cursor_row()?;
        let next = CursorRow { position, version: 0, ..cached.clone() };
        let lease = self.lease();
        match self.retire(Some((next, cached.version)), lease.as_ref())? {
            Cas::Applied => {
                self.engine.journal.collect(&self.execution_id)?;
                Ok(())
            }
            Cas::Fenced(e) => Err(Failure::new(FailureTag::Storage, e.to_string()).into()),
            Cas::VersionMoved => Err(Failure::new(FailureTag::Storage, format!("the cursor row of {} moved since execution `{}` opened", self.scope, self.execution_id)).into()),
        }
    }

    fn suspend(&mut self, label: &str, ttl_secs: u64) -> Result<String, EngineError> {
        self.live(&format!("suspending `{label}`"))?;
        let registry = self.engine.registry(&format!("suspending step `{label}`"))?;
        let created_at = self.engine.catalog.now()?.to_string();
        let key = EntryKey::new(&self.execution_id, &format!("suspend:{label}"), &ttl_secs.to_be_bytes());
        let execution_id = self.execution_id.clone();
        // The minted token is itself a recorded step, so a replay awaits the same awakeable.
        let token = self.step_keyed(&key, &|_| true, &mut |_| {
            registry.suspend(&execution_id, label, &created_at, ttl_secs).map(|a| a.token.into_bytes()).map_err(|e| match e {
                crate::awake::AwakeError::Storage(f) => f,
                crate::awake::AwakeError::Refused(r) => Failure::deterministic(FailureTag::Permanent, r.to_string()),
            })
        })?;
        self.mark(RunStatus::Waiting)?;
        Ok(String::from_utf8_lossy(token.bytes()).into_owned())
    }

    fn awaited(&mut self, token: &str) -> Result<Wake, EngineError> {
        let registry = self.engine.registry("awaiting an awakeable")?;
        let now = self.engine.catalog.now()?;
        let wake = registry.awaited(&self.execution_id, token, now).map_err(|e| match e {
            crate::awake::AwakeError::Storage(f) => EngineError::Failure(f),
            crate::awake::AwakeError::Refused(r) => EngineError::Refused(r),
        })?;
        if wake != Wake::Pending {
            self.mark(RunStatus::Running)?;
        }
        Ok(wake)
    }

    fn close(self, outcome: Outcome) -> Result<RunRow, EngineError> {
        let outcome = match outcome {
            Outcome::Success { rows, bytes, batches } => Ok((Landed { rows, bytes }, Tally { batches, ..Tally::default() })),
            Outcome::Failed(f) => Err(Close::Failed(f)),
        };
        self.close_with(outcome)
    }
}
