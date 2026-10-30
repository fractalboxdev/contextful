//! `run.retry.not-before`: a rate-limited step's next-attempt instant, durable beside its
//! attempt count, the waiting attempt it closes on, and the wake that resumes it.

use crate::support::{at, Rig, T0};
use contextful_core::connector::{lease, meter, reference::SecretName};
use contextful_core::run::drive::{Emitted, InputRow, InputSet, RowBody, RowCalls, RowStop, StoreInput};
use contextful_core::run::journal::EntryKey;
use contextful_core::run::own::{OwnerPins, OwnerScope, PlanPins};
use contextful_core::run::ports::{ExecutionPort, Landed, OpenExecution, Outcome};
use contextful_core::run::record::RunStatus;
use contextful_core::run::retry::{NotBefore, Schedule, ScheduleSpec};
use contextful_core::run::{Failure, FailureTag, RunError};
use contextful_core::surface::arm::Schedule as Cadence;
use contextful_engine::cancel::{Cadence as KeeperCadence, Keeper};
use contextful_engine::drive::{job_scope, Drive, ItemPolicy, ItemReport, ItemStatus};
use contextful_engine::scheduler::{Dispatch, Entry, Scheduler};
use contextful_engine::{Engine, EngineError, Journal, LocalCatalog};
use contextful_engine::stores::{FileBlobStore, FileJournalStore};
use serde_json::json;
use std::collections::BTreeMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// Five attempts, no jitter, a 1 ms computed delay.
fn schedule(attempts: u32) -> Schedule {
    ScheduleSpec { backoff: Some("fixed".into()), base_ms: Some(1), attempts: Some(attempts), jitter_ms: Some(0), ..ScheduleSpec::default() }.compile().unwrap()
}

/// A body whose `reserve` call spends one meter permit and whose `paid` call the vendor
/// throttles for `wait_secs` on the first `throttled` attempts for item `limited`.
struct Throttled {
    limited: &'static str,
    wait_secs: u64,
    throttled: usize,
    reserved: Mutex<Vec<String>>,
    paid: Mutex<Vec<String>>,
}

impl Throttled {
    fn new(limited: &'static str, wait_secs: u64, throttled: usize) -> Throttled {
        Throttled { limited, wait_secs, throttled, reserved: Mutex::default(), paid: Mutex::default() }
    }

    fn count(list: &Mutex<Vec<String>>, id: &str) -> usize {
        list.lock().unwrap().iter().filter(|p| *p == id).count()
    }
}

impl RowBody for Throttled {
    fn run(&self, row: &InputRow, calls: &dyn RowCalls) -> Result<Emitted, RowStop> {
        let id = row.row["id"].as_str().unwrap().to_string();
        calls.call("reserve", id.as_bytes(), &mut |_| {
            self.reserved.lock().unwrap().push(id.clone());
            Ok(b"permit".to_vec())
        })?;
        calls.call("paid", id.as_bytes(), &mut |_| {
            let mut paid = self.paid.lock().unwrap();
            paid.push(id.clone());
            if id == self.limited && paid.iter().filter(|p| **p == id).count() <= self.throttled {
                return Err(Failure::new(FailureTag::RateLimited, "the vendor answered 429").with_retry_after(self.wait_secs));
            }
            Ok(id.as_bytes().to_vec())
        })?;
        Ok(BTreeMap::from([("output".into(), vec![row.row.clone()])]))
    }
}

fn fire<'a>(body: &'a Throttled, run: &str, max_in_flight: usize) -> Drive<'a> {
    Drive {
        job: "score".into(),
        input: StoreInput { body: "score".into(), statement: "select * from docs".into(), as_of: None, sweep: None, structural_identity: None },
        body,
        max_in_flight,
        run_id: run.into(),
        site_id: "site".into(),
        pid: 42,
        boot_id: "boot".into(),
        schedule: schedule(5),
        poll: Duration::ZERO,
    }
}

