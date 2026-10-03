//! The worker adapter's pure rules (`surface.dispatch`): the head rule over a dependent
//! run, the step-result shape and cap, the signed submission, the attempt-fenced and
//! skew-bounded callback, and the heartbeat ledger that moves a silent worker's step onto
//! another worker under the next attempt.
//!
//! A worker reaches the orchestrator only through the awakeable route
//! (`run.suspend.resume-route`): `GET /awake/:token` is its heartbeat and
//! `POST /awake/:token` its completion callback, each signed over
//! `(run, step, attempt, timestamp)`.

use super::SurfaceError;
use crate::time::Instant;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};

/// Largest step result, in bytes (`surface.dispatch.step-result`).
pub const STEP_RESULT_CAP: usize = 1024 * 1024;

/// Largest distance between a callback's timestamp and the relay's clock, in seconds
/// (`surface.dispatch.callback-skew`).
pub const CALLBACK_SKEW_SECS: u64 = 300;

/// Seconds between a worker's heartbeats (`surface.dispatch.heartbeat-beat`).
pub const HEARTBEAT_BEAT_SECS: u64 = 15;

/// Seconds of heartbeat silence that reschedule a step (`surface.dispatch.heartbeat-lapse`).
pub const HEARTBEAT_LAPSE_SECS: u64 = 60;

/// The environment variable holding the key workers and the relay share.
pub const WORKER_KEY_VAR: &str = "CONTEXTFUL_WORKER_KEY";

/// The headers a heartbeat and a callback carry.
pub const RUN_HEADER: &str = "x-contextful-run";
pub const STEP_HEADER: &str = "x-contextful-step";
pub const ATTEMPT_HEADER: &str = "x-contextful-attempt";
pub const TIMESTAMP_HEADER: &str = "x-contextful-timestamp";
pub const SIGNATURE_HEADER: &str = "x-contextful-signature";

/// The unit a due id dispatches as: a head entry is its own unit, and a landing step of a
/// dependent run raises `DispatchUnitNotAHead` naming its head (`surface.dispatch.not-a-head`).
/// `steps` maps each step's id to its run's head entry.
pub fn unit_of<'a>(id: &'a str, steps: &BTreeMap<String, String>) -> Result<&'a str, SurfaceError> {
    match steps.get(id) {
        Some(head) => Err(SurfaceError::DispatchUnitNotAHead(format!(
            "`{id}` is a step of the dependent run headed by `{head}`; the run is the unit and fires on `{head}`'s cadence"
        ))),
        None => Ok(id),
    }
}

/// What a checkpointed step returns: a pointer, never the bulk it points at
/// (`surface.dispatch.step-result`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "value", rename_all = "kebab-case", deny_unknown_fields)]
pub enum StepResult {
    Pointer(String),
    ObjectKey(String),
    CatalogRow(String),
}

impl StepResult {
    /// Read a step result: JSON of one of the three shapes, at most [`STEP_RESULT_CAP`]
    /// bytes; any other body raises `StepResultRefused` (`surface.dispatch.step-result`).
    pub fn decode(body: &[u8]) -> Result<StepResult, SurfaceError> {
        if body.len() > STEP_RESULT_CAP {
            return Err(SurfaceError::StepResultRefused(format!(
                "a step result of {} B exceeds the {STEP_RESULT_CAP} B cap; a step returns a pointer, an object-store key or a catalog row id",
                body.len()
            )));
        }
        serde_json::from_slice(body).map_err(|e| {
            SurfaceError::StepResultRefused(format!("a step result is a pointer, an object-store key or a catalog row id: {e}"))
        })
    }

    /// The JSON body a worker posts.
    pub fn encode(&self) -> Vec<u8> {
        serde_json::to_vec(self).expect("a step result serializes")
    }
}

/// What a completion callback carries: the step's result, or the line its failure printed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum StepOutcome {
    Done(StepResult),
    Failed { failed: String },
}

