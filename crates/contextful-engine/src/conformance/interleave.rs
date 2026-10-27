//! Interleaved journal cases over an adapter pair. Each caller runs [`Journal::step`] on its
//! own thread over gated stores; the scheduler admits one port call, effect or store
//! operation per case step, so a case fixes the interleaving the threads run. The protocol
//! package's journal model takes the same steps, and [`Interleaving::show`] prints the
//! state in the model's projection.

use crate::journal::{Journal, Resolved, StepError};
use contextful_core::run::journal::{sha256_hex, EntryKey, Row, Stored, BLOB_SWEEP_GRACE_SECS, INLINE_CUTOFF_BYTES};
use contextful_core::run::ports::{BlobStore, Cancellation, JournalStore};
use contextful_core::run::{Failure, FailureTag, RunError};
use std::cell::RefCell;
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

/// Callers a case ranges over.
pub const CALLERS: usize = 3;
/// Keys a case ranges over, each the one step of its own execution.
pub const KEYS: usize = 2;
/// The highest attempt count a `note` step writes.
pub const MAX_ATTEMPTS: u32 = 2;

/// What a caller's effect returns.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Outcome {
    /// A value at or under the inline cutoff.
    Inline,
    /// A value over the inline cutoff, placed in a blob.
    Blob,
    /// A failure.
    Fail,
    /// A value the caller does not journal.
    Unjournaled,
}

/// One case step.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Step {
    /// An idle caller enters [`Journal::step`] on a key.
    Begin { caller: usize, key: usize },
    /// The caller's waiting port call runs, and the caller runs to its next one.
    Advance { caller: usize },
    /// The claimed caller's effect returns.
    Effect { caller: usize, outcome: Outcome },
    /// The claimed caller persists one more closed attempt on its key's row.
    Note { caller: usize },
    /// The caller's owner lease lapses, for good.
    Lapse { caller: usize },
    /// The caller's process dies wherever it waits.
    Crash { caller: usize },
    /// A resolved caller drops its result.
    Finish { caller: usize },
    /// One sweep pass: `aged` sweeps past the grace window, otherwise inside it.
    Sweep { aged: bool },
    /// The key's execution retires and its rows are collected.
    Retire { key: usize },
}

impl Step {
    /// The step in the model executable's syntax.
    pub fn line(&self) -> String {
        match *self {
            Step::Begin { caller, key } => format!("begin {caller} {key}"),
            Step::Advance { caller } => format!("advance {caller}"),
            Step::Effect { caller, outcome } => format!("effect {caller} {}", outcome_name(outcome)),
            Step::Note { caller } => format!("note {caller}"),
            Step::Lapse { caller } => format!("lapse {caller}"),
            Step::Crash { caller } => format!("crash {caller}"),
            Step::Finish { caller } => format!("finish {caller}"),
            Step::Sweep { aged } => format!("sweep {}", if aged { "aged" } else { "fresh" }),
            Step::Retire { key } => format!("retire {key}"),
        }
    }

    /// A step read back from [`Step::line`].
    pub fn parse(line: &str) -> Option<Step> {
        let words: Vec<&str> = line.split_whitespace().collect();
        let num = |i: usize, bound: usize| words.get(i).and_then(|w| w.parse::<usize>().ok()).filter(|n| *n < bound);
        Some(match words.first()? {
            &"begin" if words.len() == 3 => Step::Begin { caller: num(1, CALLERS)?, key: num(2, KEYS)? },
            &"advance" if words.len() == 2 => Step::Advance { caller: num(1, CALLERS)? },
            &"effect" if words.len() == 3 => Step::Effect {
                caller: num(1, CALLERS)?,
                outcome: match words[2] {
                    "inline" => Outcome::Inline,
                    "blob" => Outcome::Blob,
                    "fail" => Outcome::Fail,
                    "unjournaled" => Outcome::Unjournaled,
                    _ => return None,
                },
            },
            &"note" if words.len() == 2 => Step::Note { caller: num(1, CALLERS)? },
            &"lapse" if words.len() == 2 => Step::Lapse { caller: num(1, CALLERS)? },
            &"crash" if words.len() == 2 => Step::Crash { caller: num(1, CALLERS)? },
            &"finish" if words.len() == 2 => Step::Finish { caller: num(1, CALLERS)? },
            &"sweep" if words.len() == 2 => Step::Sweep {
                aged: match words[1] {
                    "aged" => true,
                    "fresh" => false,
                    _ => return None,
                },
            },
            &"retire" if words.len() == 2 => Step::Retire { key: num(1, KEYS)? },
            _ => return None,
        })
    }
}