/// An item budget of one failed attempt: a waiting item that spent it would exhaust.
fn policy() -> ItemPolicy {
    ItemPolicy { logical_key: "id".into(), entity_key: "entity".into(), entity_namespace: "documents".into(), max_attempts: 1 }
}

fn input(ids: &[&str]) -> InputSet {
    InputSet { as_of: T0.into(), snapshots: BTreeMap::new(), rows: ids.iter().map(|id| json!({"id": id, "entity": id}).as_object().unwrap().clone()).collect() }
}

fn drive(rig: &Rig, body: &Throttled, run: &str, ids: &[&str], max_in_flight: usize, landed: &Mutex<Vec<String>>) -> ItemReport {
    rig.engine
        .drive_items(&fire(body, run, max_in_flight), &policy(), &mut |_| Ok(input(ids)), &mut |_, out: &Emitted| {
            landed.lock().unwrap().push(out["output"][0]["id"].as_str().unwrap().to_string());
            Ok(Landed { rows: 1, bytes: 1 })
        })
        .unwrap()
}

fn waiting(report: &ItemReport, id_count: usize) -> Vec<contextful_core::time::Instant> {
    assert_eq!(report.items.len(), id_count);
    report.items.iter().filter_map(|i| match i.status { ItemStatus::Waiting { next_attempt_at } => Some(next_attempt_at), _ => None }).collect()
}

/// A wait past the ceiling closes the attempt `waiting` with its instant; a fire before
/// that instant waits again without calling the vendor, and the first fire at or after it
/// re-enters the step under the same work key.
// spec: run.retry.waiting-state@37adfa96
// spec: run.retry.resume-at@4b7d88f4
#[test]
fn a_reschedule_above_the_ceiling_resumes_at_the_stored_time() {
    let rig = Rig::new();
    let body = Throttled::new("doc", 3600, 1);
    let landed = Mutex::default();
    let due = at(T0).plus_secs(3600);

    let first = drive(&rig, &body, "first", &["doc"], 2, &landed);
    assert_eq!(waiting(&first, 1), [due], "{first:?}");
    let item = rig.row(first.items[0].run_id.as_deref().unwrap());
    assert_eq!((item.status, item.next_attempt_at, item.error_kind), (RunStatus::Waiting, Some(due), Some(FailureTag::RateLimited)));
    let aggregate = rig.row("first");
    assert_eq!((aggregate.status, aggregate.next_attempt_at), (RunStatus::Waiting, Some(due)), "a fire holding only waiting items waits");
    assert_eq!(rig.catalog().owner_at(&job_scope("score")).unwrap().unwrap().next_attempt_at, Some(due), "the job's owner carries the wake");

    // A regular fire half way through the wait reaches the stored instant and waits again.
    rig.clock.advance(1800);
    let early = drive(&rig, &body, "second", &["doc"], 2, &landed);
    assert_eq!(waiting(&early, 1), [due], "the stored instant stands");
    assert_eq!(Throttled::count(&body.paid, "doc"), 1, "no vendor call before the stored instant");

    rig.clock.advance(1800);
    let resumed = drive(&rig, &body, "third", &["doc"], 2, &landed);
    assert!(resumed.succeeded(), "{resumed:?}");
    assert_eq!(resumed.items[0].execution_id, first.items[0].execution_id, "the resume keeps the work key");
    assert_eq!(Throttled::count(&body.paid, "doc"), 2);
    assert_eq!(*landed.lock().unwrap(), ["doc"]);
}