impl StepOutcome {
    /// Read a callback body under the step-result cap: a [`StepResult`], or
    /// `{"failed": <line>}`; any other body raises `StepResultRefused`.
    pub fn decode(body: &[u8]) -> Result<StepOutcome, SurfaceError> {
        if body.len() <= STEP_RESULT_CAP {
            if let Ok(outcome @ StepOutcome::Failed { .. }) = serde_json::from_slice::<StepOutcome>(body) {
                return Ok(outcome);
            }
        }
        StepResult::decode(body).map(StepOutcome::Done)
    }

    /// The JSON body a worker posts.
    pub fn encode(&self) -> Vec<u8> {
        serde_json::to_vec(self).expect("a step outcome serializes")
    }
}

/// The hex signature a submission carries: HMAC-SHA256 under `key` over
/// `submit\n<timestamp>\n` followed by the body bytes.
pub fn sign_submission(key: &[u8], timestamp: i64, body: &[u8]) -> String {
    let mut message = format!("submit\n{timestamp}\n").into_bytes();
    message.extend_from_slice(body);
    hex(&hmac_sha256(key, &message))
}

/// Admit a worker's `POST /submit`: its signature under `key` over its timestamp and body,
/// the timestamp within [`CALLBACK_SKEW_SECS`] of `now`, and its `callback` one
/// `<relay>/awake/<token>` route; anything else raises `DispatchSubmitRejected`
/// (`surface.dispatch.submit-signed`).
pub fn admit_submission(
    key: &[u8],
    relay: &str,
    callback: &str,
    header: impl Fn(&str) -> Option<String>,
    body: &[u8],
    now: Instant,
) -> Result<(), SurfaceError> {
    let rejected = |why: String| SurfaceError::DispatchSubmitRejected(why);
    let (Some(timestamp), Some(signature)) = (header(TIMESTAMP_HEADER), header(SIGNATURE_HEADER)) else {
        return Err(rejected(format!("a submission carries `{TIMESTAMP_HEADER}` and `{SIGNATURE_HEADER}`")));
    };
    let timestamp: i64 = timestamp.parse().map_err(|_| rejected(format!("`{TIMESTAMP_HEADER}` `{timestamp}` is no Unix second")))?;
    if !constant_eq(&sign_submission(key, timestamp, body), &signature) {
        return Err(rejected("the signature does not verify under the worker key".into()));
    }
    let skew = now.unix_secs().abs_diff(timestamp);
    if skew > CALLBACK_SKEW_SECS {
        return Err(rejected(format!("its timestamp is {skew} s from the worker's clock, past {CALLBACK_SKEW_SECS} s")));
    }
    let route = format!("{}/awake/", relay.trim_end_matches('/'));
    let token = callback.strip_prefix(&route).filter(|t| !t.is_empty() && t.bytes().all(|b| b.is_ascii_alphanumeric()));
    if token.is_none() {
        return Err(rejected(format!("callback `{callback}` is no `{route}<token>` route on the worker's `[control] relay`")));
    }
    Ok(())
}

fn constant_eq(a: &str, b: &str) -> bool {
    a.len() == b.len() && a.bytes().zip(b.bytes()).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

/// One heartbeat or callback's signed fields.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Signed {
    pub run: String,
    pub step: String,
    pub attempt: u32,
    /// Seconds since the Unix epoch, on the worker's clock.
    pub timestamp: i64,
}

impl Signed {
    fn message(&self) -> Vec<u8> {
        format!("{}\n{}\n{}\n{}", self.run, self.step, self.attempt, self.timestamp).into_bytes()
    }

    /// The hex HMAC-SHA256 under `key` over `(run, step, attempt, timestamp)`.
    pub fn sign(&self, key: &[u8]) -> String {
        hex(&hmac_sha256(key, &self.message()))
    }

    /// Whether `signature` is this message's HMAC under `key`, compared in constant time.
    pub fn verifies(&self, key: &[u8], signature: &str) -> bool {
        constant_eq(&self.sign(key), signature)
    }

