//! `surface.dispatch`: steps on remote workers, their heartbeats and attempt-fenced callbacks
//! through the relay.

use contextful_core::ports::Clock;
use contextful_core::surface::worker::{Signed, StepOutcome, StepResult, HEARTBEAT_BEAT_SECS, HEARTBEAT_LAPSE_SECS};
use contextful_core::time::Instant;
use contextful_engine::scheduler::Dispatch;
use contextful_engine::worker::{Relay, Submission, WorkerClient, WorkerDispatch};
use std::collections::BTreeMap;
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::time::Duration;

const KEY: &[u8] = b"deployment-worker-key";

/// A clock the test moves.
#[derive(Clone)]
struct Stepped(Arc<Mutex<Instant>>);

impl Stepped {
    fn new() -> Stepped {
        Stepped(Arc::new(Mutex::new(Instant::parse("2030-01-01T00:00:00Z").unwrap())))
    }
    fn advance(&self, secs: u64) {
        let mut t = self.0.lock().unwrap();
        *t = t.plus_secs(secs);
    }
}

impl Clock for Stepped {
    fn now(&self) -> Instant {
        *self.0.lock().unwrap()
    }
}

/// Workers that record each submission and accept, except those listed as down.
struct Fleet {
    sent: Mutex<Sender<(String, Submission)>>,
    down: Mutex<Vec<String>>,
    tried: Mutex<Vec<String>>,
}

impl WorkerClient for Fleet {
    fn submit(&self, worker: &str, s: &Submission) -> Result<(), String> {
        self.tried.lock().unwrap().push(worker.to_string());
        if self.down.lock().unwrap().iter().any(|w| w == worker) {
            return Err(format!("{worker}: connection refused"));
        }
        self.sent.lock().unwrap().send((worker.to_string(), s.clone())).unwrap();
        Ok(())
    }
}

struct Rig {
    clock: Stepped,
    relay: Arc<Relay>,
    dispatch: Arc<WorkerDispatch>,
    fleet: Arc<Fleet>,
    received: Receiver<(String, Submission)>,
}

fn rig(workers: &[&str]) -> Rig {
    let clock = Stepped::new();
    let relay = Arc::new(Relay::new(KEY.to_vec(), workers.iter().map(|w| w.to_string()).collect(), Arc::new(clock.clone())));
    let (tx, received) = channel();
    let fleet = Arc::new(Fleet { sent: Mutex::new(tx), down: Mutex::default(), tried: Mutex::default() });
    let dispatch = Arc::new(WorkerDispatch {
        relay: relay.clone(),
        client: fleet.clone(),
        callback_base: "http://127.0.0.1:8788".into(),
        check: Duration::from_millis(5),
    });
    Rig { clock, relay, dispatch, fleet, received }
}

fn token(s: &Submission) -> String {
    s.callback.rsplit('/').next().unwrap().to_string()
}

fn headers(s: &Submission, attempt: u32, at: Instant, key: &[u8]) -> BTreeMap<&'static str, String> {
    Signed { run: s.run.clone(), step: s.step.clone(), attempt, timestamp: at.unix_secs() }.headers(key).into_iter().collect()
}

fn next(rig: &Rig) -> (String, Submission) {
    rig.received.recv_timeout(Duration::from_secs(10)).expect("a submission")
}

