//! The worker adapter behind `serve`'s dispatch (`surface.dispatch`): each landing step of a
//! unit goes to a remote worker, the worker heartbeats and calls back through the step's
//! awakeable route, and a step whose worker falls silent past the lapse moves to another
//! worker under the next attempt.
//!
//! The relay is the orchestrator's side of `GET /awake/:token` (a heartbeat) and
//! `POST /awake/:token` (a completion callback). It verifies the signature over
//! `(run, step, attempt, timestamp)`, fences the attempt and the skew, and wakes the step's
//! dispatcher. A rejected message changes no step (`surface.dispatch.callback-rejected`).

use crate::scheduler::Dispatch;
use contextful_core::ports::Clock;
use contextful_core::surface::worker::{Assignment, Ledger, Signed, StepOutcome};
use contextful_core::surface::SurfaceError;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

/// What the orchestrator posts to a worker's `POST /submit`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Submission {
    pub run: String,
    pub step: String,
    pub attempt: u32,
    /// The applied snapshot version the step runs under.
    pub version: u64,
    /// The step's awakeable route on the relay.
    pub callback: String,
    /// One key per step attempt; a repeated submission answers the existing job.
    pub idempotency_key: String,
}

/// How the orchestrator reaches a worker.
pub trait WorkerClient: Send + Sync {
    fn submit(&self, worker: &str, submission: &Submission) -> Result<(), String>;
}

/// Why the relay refused a heartbeat or a callback.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RelayRefusal {
    /// No step holds the token.
    UnknownToken(String),
    /// A signature, attempt or skew refusal, or a body outside the step-result shape.
    Refused(SurfaceError),
}

impl RelayRefusal {
    /// The HTTP status the route answers.
    pub fn status(&self) -> u16 {
        match self {
            RelayRefusal::UnknownToken(_) => 404,
            RelayRefusal::Refused(SurfaceError::DispatchCallbackRejected(_)) => 409,
            RelayRefusal::Refused(_) => 422,
        }
    }
}

impl std::fmt::Display for RelayRefusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RelayRefusal::UnknownToken(t) => write!(f, "no dispatched step holds the token `{t}`"),
            RelayRefusal::Refused(e) => write!(f, "{e}"),
        }
    }
}

struct State {
    ledger: Ledger,
    tokens: BTreeMap<String, (String, String)>,
}

/// The orchestrator's side of the awakeable route for dispatched steps.
pub struct Relay {
    key: Vec<u8>,
    clock: Arc<dyn Clock + Send + Sync>,
    state: Mutex<State>,
    changed: Condvar,
}