/// A resumed attempt replays every recorded call and reserves no permit an earlier attempt
/// reserved.
// spec: run.retry.no-double-spend@ad857f60
#[test]
fn a_resume_does_not_double_reserve_spend() {
    let rig = Rig::new();
    let body = Throttled::new("doc", 3600, 1);
    let landed = Mutex::default();
    drive(&rig, &body, "first", &["doc"], 1, &landed);
    rig.clock.advance(1800);
    drive(&rig, &body, "second", &["doc"], 1, &landed);
    rig.clock.advance(1800);
    let resumed = drive(&rig, &body, "third", &["doc"], 1, &landed);
    assert!(resumed.succeeded(), "{resumed:?}");
    assert_eq!(Throttled::count(&body.reserved, "doc"), 1, "three fires reserve one permit");
    assert_eq!(Throttled::count(&body.paid, "doc"), 2, "one throttled call and one granted call");
}

/// Under `isolated_items`, a waiting item frees its slot: its siblings land in the same
/// fire whether its wait is past the ceiling or within it.
// spec: run.items.waiting-item@e062a424
#[test]
fn an_item_waiting_under_isolated_items_leaves_its_siblings_completing() {
    // Past the ceiling: one slot, the throttled item first, and both siblings land.
    let rig = Rig::new();
    let body = Throttled::new("a-limited", 3600, 1);
    let landed = Mutex::default();
    let report = drive(&rig, &body, "long", &["a-limited", "b", "c"], 1, &landed);
    assert_eq!(waiting(&report, 3), [at(T0).plus_secs(3600)]);
    assert_eq!(*landed.lock().unwrap(), ["b", "c"]);
    assert_eq!(rig.row("long").status, RunStatus::Waiting, "a fire whose unfinished items all wait closes waiting");

    // Within the ceiling: the siblings land before the throttled item's retry, which
    // completes in the same fire.
    let rig = Rig::new();
    let body = Throttled::new("a-limited", 1, 1);
    let landed = Mutex::default();
    let report = drive(&rig, &body, "short", &["a-limited", "b", "c"], 1, &landed);
    assert!(report.succeeded(), "{report:?}");
    assert_eq!(*landed.lock().unwrap(), ["b", "c", "a-limited"], "no sibling waited on the throttled item");
}

fn open(run_id: &str, attempts: u32) -> OpenExecution {
    OpenExecution {
        scope: OwnerScope::host("vendor-sync"),
        pins: OwnerPins::from(PlanPins { plan_ref: "plan-a".into(), identities: Default::default() }),
        run_id: run_id.into(),
        site_id: "site-a".into(),
        pid: 4242,
        boot_id: "boot-a".into(),
        trace_id: None,
        connector: None,
        schedule: schedule(attempts),
    }
}

/// A fresh engine over the rig's directory and clock: the process after a restart.
fn restarted(rig: &Rig) -> Engine {
    Engine {
        catalog: Arc::new(LocalCatalog::open(rig.dir.path(), Arc::new(rig.clock.clone()))),
        journal: Journal::over(FileJournalStore::open(rig.dir.path()), FileBlobStore::open(rig.dir.path())),
        awakeables: None,
        keeper: Keeper::new(KeeperCadence { poll: Duration::from_millis(20), renew: Duration::from_secs(10) }),
        emitter: None,
        worlds: Vec::new(),
        stages: None,
        trace_id: None,
    }
}