fn outcome_name(o: Outcome) -> &'static str {
    match o {
        Outcome::Inline => "inline",
        Outcome::Blob => "blob",
        Outcome::Fail => "fail",
        Outcome::Unjournaled => "unjournaled",
    }
}

/// A value a caller's effect produced: its caller, and whether it lives in a blob.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Value {
    pub origin: usize,
    pub blob: bool,
}

impl Value {
    /// The value's bytes: `value-<caller>` inline, or a run of one byte past the cutoff.
    pub fn bytes(self) -> Vec<u8> {
        if self.blob {
            vec![b'a' + self.origin as u8; INLINE_CUTOFF_BYTES + 1]
        } else {
            format!("value-{}", self.origin).into_bytes()
        }
    }

    fn of_bytes(bytes: &[u8]) -> Option<Value> {
        let blob = bytes.len() > INLINE_CUTOFF_BYTES;
        (0..CALLERS).map(|origin| Value { origin, blob }).find(|v| v.bytes() == bytes)
    }

    /// The sha256 of caller `origin`'s blob value, hashed once.
    pub fn sha(origin: usize) -> &'static str {
        static SHAS: std::sync::OnceLock<Vec<String>> = std::sync::OnceLock::new();
        &SHAS.get_or_init(|| (0..CALLERS).map(|origin| sha256_hex(&Value { origin, blob: true }.bytes())).collect())[origin]
    }

    fn of_sha(sha: &str) -> Option<Value> {
        (0..CALLERS).find(|origin| Value::sha(*origin) == sha).map(|origin| Value { origin, blob: true })
    }

    fn of_stored(stored: &Stored) -> Option<Value> {
        match stored.blob() {
            Some(sha) => Value::of_sha(sha),
            None => Value::of_bytes(&stored.inline_bytes()?),
        }
    }

    fn show(self) -> String {
        format!("{}{}", if self.blob { "b" } else { "i" }, self.origin)
    }
}

/// A key's row, projected.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ModelRow {
    Pending { holder: usize, attempts: u32 },
    Recorded(Value),
}

/// Why a caller releases its claim.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Release {
    Failed,
    Unrecorded,
}

/// How a step resolved.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Resolution {
    Replayed(Value),
    Recorded(Value),
    Unrecorded,
    Failed,
    /// `BlobMissing`.
    Missing,
}

/// Where a caller stands: idle, waiting on its next port call, running its effect, or
/// resolved.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Phase {
    Idle,
    /// Next: `create_pending`.
    Claiming(usize),
    /// Next: `read`, before the effect.
    Reading(usize),
    /// Next: `replace_if_pending` naming the lapsed holder.
    TakingOver(usize, usize),
    /// Next: the effect.
    Claimed(usize),
    /// Next: `read`, after the effect returned the value.
    Checking(usize, Value),
    /// Next: `put` of the value's blob.
    Storing(usize, Value),
    /// Next: `record`.
    Recording(usize, Value),
    /// Next: `release`.
    Releasing(usize, Release),
    /// Next: `get` of the recorded value's blob.
    Loading(usize, Value),
    Done(usize, Resolution),
}

impl Phase {
    /// The key a non-idle caller works on.
    pub fn key(self) -> Option<usize> {
        match self {
            Phase::Idle => None,
            Phase::Claiming(k) | Phase::Reading(k) | Phase::TakingOver(k, _) | Phase::Claimed(k) | Phase::Releasing(k, _) | Phase::Done(k, _) => Some(k),
            Phase::Checking(k, _) | Phase::Storing(k, _) | Phase::Recording(k, _) | Phase::Loading(k, _) => Some(k),
        }
    }

    fn waits_on_port(self) -> bool {
        !matches!(self, Phase::Idle | Phase::Claimed(_) | Phase::Done(..))
    }