    /// Read the signed fields from header lookups, `None` when one is absent or malformed.
    pub fn from_headers(header: impl Fn(&str) -> Option<String>) -> Option<(Signed, String)> {
        let signed = Signed {
            run: header(RUN_HEADER)?,
            step: header(STEP_HEADER)?,
            attempt: header(ATTEMPT_HEADER)?.parse().ok()?,
            timestamp: header(TIMESTAMP_HEADER)?.parse().ok()?,
        };
        Some((signed, header(SIGNATURE_HEADER)?))
    }

    /// The headers carrying these fields and their signature under `key`.
    pub fn headers(&self, key: &[u8]) -> Vec<(&'static str, String)> {
        vec![
            (RUN_HEADER, self.run.clone()),
            (STEP_HEADER, self.step.clone()),
            (ATTEMPT_HEADER, self.attempt.to_string()),
            (TIMESTAMP_HEADER, self.timestamp.to_string()),
            (SIGNATURE_HEADER, self.sign(key)),
        ]
    }
}

fn hmac_sha256(key: &[u8], message: &[u8]) -> [u8; 32] {
    const BLOCK: usize = 64;
    let mut k = [0u8; BLOCK];
    if key.len() > BLOCK {
        k[..32].copy_from_slice(&Sha256::digest(key));
    } else {
        k[..key.len()].copy_from_slice(key);
    }
    let pad = |byte: u8| k.iter().map(|b| b ^ byte).collect::<Vec<u8>>();
    let inner = Sha256::new().chain_update(pad(0x36)).chain_update(message).finalize();
    Sha256::new().chain_update(pad(0x5c)).chain_update(inner).finalize().into()
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// One step placed on one worker under one attempt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Assignment {
    pub run: String,
    pub step: String,
    pub attempt: u32,
    pub worker: String,
}

#[derive(Debug, Clone)]
struct Slot {
    attempt: u32,
    worker: String,
    beat: Instant,
    result: Option<StepOutcome>,
    /// The run has read the outcome; the slot keeps only its final attempt for the fence.
    closed: bool,
}

/// The orchestrator's view of every remote step: its current attempt, the worker holding
/// it and that worker's last heartbeat.
#[derive(Debug, Clone)]
pub struct Ledger {
    workers: Vec<String>,
    slots: BTreeMap<(String, String), Slot>,
}

impl Ledger {
    /// A ledger over the matching workers, in their configured order.
    pub fn new(workers: Vec<String>) -> Ledger {
        Ledger { workers, slots: BTreeMap::new() }
    }

    fn load(&self, worker: &str) -> usize {
        self.slots.values().filter(|s| !s.closed && s.result.is_none() && s.worker == worker).count()
    }

    /// The least-loaded worker outside `excluded`, first in configured order on a tie.
    fn pick(&self, excluded: &BTreeSet<&str>) -> Option<String> {
        self.workers.iter().filter(|w| !excluded.contains(w.as_str())).min_by_key(|w| self.load(w)).cloned()
    }

    /// Place `step` of `run` on a worker under attempt 1; `None` with no worker configured.
    pub fn submit(&mut self, run: &str, step: &str, now: Instant) -> Option<Assignment> {
        let worker = self.pick(&BTreeSet::new())?;
        self.slots.insert((run.into(), step.into()), Slot { attempt: 1, worker: worker.clone(), beat: now, result: None, closed: false });
        Some(Assignment { run: run.into(), step: step.into(), attempt: 1, worker })
    }

    /// The step's current attempt.
    pub fn attempt(&self, run: &str, step: &str) -> Option<u32> {
        self.slots.get(&(run.into(), step.into())).map(|s| s.attempt)
    }

    /// The step's accepted outcome.
    pub fn result(&self, run: &str, step: &str) -> Option<&StepOutcome> {
        self.slots.get(&(run.into(), step.into())).and_then(|s| s.result.as_ref())
    }