/// A process dying during an in-ceiling sleep leaves the stored instant; its resume sleeps
/// out the remaining wait, neither restarting it nor skipping it.
// spec: run.retry.resume-at@4b7d88f4
#[test]
fn a_crash_during_an_in_ceiling_sleep_resumes_the_remaining_wait() {
    let rig = Rig::new();
    let calls = Arc::new(AtomicUsize::new(0));
    let (execution, token) = std::sync::mpsc::channel();
    let engine = &rig.engine;
    std::thread::scope(|s| {
        let calls = calls.clone();
        s.spawn(move || {
            let mut x = engine.open_execution(&open("job-1", 5)).unwrap();
            execution.send((x.execution_id().to_string(), x.token().clone())).unwrap();
            let slept = x.step("charge", b"order-9", &mut |_| {
                calls.fetch_add(1, Ordering::SeqCst);
                Err(Failure::new(FailureTag::RateLimited, "the vendor answered 429").with_retry_after(120))
            });
            assert!(slept.is_err(), "the sleep ends only on the stop");
            // Dropped unclosed: the process died mid-sleep.
            drop(x);
        });
        let (id, stop) = token.recv().unwrap();
        let key = EntryKey::new(&id, "charge", b"order-9");
        let stored = loop {
            if let Some(stored) = engine.not_before(&key).unwrap() {
                break stored;
            }
            std::thread::sleep(Duration::from_millis(5));
        };
        assert_eq!(stored, NotBefore { at: at(T0).plus_secs(120), attempts: 1 }, "the instant persists before the sleep");
        stop.fire();
    });

    // The replacement opens one second before the stored instant.
    rig.clock.advance(119);
    let engine = restarted(&rig);
    let mut x = engine.open_execution(&open("job-2", 5)).unwrap();
    let started = std::time::Instant::now();
    let value = x
        .step("charge", b"order-9", &mut |_| {
            calls.fetch_add(1, Ordering::SeqCst);
            Ok(b"charged".to_vec())
        })
        .unwrap();
    let slept = started.elapsed();
    assert_eq!(value, b"charged");
    assert_eq!(calls.load(Ordering::SeqCst), 2);
    assert!(slept >= Duration::from_millis(900) && slept < Duration::from_secs(60), "slept {slept:?}, the one remaining second");
}