    fn show(self) -> String {
        match self {
            Phase::Idle => "idle".into(),
            Phase::Claiming(k) => format!("claiming {k}"),
            Phase::Reading(k) => format!("reading {k}"),
            Phase::TakingOver(k, h) => format!("taking-over {k} {h}"),
            Phase::Claimed(k) => format!("claimed {k}"),
            Phase::Checking(k, v) => format!("checking {k} {}", v.show()),
            Phase::Storing(k, v) => format!("storing {k} {}", v.show()),
            Phase::Recording(k, v) => format!("recording {k} {}", v.show()),
            Phase::Releasing(k, Release::Failed) => format!("releasing {k} failed"),
            Phase::Releasing(k, Release::Unrecorded) => format!("releasing {k} unrecorded"),
            Phase::Loading(k, v) => format!("loading {k} {}", v.show()),
            Phase::Done(k, Resolution::Replayed(v)) => format!("done {k} replayed {}", v.show()),
            Phase::Done(k, Resolution::Recorded(v)) => format!("done {k} recorded {}", v.show()),
            Phase::Done(k, Resolution::Unrecorded) => format!("done {k} unrecorded"),
            Phase::Done(k, Resolution::Failed) => format!("done {k} failed"),
            Phase::Done(k, Resolution::Missing) => format!("done {k} missing"),
        }
    }
}

/// The entry key of case key `k`: the one step of execution `x-<k>`.
pub fn entry_key(k: usize) -> EntryKey {
    EntryKey::new(&format!("x-{k}"), "step", b"null")
}

fn key_index(key: &EntryKey) -> usize {
    (0..KEYS).find(|k| entry_key(*k) == *key).expect("a gated call names a case key")
}

fn caller_index(run_id: &str) -> usize {
    run_id.strip_prefix("run-").and_then(|n| n.parse().ok()).expect("a gated call names a case caller")
}

/// A port call a caller waits on.
#[derive(Clone, Copy, Debug)]
enum Call {
    CreatePending(usize),
    Read(usize),
    Replace(usize, usize),
    Record(usize),
    Release(usize),
    Put,
    Get(Value),
}

enum Event {
    Waiting(Call),
    Effect,
    Done(Result<Resolved, StepError>),
    Crashed,
}

enum Cmd {
    Go,
    Crash,
    Effect(Outcome),
}

/// The unwinding payload a crashed caller's thread carries.
struct Crashed;

struct Gate {
    events: Sender<Event>,
    cmds: Receiver<Cmd>,
}

thread_local! {
    static GATE: RefCell<Option<Gate>> = const { RefCell::new(None) };
}

/// On a caller's thread, report `call` and wait for the scheduler to admit it; elsewhere,
/// pass straight through.
fn admit(call: impl FnOnce() -> Call) {
    let crashed = GATE.with(|g| {
        let g = g.borrow();
        let Some(g) = g.as_ref() else { return false };
        if g.events.send(Event::Waiting(call())).is_err() {
            return true;
        }
        !matches!(g.cmds.recv(), Ok(Cmd::Go))
    });
    if crashed {
        std::panic::resume_unwind(Box::new(Crashed));
    }
}

/// An adapter whose every call from a caller's thread waits for the scheduler.
#[derive(Clone)]
pub struct Gated<T>(pub T);

impl<J: JournalStore> JournalStore for Gated<J> {
    fn create_pending(&self, key: &EntryKey, run_id: &str) -> Result<bool, Failure> {
        admit(|| Call::CreatePending(key_index(key)));
        self.0.create_pending(key, run_id)
    }
    fn read(&self, key: &EntryKey) -> Result<Option<Row>, Failure> {
        admit(|| Call::Read(key_index(key)));
        self.0.read(key)
    }
    fn replace_if_pending(&self, key: &EntryKey, holder: &str, run_id: &str) -> Result<bool, Failure> {
        admit(|| Call::Replace(key_index(key), caller_index(holder)));
        self.0.replace_if_pending(key, holder, run_id)
    }
    fn record(&self, key: &EntryKey, value: &Stored) -> Result<Option<Stored>, Failure> {
        admit(|| Call::Record(key_index(key)));
        self.0.record(key, value)
    }
    fn release(&self, key: &EntryKey, run_id: &str) -> Result<(), Failure> {
        admit(|| Call::Release(key_index(key)));
        self.0.release(key, run_id)
    }
    fn note_attempts(&self, key: &EntryKey, run_id: &str, attempts: u32) -> Result<(), Failure> {
        self.0.note_attempts(key, run_id, attempts)
    }
    fn rows(&self, execution_id: &str) -> Result<Vec<Row>, Failure> {
        self.0.rows(execution_id)
    }
    fn retire(&self, execution_id: &str) -> Result<(), Failure> {
        self.0.retire(execution_id)
    }
    fn executions(&self) -> Result<Vec<String>, Failure> {
        self.0.executions()
    }
}