    /// Check a signed message against the step's current attempt and the relay's clock:
    /// a superseded or unknown attempt, a closed step, or a timestamp past the skew, raises
    /// `DispatchCallbackRejected` (`surface.dispatch.callback-rejected`).
    fn admit(&self, signed: &Signed, now: Instant) -> Result<(), SurfaceError> {
        let rejected = |why: String| {
            SurfaceError::DispatchCallbackRejected(format!("step `{}` of run `{}`, attempt {}: {why}", signed.step, signed.run, signed.attempt))
        };
        let slot = self.slots.get(&(signed.run.clone(), signed.step.clone())).ok_or_else(|| rejected("no such step is dispatched".into()))?;
        if signed.attempt < slot.attempt {
            return Err(rejected(format!("superseded by attempt {}", slot.attempt)));
        }
        if signed.attempt > slot.attempt {
            return Err(rejected(format!("the step's current attempt is {}", slot.attempt)));
        }
        if slot.closed {
            return Err(rejected(format!("the step closed under attempt {}", slot.attempt)));
        }
        let skew = now.unix_secs().abs_diff(signed.timestamp);
        if skew > CALLBACK_SKEW_SECS {
            return Err(rejected(format!("its timestamp is {skew} s from the relay's clock, past {CALLBACK_SKEW_SECS} s")));
        }
        Ok(())
    }

    /// Record a heartbeat from the step's current attempt.
    pub fn heartbeat(&mut self, signed: &Signed, now: Instant) -> Result<(), SurfaceError> {
        self.admit(signed, now)?;
        if let Some(slot) = self.slots.get_mut(&(signed.run.clone(), signed.step.clone())) {
            slot.beat = slot.beat.max(now);
        }
        Ok(())
    }

    /// Accept a completion callback: the attempt and skew fence first, then the result; a
    /// rejected callback changes no step. A repeat of the accepted callback answers the
    /// recorded result.
    pub fn accept(&mut self, signed: &Signed, body: &[u8], now: Instant) -> Result<StepOutcome, SurfaceError> {
        self.admit(signed, now)?;
        let result = StepOutcome::decode(body)?;
        let slot = self.slots.get_mut(&(signed.run.clone(), signed.step.clone())).expect("an admitted step holds a slot");
        Ok(slot.result.get_or_insert(result).clone())
    }

    /// Steps whose worker has been silent past the lapse, each moved onto another worker
    /// under the next attempt (`surface.dispatch.heartbeat-lapse`). A step with no other
    /// worker to take it stays where it is.
    pub fn lapsed(&mut self, now: Instant) -> Vec<Assignment> {
        let keys: Vec<(String, String)> = self.slots.keys().cloned().collect();
        keys.into_iter().filter_map(|(run, step)| self.lapsed_step(&run, &step, now)).collect()
    }

    /// [`Ledger::lapsed`] for one step.
    pub fn lapsed_step(&mut self, run: &str, step: &str, now: Instant) -> Option<Assignment> {
        let slot = self.slots.get(&(run.into(), step.into()))?;
        if slot.closed || slot.result.is_some() || slot.beat.secs_until(now) <= HEARTBEAT_LAPSE_SECS {
            return None;
        }
        self.reassign(run, step, now, &BTreeSet::new())
    }

    /// Move an open step off its worker onto another outside `excluded` under the next
    /// attempt; `None` when no other worker can take it.
    pub fn reassign(&mut self, run: &str, step: &str, now: Instant, excluded: &BTreeSet<&str>) -> Option<Assignment> {
        let key = (run.to_string(), step.to_string());
        let gone = self.slots.get(&key).filter(|s| !s.closed && s.result.is_none())?.worker.clone();
        let mut excluded: BTreeSet<&str> = excluded.iter().copied().collect();
        excluded.insert(&gone);
        let worker = self.pick(&excluded)?;
        let slot = self.slots.get_mut(&key)?;
        slot.attempt += 1;
        slot.worker = worker.clone();
        slot.beat = now;
        Some(Assignment { run: key.0, step: key.1, attempt: slot.attempt, worker })
    }

    /// Close a step once its run has read its result: the slot drops its outcome and holds
    /// no worker, and keeps its final attempt so a late message still meets the fence
    /// (`surface.dispatch.callback-rejected`).
    pub fn close(&mut self, run: &str, step: &str) {
        if let Some(slot) = self.slots.get_mut(&(run.into(), step.into())) {
            slot.closed = true;
            slot.result = None;
        }
    }
}