/// A worker heartbeats every 15 s with a signed `GET` on its step's awakeable route, and posts its outcome to
/// that route with a signed `POST`.
// spec: surface.dispatch.heartbeat-beat@db450e3f
#[test]
fn a_beating_worker_keeps_its_step_and_its_callback_completes_it() {
    let rig = rig(&["w1", "w2"]);
    let d = rig.dispatch.clone();
    let run = std::thread::spawn(move || d.step("orders-1", "orders", 3));
    let (worker, s) = next(&rig);
    assert_eq!((worker.as_str(), s.attempt, s.version, s.idempotency_key.as_str()), ("w1", 1, 3, "orders-1/orders/1"));
    assert!(s.callback.starts_with("http://127.0.0.1:8788/awake/"), "{}", s.callback);
    let t = token(&s);
    for _ in 0..8 {
        rig.clock.advance(HEARTBEAT_BEAT_SECS);
        let h = headers(&s, 1, rig.clock.now(), KEY);
        assert_eq!(rig.relay.heartbeat(&t, &|k| h.get(k).cloned()).unwrap(), 1);
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(rig.received.try_recv().is_err(), "a beating worker keeps its step for {} s", 8 * HEARTBEAT_BEAT_SECS);
    // A heartbeat signed under another key changes nothing.
    let forged = headers(&s, 1, rig.clock.now(), b"other");
    assert_eq!(rig.relay.heartbeat(&t, &|k| forged.get(k).cloned()).unwrap_err().status(), 409);
    assert_eq!(rig.relay.heartbeat("ffff", &|k| forged.get(k).cloned()).unwrap_err().status(), 404);
    let done = StepOutcome::Done(StepResult::CatalogRow("orders-1".into()));
    let h = headers(&s, 1, rig.clock.now(), KEY);
    assert_eq!(rig.relay.callback(&t, &|k| h.get(k).cloned(), &done.encode()).unwrap(), done);
    assert_eq!(run.join().unwrap().unwrap(), done);
}

/// A heartbeat lapse past 60 s reschedules that worker's in-flight work onto another matching worker under
/// the next attempt number.
// spec: surface.dispatch.heartbeat-lapse@15bf5766
#[test]
fn killing_a_worker_mid_step_reschedules_it_once_and_fences_the_late_callback() {
    let rig = rig(&["w1", "w2"]);
    let d = rig.dispatch.clone();
    let run = std::thread::spawn(move || d.fire("orders", &[], 7));
    let (worker, first) = next(&rig);
    assert_eq!((worker.as_str(), first.attempt), ("w1", 1));
    let t = token(&first);
    let beat = headers(&first, 1, rig.clock.now(), KEY);
    rig.relay.heartbeat(&t, &|k| beat.get(k).cloned()).unwrap();

    // w1 dies: exactly 60 s of silence keeps the step; one more second moves it, once.
    rig.clock.advance(HEARTBEAT_LAPSE_SECS);
    std::thread::sleep(Duration::from_millis(50));
    assert!(rig.received.try_recv().is_err());
    rig.clock.advance(1);
    let (worker, second) = next(&rig);
    assert_eq!((worker.as_str(), second.attempt, second.idempotency_key.as_str()), ("w2", 2, &*format!("{}/orders/2", first.run)));
    assert_eq!(token(&second), t, "the step keeps its route across attempts");
    std::thread::sleep(Duration::from_millis(50));
    assert!(rig.received.try_recv().is_err(), "rescheduled once");

    // w1's late attempt-1 callback is rejected and changes no step.
    let late = headers(&first, 1, rig.clock.now(), KEY);
    let stale = StepOutcome::Done(StepResult::CatalogRow("stale".into()));
    let e = rig.relay.callback(&t, &|k| late.get(k).cloned(), &stale.encode()).unwrap_err();
    assert_eq!(e.status(), 409);
    assert!(e.to_string().starts_with("DispatchCallbackRejected:") && e.to_string().contains("superseded by attempt 2"), "{e}");

    let won = StepOutcome::Done(StepResult::CatalogRow("orders-a2".into()));
    let h = headers(&second, 2, rig.clock.now(), KEY);
    rig.relay.callback(&t, &|k| h.get(k).cloned(), &won.encode()).unwrap();
    let line = run.join().unwrap().unwrap();
    assert!(line.contains("orders-a2") && !line.contains("stale"), "{line}");

    // Once the step has closed, a late callback still meets the attempt fence, not an unknown route.
    let e = rig.relay.callback(&t, &|k| late.get(k).cloned(), &stale.encode()).unwrap_err();
    assert_eq!(e.status(), 409, "{e}");
    assert!(e.to_string().contains("superseded by attempt 2"), "{e}");
    let e = rig.relay.callback(&t, &|k| h.get(k).cloned(), &won.encode()).unwrap_err();
    assert!(e.status() == 409 && e.to_string().contains("closed under attempt 2"), "{e}");
    let beat = headers(&first, 1, rig.clock.now(), KEY);
    assert_eq!(rig.relay.heartbeat(&t, &|k| beat.get(k).cloned()).unwrap_err().status(), 409);
}

/// A step every listed worker refuses in turn at `POST /submit` fails its unit naming the last refusal, each
/// worker tried once.
// spec: surface.dispatch.submit-exhausted@68b82cfd
#[test]
fn a_step_every_worker_refuses_fails_after_one_try_each() {
    let rig = rig(&["w1", "w2", "w3"]);
    rig.fleet.down.lock().unwrap().extend(["w1".to_string(), "w2".to_string(), "w3".to_string()]);
    let d = rig.dispatch.clone();
    let (tx, rx) = channel();
    std::thread::spawn(move || tx.send(d.fire("orders", &[], 1)).unwrap());
    let e = rx.recv_timeout(Duration::from_secs(10)).expect("the step ends").unwrap_err();
    assert!(e.contains("no worker took it") && e.contains("w3: connection refused"), "{e}");
    assert_eq!(*rig.fleet.tried.lock().unwrap(), ["w1", "w2", "w3"]);
}

/// A worker refusing the submission moves the step to the next worker at once; a failed
/// outcome fails the unit.
#[test]
fn an_unreachable_worker_passes_the_step_on_and_a_failed_outcome_fails_the_unit() {
    let rig = rig(&["w1", "w2"]);
    rig.fleet.down.lock().unwrap().push("w1".into());
    let d = rig.dispatch.clone();
    let run = std::thread::spawn(move || d.fire("orders", &[], 1));
    let (worker, s) = next(&rig);
    assert_eq!((worker.as_str(), s.attempt), ("w2", 2));
    let failed = StepOutcome::Failed { failed: "LeaseHeld: orders".into() };
    let h = headers(&s, 2, rig.clock.now(), KEY);
    rig.relay.callback(&token(&s), &|k| h.get(k).cloned(), &failed.encode()).unwrap();
    assert_eq!(run.join().unwrap().unwrap_err(), "orders: LeaseHeld: orders");
}

/// A head entry's landing steps run in order as steps of one run.
#[test]
fn a_dependent_run_submits_its_steps_in_order_under_one_run() {
    let rig = rig(&["w1"]);
    let d = rig.dispatch.clone();
    let run = std::thread::spawn(move || d.fire("orders", &["orders-enrich".to_string()], 1));
    let mut runs = Vec::new();
    for step in ["orders", "orders-enrich"] {
        let (_, s) = next(&rig);
        assert_eq!(s.step, step);
        runs.push(s.run.clone());
        let h = headers(&s, 1, rig.clock.now(), KEY);
        rig.relay.callback(&token(&s), &|k| h.get(k).cloned(), &StepOutcome::Done(StepResult::Pointer(step.into())).encode()).unwrap();
    }
    assert_eq!(runs[0], runs[1]);
    run.join().unwrap().unwrap();
}

#[test]
fn a_failed_head_still_submits_its_child_and_reports_the_failure() {
    let rig = rig(&["w1"]);
    let d = rig.dispatch.clone();
    let run = std::thread::spawn(move || d.fire("parent", &["child".to_string()], 1));
    let (_, parent) = next(&rig);
    assert_eq!(parent.step, "parent");
    let h = headers(&parent, 1, rig.clock.now(), KEY);
    rig.relay.callback(&token(&parent), &|k| h.get(k).cloned(), &StepOutcome::Failed { failed: "parent failed".into() }.encode()).unwrap();
    let (_, child) = next(&rig);
    assert_eq!(child.step, "child");
    let h = headers(&child, 1, rig.clock.now(), KEY);
    rig.relay.callback(&token(&child), &|k| h.get(k).cloned(), &StepOutcome::Done(StepResult::Pointer("child".into())).encode()).unwrap();
    assert!(run.join().unwrap().unwrap_err().contains("parent: parent failed"));
}