impl<B: BlobStore> BlobStore for Gated<B> {
    fn put(&self, sha256: &str, bytes: &[u8]) -> Result<(), Failure> {
        admit(|| Call::Put);
        self.0.put(sha256, bytes)
    }
    fn get(&self, sha256: &str) -> Result<Option<Vec<u8>>, Failure> {
        admit(|| Call::Get(Value::of_sha(sha256).expect("a gated get names a case blob")));
        self.0.get(sha256)
    }
    fn sweep(&self, referenced: &[String], now_unix: i64) -> Result<Vec<String>, Failure> {
        self.0.sweep(referenced, now_unix)
    }
    fn swept_at(&self) -> Result<Option<i64>, Failure> {
        self.0.swept_at()
    }
    fn mark_swept(&self, at: i64) -> Result<(), Failure> {
        self.0.mark_swept(at)
    }
}

struct Never;

impl Cancellation for Never {
    fn requested(&self) -> bool {
        false
    }
}

struct Caller {
    phase: Phase,
    /// The value the caller's effect returned, while the step still runs.
    value: Option<Value>,
    release: Release,
    lapsed: bool,
    thread: Option<(JoinHandle<()>, Receiver<Event>, Sender<Cmd>)>,
}

/// A running case over one adapter pair.
pub struct Interleaving<J: JournalStore + Clone + 'static, B: BlobStore + Clone + 'static> {
    journal: Journal<Gated<J>, Gated<B>>,
    callers: Vec<Caller>,
    lapsed: Arc<Mutex<[bool; CALLERS]>>,
    /// Whether a sweep past the grace window spares the blob of every caller between its put
    /// and its record: the grace hypothesis the model's `no_dangling_blob` assumes.
    grace: bool,
    /// Effects run so far.
    effects: usize,
}

impl<J: JournalStore + Clone + 'static, B: BlobStore + Clone + 'static> Interleaving<J, B> {
    /// A case over empty `rows` and `blobs`, with or without the grace hypothesis.
    pub fn new(rows: J, blobs: B, grace: bool) -> Self {
        let callers = (0..CALLERS).map(|_| Caller { phase: Phase::Idle, value: None, release: Release::Failed, lapsed: false, thread: None }).collect();
        Interleaving { journal: Journal::over(Gated(rows), Gated(blobs)), callers, lapsed: Arc::new(Mutex::new([false; CALLERS])), grace, effects: 0 }
    }

    /// Where `caller` stands.
    pub fn phase(&self, caller: usize) -> Phase {
        self.callers[caller].phase
    }

    /// Whether `caller`'s owner lease lapsed.
    pub fn lapsed(&self, caller: usize) -> bool {
        self.callers[caller].lapsed
    }

    /// Effects the case ran.
    pub fn effects(&self) -> usize {
        self.effects
    }

    /// Key `k`'s row.
    pub fn row(&self, k: usize) -> Option<ModelRow> {
        match self.journal.row(&entry_key(k)).expect("the adapter reads a row") {
            None => None,
            Some(Row::Pending { run_id, attempts, .. }) => Some(ModelRow::Pending { holder: caller_index(&run_id), attempts }),
            Some(Row::Recorded { value, .. }) => Some(ModelRow::Recorded(Value::of_stored(&value).expect("a recorded row holds a case value"))),
        }
    }

    /// The rows the adapter lists under key `k`'s execution.
    pub fn listed(&self, k: usize) -> usize {
        self.journal.row_store().rows(&entry_key(k).execution_id).expect("the adapter lists rows").len()
    }

    /// Whether the blob of `caller`'s value is stored.
    pub fn blob(&self, caller: usize) -> bool {
        self.journal.blob_store().get(Value::sha(caller)).expect("the adapter reads a blob").is_some()
    }