/// A restart between a reschedule and its resume keeps the stored instant and the attempt
/// count, and the attempt budget carries over to the resumed attempt.
// spec: run.retry.not-before@bb80cde6
// spec: run.retry.one-layer@4e92c090
#[test]
fn a_restart_between_reschedule_and_resume_keeps_the_time_and_the_attempt_count() {
    let rig = Rig::new();
    let name = SecretName::parse("vendor-key").unwrap();
    let calls = AtomicUsize::new(0);
    // A lease mint the provider throttles for an hour.
    let mut x = rig.engine.open_execution(&open("job-1", 2)).unwrap();
    let execution = x.execution_id().to_string();
    let waited = x.step("mint", b"key", &mut |_| {
        calls.fetch_add(1, Ordering::SeqCst);
        Err(lease::classify(429, Some(3600), &name))
    });
    assert!(matches!(&waited, Err(EngineError::Failure(f)) if f.tag == FailureTag::RateLimited), "{waited:?}");
    drop(x);

    let engine = restarted(&rig);
    let key = EntryKey::new(&execution, "mint", b"key");
    let due = at(T0).plus_secs(3600);
    assert_eq!(engine.not_before(&key).unwrap(), Some(NotBefore { at: due, attempts: 1 }), "the restart reads the instant and the count");

    rig.clock.advance(1800);
    let mut x = engine.open_execution(&open("job-2", 2)).unwrap();
    assert_eq!(x.execution_id(), execution);
    let early = x.step("mint", b"key", &mut |_| {
        calls.fetch_add(1, Ordering::SeqCst);
        Ok(vec![])
    });
    assert!(matches!(&early, Err(EngineError::Failure(f)) if f.tag == FailureTag::RateLimited), "{early:?}");
    assert_eq!(calls.load(Ordering::SeqCst), 1, "no attempt before the stored instant");
    let Err(EngineError::Failure(failure)) = early else { unreachable!() };
    let closed = x.close(Outcome::Failed(failure)).unwrap();
    assert_eq!((closed.status, closed.next_attempt_at), (RunStatus::Waiting, Some(due)), "the host's close records the wait");
    assert_eq!(engine.not_before(&key).unwrap(), Some(NotBefore { at: due, attempts: 1 }));

    // At the instant, the second and last attempt runs; its failure exhausts the budget.
    rig.clock.advance(1800);
    let mut x = engine.open_execution(&open("job-3", 2)).unwrap();
    let last = x.step("mint", b"key", &mut |_| {
        calls.fetch_add(1, Ordering::SeqCst);
        Err(lease::classify(429, Some(3600), &name))
    });
    assert!(matches!(&last, Err(EngineError::Refused(RunError::StepFailed { failure, .. })) if failure.tag == FailureTag::RateLimited), "{last:?}");
    assert_eq!(calls.load(Ordering::SeqCst), 2, "the budget carried over: attempt 2 of 2 was the last");

    // An HTTP `Retry-After` and a limiter denial reach the same path.
    let now = rig.engine.catalog.now().unwrap();
    let http = Failure::new(FailureTag::RateLimited, "429").with_retry_after(meter::parse_retry_after("900", now).unwrap());
    let meter::Decision::Denied { retry_after_secs } = meter::Decision::read(br#"{"decision":"denied","retry_after_secs":60}"#).unwrap() else { panic!("a denial") };
    let denied = Failure::new(FailureTag::RateLimited, "the limiter denied the acquire").with_retry_after(retry_after_secs);
    for (label, failure, wait) in [("http", http, 900), ("limiter", denied, 60)] {
        let engine = restarted(&rig);
        let opened = OpenExecution { scope: OwnerScope::host(label), ..open(&format!("{label}-1"), 5) };
        let mut x = engine.open_execution(&opened).unwrap();
        let id = x.execution_id().to_string();
        std::thread::scope(|s| {
            let token = x.token().clone();
            let key = EntryKey::new(&id, label, b"call");
            let engine = &engine;
            s.spawn(move || loop {
                if let Some(stored) = engine.not_before(&key).unwrap() {
                    assert_eq!(stored, NotBefore { at: now.plus_secs(wait), attempts: 1 }, "{label}");
                    token.fire();
                    return;
                }
                std::thread::sleep(Duration::from_millis(5));
            });
            let failure = failure.clone();
            let _ = x.step(label, b"call", &mut |_| Err(failure.clone()));
        });
    }
}

/// A dispatch that records each fire.
#[derive(Default)]
struct Fires(Mutex<Vec<String>>);

impl Dispatch for Fires {
    fn fire(&self, id: &str, _: &[String], _: &BTreeMap<String, String>, _: u64) -> Result<String, String> {
        self.0.lock().unwrap().push(id.to_string());
        Ok(format!("{id} fired"))
    }
}

/// The first beat at or after a waiting job's stored instant fires it, ahead of its regular
/// daily fire.
// spec: run.retry.wake@3dbe63da
#[test]
fn the_first_beat_at_the_stored_time_fires_the_waiting_job() {
    let rig = Rig::new();
    let body = Throttled::new("doc", 3600, 1);
    let landed = Mutex::default();
    let mut row = crate::support_row("job:score-0", RunStatus::Success);
    row.pipeline_id = "job:score".into();
    rig.catalog().put_run(&row).unwrap();
    drive(&rig, &body, "first", &["doc"], 1, &landed);

    let fires = Arc::new(Fires::default());
    let mut s = Scheduler::new(rig.engine.catalog.clone(), fires.clone() as Arc<dyn Dispatch>, "prod", "daemon-a", 4);
    s.arm(1, vec![Entry { id: "job:score".into(), schedule: Cadence::parse("every 1d").unwrap(), priority: 0 }]).unwrap();
    assert_eq!(s.next_due().unwrap(), Some(at(T0).plus_secs(3600)), "the wake precedes the daily fire");
    rig.clock.advance(3599);
    assert!(s.beat().unwrap().started.is_empty());
    rig.clock.advance(1);
    assert_eq!(s.beat().unwrap().started, ["job:score"]);
    s.drain();
    assert!(s.beat().unwrap().started.is_empty(), "one wake fires once");
    assert_eq!(*fires.0.lock().unwrap(), ["job:score"]);
}