impl Relay {
    /// A relay over `workers`, verifying under `key` and reading `clock`.
    pub fn new(key: Vec<u8>, workers: Vec<String>, clock: Arc<dyn Clock + Send + Sync>) -> Relay {
        Relay { key, clock, state: Mutex::new(State { ledger: Ledger::new(workers), tokens: BTreeMap::new() }), changed: Condvar::new() }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Open `step` of `run`: mint its token and place it under attempt 1.
    fn open(&self, run: &str, step: &str) -> Result<(String, Assignment), String> {
        let mut bytes = [0u8; 16];
        getrandom::fill(&mut bytes).map_err(|e| format!("minting a token: {e}"))?;
        let token: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
        let now = self.clock.now();
        let mut state = self.lock();
        let assignment = state.ledger.submit(run, step, now).ok_or("no worker is configured")?;
        state.tokens.insert(token.clone(), (run.to_string(), step.to_string()));
        Ok((token, assignment))
    }

    /// Close the step on `token`. The token keeps its route, so a late heartbeat or
    /// callback meets the attempt fence (`surface.dispatch.callback-rejected`).
    fn close(&self, token: &str) {
        let mut state = self.lock();
        if let Some((run, step)) = state.tokens.get(token).cloned() {
            state.ledger.close(&run, &step);
        }
    }

    /// Read and verify the signed fields of a message on `token`'s route.
    fn verified(&self, state: &State, token: &str, header: &dyn Fn(&str) -> Option<String>) -> Result<Signed, RelayRefusal> {
        let (run, step) = state.tokens.get(token).ok_or_else(|| RelayRefusal::UnknownToken(token.to_string()))?;
        let rejected = |why: &str| RelayRefusal::Refused(SurfaceError::DispatchCallbackRejected(format!("step `{step}` of run `{run}`: {why}")));
        let (signed, signature) = Signed::from_headers(header).ok_or_else(|| rejected("a signed field is absent or malformed"))?;
        if (&signed.run, &signed.step) != (run, step) {
            return Err(rejected("the signed run and step are not this token's"));
        }
        if !signed.verifies(&self.key, &signature) {
            return Err(rejected("the signature does not verify"));
        }
        Ok(signed)
    }

    /// `GET /awake/:token` from a worker: record its heartbeat.
    pub fn heartbeat(&self, token: &str, header: &dyn Fn(&str) -> Option<String>) -> Result<u32, RelayRefusal> {
        let now = self.clock.now();
        let mut state = self.lock();
        let signed = self.verified(&state, token, header)?;
        state.ledger.heartbeat(&signed, now).map_err(RelayRefusal::Refused)?;
        Ok(signed.attempt)
    }

    /// `POST /awake/:token` from a worker: accept its outcome and wake the step's dispatcher.
    pub fn callback(&self, token: &str, header: &dyn Fn(&str) -> Option<String>, body: &[u8]) -> Result<StepOutcome, RelayRefusal> {
        let now = self.clock.now();
        let mut state = self.lock();
        let signed = self.verified(&state, token, header)?;
        let outcome = state.ledger.accept(&signed, body, now).map_err(RelayRefusal::Refused)?;
        drop(state);
        self.changed.notify_all();
        Ok(outcome)
    }

    /// The step's outcome, waiting up to `wait` for one; on none, its reassignment when its
    /// worker has lapsed.
    fn wait(&self, run: &str, step: &str, wait: Duration) -> Result<StepOutcome, Option<Assignment>> {
        let state = self.lock();
        let (mut state, _) = self
            .changed
            .wait_timeout_while(state, wait, |s| s.ledger.result(run, step).is_none())
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(outcome) = state.ledger.result(run, step) {
            return Ok(outcome.clone());
        }
        Err(state.ledger.lapsed_step(run, step, self.clock.now()))
    }

    fn reassign(&self, run: &str, step: &str, refused: &BTreeSet<String>) -> Option<Assignment> {
        let now = self.clock.now();
        self.lock().ledger.reassign(run, step, now, &refused.iter().map(String::as_str).collect())
    }
}

/// Dispatch onto remote workers: each landing step of a unit is one step of the unit's run,
/// submitted in order, each awaited through the relay.
pub struct WorkerDispatch {
    pub relay: Arc<Relay>,
    pub client: Arc<dyn WorkerClient>,
    /// The relay's base URL; a step's route is `<base>/awake/<token>`.
    pub callback_base: String,
    /// How often a waiting step re-checks its worker's heartbeat.
    pub check: Duration,
}

impl WorkerDispatch {
    /// Run one step to its outcome, moving it on a lapse or a failed submission. Workers
    /// refusing in turn are each tried once; when every one has refused, the step fails
    /// naming the last refusal (`surface.dispatch.submit-exhausted`).
    pub fn step(&self, run: &str, step: &str, version: u64) -> Result<StepOutcome, String> {
        let (token, mut assignment) = self.relay.open(run, step)?;
        let callback = format!("{}/awake/{token}", self.callback_base.trim_end_matches('/'));
        let mut refused = BTreeSet::new();
        let result = loop {
            let submission = Submission {
                run: run.to_string(),
                step: step.to_string(),
                attempt: assignment.attempt,
                version,
                callback: callback.clone(),
                idempotency_key: format!("{run}/{step}/{}", assignment.attempt),
            };
            eprintln!("step {step} of {run}: attempt {} on {}", assignment.attempt, assignment.worker);
            if let Err(e) = self.client.submit(&assignment.worker, &submission) {
                eprintln!("step {step} of {run}: attempt {} on {}: {e}", assignment.attempt, assignment.worker);
                refused.insert(assignment.worker.clone());
                match self.relay.reassign(run, step, &refused) {
                    Some(next) => {
                        assignment = next;
                        continue;
                    }
                    None => break Err(format!("step `{step}` of run `{run}`: no worker took it: {e}")),
                }
            }
            refused.clear();
            let next = loop {
                match self.relay.wait(run, step, self.check) {
                    Ok(outcome) => break Ok(outcome),
                    Err(Some(next)) => break Err(next),
                    Err(None) => {}
                }
            };
            match next {
                Ok(outcome) => break Ok(outcome),
                Err(next) => {
                    eprintln!("step {step} of {run}: {} fell silent; rescheduling", assignment.worker);
                    assignment = next;
                }
            }
        };
        self.relay.close(&token);
        result
    }
}

impl Dispatch for WorkerDispatch {
    fn fire(&self, id: &str, steps: &[String], derived_children: &BTreeSet<String>, version: u64) -> Result<String, String> {
        let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or_default();
        let run = format!("{id}-{nanos}");
        let mut lines = Vec::new();
        let mut failures = Vec::new();
        for step in std::iter::once(id).chain(steps.iter().map(String::as_str)) {
            if !failures.is_empty() && !derived_children.contains(step) {
                break;
            }
            match self.step(&run, step, version) {
                Ok(StepOutcome::Done(result)) => lines.push(format!("{step}: {}", String::from_utf8_lossy(&result.encode()))),
                Ok(StepOutcome::Failed { failed }) => failures.push(format!("{step}: {failed}")),
                Err(error) => {
                    failures.push(error);
                    break;
                }
            }
        }
        if failures.is_empty() { Ok(lines.join("; ")) } else { Err(failures.join("; ")) }
    }
}