    /// Run one step; a step its guard refuses leaves the case unchanged.
    pub fn apply(&mut self, step: Step) {
        match step {
            Step::Begin { caller, key } if self.callers[caller].phase == Phase::Idle => self.begin(caller, key),
            Step::Advance { caller } if self.callers[caller].phase.waits_on_port() => self.send(caller, Cmd::Go),
            Step::Effect { caller, outcome } if matches!(self.callers[caller].phase, Phase::Claimed(_)) => {
                let c = &mut self.callers[caller];
                c.value = match outcome {
                    Outcome::Inline => Some(Value { origin: caller, blob: false }),
                    Outcome::Blob => Some(Value { origin: caller, blob: true }),
                    Outcome::Fail | Outcome::Unjournaled => None,
                };
                c.release = if outcome == Outcome::Fail { Release::Failed } else { Release::Unrecorded };
                self.effects += 1;
                self.send(caller, Cmd::Effect(outcome));
            }
            Step::Note { caller } => {
                if let Phase::Claimed(k) = self.callers[caller].phase {
                    let attempts = match self.row(k) {
                        Some(ModelRow::Pending { holder, attempts }) if holder == caller => attempts + 1,
                        _ => 1,
                    };
                    if attempts <= MAX_ATTEMPTS {
                        self.journal.note_attempts(&entry_key(k), &format!("run-{caller}"), attempts).expect("the adapter notes attempts");
                    }
                }
            }
            Step::Lapse { caller } => {
                self.callers[caller].lapsed = true;
                self.lapsed.lock().expect("the lapse set")[caller] = true;
            }
            Step::Crash { caller } => match self.callers[caller].phase {
                Phase::Idle => {}
                Phase::Done(..) => self.finish(caller),
                _ => self.send(caller, Cmd::Crash),
            },
            Step::Finish { caller } if matches!(self.callers[caller].phase, Phase::Done(..)) => self.finish(caller),
            Step::Sweep { aged } => {
                // Under the grace hypothesis, a blob between its put and its record is younger
                // than the window, so an aged pass spares it as the model's `inFlight` does.
                let in_flight: Vec<String> = (0..CALLERS)
                    .filter(|c| self.grace && matches!(self.callers[*c].phase, Phase::Recording(_, v) if v.blob && v.origin == *c))
                    .map(|c| Value::sha(c).to_string())
                    .collect();
                let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0);
                let past = if aged { i64::try_from(BLOB_SWEEP_GRACE_SECS).unwrap_or(i64::MAX) + 60 } else { 0 };
                self.journal.sweep(&in_flight, now + past).expect("the adapter sweeps");
            }
            Step::Retire { key } if self.callers.iter().all(|c| c.phase.key() != Some(key)) => {
                self.journal.collect(&entry_key(key).execution_id).expect("the adapter retires an execution");
            }
            _ => {}
        }
    }

    fn begin(&mut self, caller: usize, key: usize) {
        let (events, events_rx) = channel();
        let (cmds, cmds_rx) = channel();
        let journal = self.journal.clone();
        let lapsed = self.lapsed.clone();
        let handle = std::thread::spawn(move || {
            let done = events.clone();
            GATE.with(|g| *g.borrow_mut() = Some(Gate { events, cmds: cmds_rx }));
            let holder_live = |holder: &str| -> Result<bool, Failure> { Ok(!lapsed.lock().expect("the lapse set")[caller_index(holder)]) };
            let mut effect = || -> Result<Vec<u8>, Failure> {
                let outcome = GATE.with(|g| {
                    let g = g.borrow();
                    let g = g.as_ref().expect("a caller thread holds its gate");
                    let _ = g.events.send(Event::Effect);
                    match g.cmds.recv() {
                        Ok(Cmd::Effect(o)) => Some(o),
                        _ => None,
                    }
                });
                match outcome {
                    Some(Outcome::Inline) => Ok(Value { origin: caller, blob: false }.bytes()),
                    Some(Outcome::Blob) => Ok(Value { origin: caller, blob: true }.bytes()),
                    Some(Outcome::Fail) => Err(Failure::new(FailureTag::Transient, "the case's effect failed")),
                    Some(Outcome::Unjournaled) => Ok(b"unjournaled".to_vec()),
                    None => std::panic::resume_unwind(Box::new(Crashed)),
                }
            };
            let journal_it = |v: &[u8]| v != b"unjournaled";
            let run = format!("run-{caller}");
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| journal.step(&entry_key(key), &run, &holder_live, &Never, &journal_it, &mut effect)));
            let _ = done.send(match result {
                Ok(r) => Event::Done(r),
                Err(_) => Event::Crashed,
            });
        });
        let c = &mut self.callers[caller];
        c.thread = Some((handle, events_rx, cmds));
        c.value = None;
        c.phase = Phase::Claiming(key);
        self.observe(caller);
    }

    fn send(&mut self, caller: usize, cmd: Cmd) {
        if let Some((_, _, cmds)) = &self.callers[caller].thread {
            cmds.send(cmd).expect("a waiting caller takes its command");
        }
        self.observe(caller);
    }

    /// Wait for `caller`'s next event and project it onto its phase.
    fn observe(&mut self, caller: usize) {
        let c = &mut self.callers[caller];
        let Some((_, events, _)) = &c.thread else { return };
        let k = c.phase.key().expect("a running caller works on a key");
        let event = events.recv().expect("a caller thread reports before it ends");
        c.phase = match event {
            Event::Waiting(Call::CreatePending(k)) => Phase::Claiming(k),
            Event::Waiting(Call::Read(k)) => match c.value {
                Some(v) => Phase::Checking(k, v),
                None => Phase::Reading(k),
            },
            Event::Waiting(Call::Replace(k, h)) => Phase::TakingOver(k, h),
            Event::Waiting(Call::Record(k)) => Phase::Recording(k, c.value.expect("a recording caller holds its value")),
            Event::Waiting(Call::Release(k)) => Phase::Releasing(k, c.release),
            Event::Waiting(Call::Put) => Phase::Storing(k, c.value.expect("a storing caller holds its value")),
            Event::Waiting(Call::Get(w)) => Phase::Loading(k, w),
            Event::Effect => Phase::Claimed(k),
            Event::Done(r) => Phase::Done(
                k,
                match r {
                    Ok(Resolved::Replayed(b)) => Resolution::Replayed(Value::of_bytes(&b).expect("a replayed case value")),
                    Ok(Resolved::Recorded(b)) => Resolution::Recorded(Value::of_bytes(&b).expect("a recorded case value")),
                    Ok(Resolved::Unrecorded(_)) => Resolution::Unrecorded,
                    Err(StepError::Failed(_)) => Resolution::Failed,
                    Err(StepError::Journal(RunError::BlobMissing(_))) => Resolution::Missing,
                    Err(e) => panic!("caller {caller} on key {k}: {e:?}"),
                },
            ),
            Event::Crashed => Phase::Idle,
        };
        if matches!(c.phase, Phase::Done(..) | Phase::Idle) {
            c.value = None;
            if let Some((handle, _, _)) = c.thread.take() {
                handle.join().expect("a caller thread ends cleanly");
            }
        }
    }

    fn finish(&mut self, caller: usize) {
        self.callers[caller].phase = Phase::Idle;
    }

    /// The state in the model executable's projection: each key's row, each caller's
    /// blob, and each caller's phase and lease.
    pub fn show(&self) -> String {
        let rows = (0..KEYS)
            .map(|k| match self.row(k) {
                None => format!("k{k}=none"),
                Some(ModelRow::Pending { holder, attempts }) => format!("k{k}=pending {holder} {attempts}"),
                Some(ModelRow::Recorded(v)) => format!("k{k}=recorded {}", v.show()),
            })
            .collect::<Vec<_>>()
            .join(" ");
        let blobs = (0..CALLERS).map(|c| if self.blob(c) { "1" } else { "0" }).collect::<String>();
        let callers = (0..CALLERS).map(|c| format!("c{c}({} lapsed={})", self.callers[c].phase.show(), self.callers[c].lapsed)).collect::<Vec<_>>().join(" ");
        format!("rows[{rows}] blobs={blobs} {callers}")
    }
}

impl<J: JournalStore + Clone + 'static, B: BlobStore + Clone + 'static> Drop for Interleaving<J, B> {
    fn drop(&mut self) {
        for c in &mut self.callers {
            if let Some((handle, _, cmds)) = c.thread.take() {
                let _ = cmds.send(Cmd::Crash);
                drop(cmds);
                let _ = handle.join();
            }
        }
    }
}
