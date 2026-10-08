//! `run.journal`, `run.suspend` and `run.record` for the store-driven kind: an execution
//! under a job's host scope that reads its input once, runs a registered body per row and
//! journals each paid call under a label scoped to the row.

use crate::support::{Rig, T0};
use contextful_core::run::drive::{row_label, Emitted, InputRow, InputSet, RowBody, RowCalls, RowStop, StoreInput, Woken};
use contextful_core::run::journal::{EntryKey, Row as JournalRow};
use contextful_core::run::ports::{Landed, Row};
use contextful_core::run::record::RunStatus;
use contextful_core::run::retry::Schedule;
use contextful_core::run::{Failure, FailureTag, RunError};
use contextful_engine::awake::Registry;
use contextful_engine::drive::{job_scope, Drive};
use contextful_engine::stores::FileAwakeableStore;
use contextful_engine::{Engine, EngineError};
use serde_json::json;
use std::collections::BTreeMap;
use std::panic::AssertUnwindSafe;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

const JOB: &str = "score";

fn input(statement: &str, as_of: Option<&str>) -> StoreInput {
    StoreInput { body: "score".into(), statement: statement.into(), as_of: as_of.map(str::to_string) }
}

fn fire<'a>(input: StoreInput, body: &'a dyn RowBody, max_in_flight: usize, run_id: &str) -> Drive<'a> {
    Drive {
        job: JOB.into(),
        input,
        body,
        max_in_flight,
        run_id: run_id.into(),
        site_id: "site-a".into(),
        pid: 4242,
        boot_id: "boot-a".into(),
        schedule: Schedule::single_attempt(),
        poll: Duration::from_millis(10),
    }
}

fn docs(n: usize) -> Vec<Row> {
    (0..n).map(|i| json!({ "doc_id": format!("d{i:03}") }).as_object().cloned().unwrap()).collect()
}

fn set(as_of: &str, rows: Vec<Row>) -> InputSet {
    InputSet { as_of: as_of.into(), snapshots: BTreeMap::from([("documents".to_string(), "snap-1".to_string())]), rows }
}

/// A paid endpoint: every call it served, as the row's doc id and the idempotency key it carried.
#[derive(Default)]
struct Endpoint {
    served: Mutex<Vec<(String, String)>>,
    /// Calls it serves before every later call dies with the process.
    dies_after: Option<usize>,
}

impl Endpoint {
    fn serve(&self, doc: &str, key: &str) -> Result<Vec<u8>, Failure> {
        let mut served = self.served.lock().unwrap();
        if self.dies_after.is_some_and(|n| served.len() >= n) {
            drop(served);
            panic!("the process dies inside a paid call");
        }
        served.push((doc.to_string(), key.to_string()));
        Ok(format!("score:{doc}").into_bytes())
    }

    fn served(&self) -> Vec<(String, String)> {
        self.served.lock().unwrap().clone()
    }
}

/// Scores each row with one paid call and emits one `scores` row.
struct Score<'a> {
    endpoint: &'a Endpoint,
    /// Rows inside the body now, and the most seen at once.
    inside: AtomicUsize,
    most: AtomicUsize,
    entered: Mutex<Vec<String>>,
    /// A doc id whose row fails, terminally.
    fails: Option<String>,
    /// How long each row holds its slot.
    hold: Duration,
}

impl<'a> Score<'a> {
    fn new(endpoint: &'a Endpoint) -> Score<'a> {
        Score { endpoint, inside: AtomicUsize::new(0), most: AtomicUsize::new(0), entered: Mutex::default(), fails: None, hold: Duration::ZERO }
    }
}

fn doc_of(row: &InputRow) -> String {
    row.row["doc_id"].as_str().unwrap().to_string()
}

fn one(table: &str, row: serde_json::Value) -> Emitted {
    BTreeMap::from([(table.to_string(), vec![row.as_object().cloned().unwrap()])])
}

impl RowBody for Score<'_> {
    fn run(&self, row: &InputRow, calls: &dyn RowCalls) -> Result<Emitted, RowStop> {
        let doc = doc_of(row);
        let now = self.inside.fetch_add(1, Ordering::SeqCst) + 1;
        self.most.fetch_max(now, Ordering::SeqCst);
        self.entered.lock().unwrap().push(doc.clone());
        std::thread::sleep(self.hold);
        let result = (|| {
            if self.fails.as_deref() == Some(doc.as_str()) {
                return Err(RowStop::Failed(Failure::deterministic(FailureTag::Permanent, format!("row {doc} is malformed"))));
            }
            let score = calls.call("model", doc.as_bytes(), &mut |key| self.endpoint.serve(&doc, key))?;
            Ok(one("scores", json!({ "doc_id": doc, "score": String::from_utf8(score).unwrap() })))
        })();
        self.inside.fetch_sub(1, Ordering::SeqCst);
        result
    }
}

/// A land that keeps every emitted table.
fn keep(into: &Mutex<Vec<Emitted>>) -> impl FnMut(&Emitted) -> Result<Landed, Failure> + '_ {
    move |e| {
        into.lock().unwrap().push(e.clone());
        Ok(Landed { rows: e.values().map(|r| r.len() as u64).sum(), bytes: 0 })
    }
}

/// Each input row runs the registered body under a row key, its ordinal in the recorded input; every step the body
/// records carries a label scoped to that key, and a paid call carries {{run.journal.idempotency-key}}.
// spec: run.journal.row-step@3df3ff77
#[test]
fn a_run_killed_after_forty_paid_calls_resumes_paying_for_the_other_sixty() {
    let rig = Rig::new();
    let dying = Endpoint { dies_after: Some(40), ..Endpoint::default() };
    let body = Score::new(&dying);
    let reads = AtomicUsize::new(0);
    let mut read = |as_of: &str| {
        reads.fetch_add(1, Ordering::SeqCst);
        Ok(set(as_of, docs(100)))
    };
    let died = std::panic::catch_unwind(AssertUnwindSafe(|| {
        let _ = rig.engine.drive(&fire(input("SELECT doc_id FROM documents", Some(T0)), &body, 4, "fire-1"), &mut read, &mut |_| unreachable!("a dead fire lands nothing"));
    }));
    assert!(died.is_err(), "the fire dies inside its forty-first paid call");
    assert_eq!(dying.served().len(), 40);
    let execution_id = rig.catalog().owner_at(&job_scope(JOB)).unwrap().expect("the owner stays pending").execution_id;

    rig.clock.advance(60);
    let endpoint = Endpoint::default();
    let body = Score::new(&endpoint);
    let landed = Mutex::new(Vec::new());
    let row = rig.engine.drive(&fire(input("SELECT doc_id FROM documents", Some(T0)), &body, 4, "fire-2"), &mut read, &mut keep(&landed)).unwrap();
    assert_eq!(row.status, RunStatus::Success, "{row:?}");
    assert_eq!(row.execution_id, execution_id, "the resume keys on the same execution");
    let resumed = endpoint.served();
    assert_eq!(resumed.len(), 60, "every recorded call replays without paying again");
    assert_eq!(reads.load(Ordering::SeqCst), 1);
    let first: Vec<String> = dying.served().into_iter().map(|(d, _)| d).collect();
    for (doc, key) in &resumed {
        assert!(!first.contains(doc), "{doc} was paid for twice");
        let ordinal = doc.trim_start_matches('d').parse::<usize>().unwrap().to_string();
        let entry = EntryKey::new(&execution_id, &row_label(&ordinal, "model"), doc.as_bytes());
        assert_eq!(key, &entry.idempotency_key(), "{doc} carries the idempotency key of its entry");
    }
    let scores = &landed.lock().unwrap()[0]["scores"];
    assert_eq!(scores.len(), 100);
    assert_eq!(scores.iter().map(|r| r["doc_id"].as_str().unwrap().to_string()).collect::<Vec<_>>(), (0..100).map(|i| format!("d{i:03}")).collect::<Vec<_>>());
}

/// The input is read once, at the declared `as_of`, and every row of it runs.
#[test]
fn the_input_is_read_once_and_recorded_as_the_first_step() {
    let rig = Rig::new();
    let endpoint = Endpoint::default();
    let body = Score::new(&endpoint);
    let asked = Mutex::new(Vec::new());
    let landed = Mutex::new(Vec::new());
    let mut read = |as_of: &str| {
        asked.lock().unwrap().push(as_of.to_string());
        Ok(set(as_of, docs(3)))
    };
    let row = rig.engine.drive(&fire(input("SELECT doc_id FROM documents", Some("2030-01-01T00:00:05Z")), &body, 2, "fire-1"), &mut read, &mut keep(&landed)).unwrap();
    assert_eq!(row.status, RunStatus::Success);
    assert_eq!(*asked.lock().unwrap(), vec!["2030-01-01T00:00:05Z".to_string()]);
    assert_eq!(endpoint.served().len(), 3);
}

/// A resume iterates the recorded input and never re-reads the statement, so a row landed after the pinned `as_of`,
/// or folded since, never enters the input set.
// spec: run.journal.input-replay@8e916a02
#[test]
fn a_resume_iterates_the_recorded_rows_whatever_the_store_holds_since() {
    let rig = Rig::new();
    let dying = Endpoint { dies_after: Some(1), ..Endpoint::default() };
    let body = Score::new(&dying);
    let _ = std::panic::catch_unwind(AssertUnwindSafe(|| {
        let mut read = |as_of: &str| Ok(set(as_of, docs(3)));
        let _ = rig.engine.drive(&fire(input("SELECT doc_id FROM documents", Some(T0)), &body, 1, "fire-1"), &mut read, &mut |_| unreachable!());
    }));
    rig.clock.advance(60);
    let endpoint = Endpoint::default();
    let body = Score::new(&endpoint);
    let landed = Mutex::new(Vec::new());
    // The store now answers a later landing and a fold; the resume never asks it.
    let mut reread = |as_of: &str| Ok(set(as_of, docs(9)));
    let row = rig.engine.drive(&fire(input("SELECT doc_id FROM documents", Some(T0)), &body, 1, "fire-2"), &mut reread, &mut keep(&landed)).unwrap();
    assert_eq!(row.status, RunStatus::Success);
    assert_eq!(body.entered.lock().unwrap().len(), 3, "the three recorded rows run, and no later one");
    assert_eq!(landed.lock().unwrap()[0]["scores"].len(), 3);
}

/// A store-driven job declaring no `as_of` resolves it to the instant its execution opens, recorded in the input
/// step, so every resume reads that instant.
// spec: run.journal.open-as-of@41577ed5
#[test]
fn an_undeclared_as_of_is_the_open_instant_and_a_resume_keeps_it() {
    let rig = Rig::new();
    let dying = Endpoint { dies_after: Some(0), ..Endpoint::default() };
    let body = Score::new(&dying);
    let _ = std::panic::catch_unwind(AssertUnwindSafe(|| {
        let mut read = |as_of: &str| Ok(set(as_of, docs(2)));
        let _ = rig.engine.drive(&fire(input("SELECT doc_id FROM documents", None), &body, 1, "fire-1"), &mut read, &mut |_| unreachable!());
    }));
    assert_eq!(rig.row("fire-1").input.unwrap().as_of, "2030-01-01T00:00:00Z");
    rig.clock.advance(3600);
    let endpoint = Endpoint::default();
    let body = Score::new(&endpoint);
    let landed = Mutex::new(Vec::new());
    let mut read = |_: &str| -> Result<InputSet, Failure> { panic!("a resume reads no input") };
    let row = rig.engine.drive(&fire(input("SELECT doc_id FROM documents", None), &body, 1, "fire-2"), &mut read, &mut keep(&landed)).unwrap();
    assert_eq!(row.input.unwrap().as_of, "2030-01-01T00:00:00Z", "the resume reads the instant the first attempt resolved");

    // An attempt that dies inside the read records no input step; the resume still reads
    // at the instant the execution opened, not at its own.
    let rig = Rig::new();
    let body = Score::new(&endpoint);
    let died = std::panic::catch_unwind(AssertUnwindSafe(|| {
        let mut read = |_: &str| -> Result<InputSet, Failure> { panic!("the process dies inside the read") };
        let _ = rig.engine.drive(&fire(input("SELECT doc_id FROM documents", None), &body, 1, "fire-1"), &mut read, &mut |_| unreachable!());
    }));
    assert!(died.is_err());
    rig.clock.advance(3600);
    let asked = Mutex::new(Vec::new());
    let mut read = |as_of: &str| {
        asked.lock().unwrap().push(as_of.to_string());
        Ok(set(as_of, docs(2)))
    };
    let landed = Mutex::new(Vec::new());
    let row = rig.engine.drive(&fire(input("SELECT doc_id FROM documents", None), &body, 1, "fire-2"), &mut read, &mut keep(&landed)).unwrap();
    assert_eq!(row.status, RunStatus::Success, "{row:?}");
    assert_eq!(*asked.lock().unwrap(), vec![T0.to_string()], "the resume reads at the instant the execution opened");
    assert_eq!(row.input.unwrap().as_of, T0);
}

/// A store-driven run's plan reference hashes its body name, statement and declared `as_of`, so a resume under a
/// changed one meets {{run.own.pinned-plan-changed}} before any step replays.
// spec: run.journal.input-pin@24cf14b0
#[test]
fn a_resume_under_a_changed_statement_or_as_of_refuses_before_any_replay() {
    let rig = Rig::new();
    let dying = Endpoint { dies_after: Some(2), ..Endpoint::default() };
    let body = Score::new(&dying);
    let _ = std::panic::catch_unwind(AssertUnwindSafe(|| {
        let mut read = |as_of: &str| Ok(set(as_of, docs(5)));
        let _ = rig.engine.drive(&fire(input("SELECT doc_id FROM documents", Some(T0)), &body, 1, "fire-1"), &mut read, &mut |_| unreachable!());
    }));
    rig.clock.advance(60);
    let endpoint = Endpoint::default();
    let body = Score::new(&endpoint);
    let changed = [input("SELECT doc_id FROM documents WHERE doc_id > 'd001'", Some(T0)), input("SELECT doc_id FROM documents", Some("2030-01-02T00:00:00Z"))];
    for (i, changed) in changed.into_iter().enumerate() {
        let run_id = format!("fire-x{i}");
        let mut read = |_: &str| -> Result<InputSet, Failure> { panic!("a refused open reads no input") };
        match rig.engine.drive(&fire(changed, &body, 1, &run_id), &mut read, &mut |_| unreachable!()) {
            Err(EngineError::Refused(RunError::ExecutionPinMismatch(m))) => assert!(m.contains("host scope `job:score`"), "{m}"),
            other => panic!("expected ExecutionPinMismatch, got {other:?}"),
        }
        assert_eq!(rig.row(&run_id).status, RunStatus::Failed);
    }
    assert!(body.entered.lock().unwrap().is_empty() && endpoint.served().is_empty(), "no refused resume replays a step");
}

/// A store-driven run holds at most `max_in_flight` rows inside the body at once; a failing row admits no further
/// row, and the run closes after the rows in flight return.
// spec: run.journal.row-concurrency@be5e10f8
#[test]
fn at_most_max_in_flight_rows_run_and_a_failed_row_admits_no_further_row() {
    struct FirstWave<'a> {
        body: Score<'a>,
        entered: Mutex<usize>,
        changed: Condvar,
        inside: AtomicUsize,
        most: AtomicUsize,
        failed: bool,
        returned: AtomicUsize,
    }
    impl RowBody for FirstWave<'_> {
        fn run(&self, row: &InputRow, calls: &dyn RowCalls) -> Result<Emitted, RowStop> {
            let inside = self.inside.fetch_add(1, Ordering::SeqCst) + 1;
            self.most.fetch_max(inside, Ordering::SeqCst);
            let mut entered = self.entered.lock().unwrap();
            *entered += 1;
            self.changed.notify_all();
            let (entered, timeout) = self.changed.wait_timeout_while(entered, Duration::from_secs(5), |n| *n < 4).unwrap();
            assert!(!timeout.timed_out(), "four admitted rows reach the first-wave barrier");
            drop(entered);
            let result = if self.failed {
                Err(RowStop::Failed(Failure::deterministic(FailureTag::Permanent, "first-wave row is malformed")))
            } else {
                self.body.run(row, calls)
            };
            self.inside.fetch_sub(1, Ordering::SeqCst);
            self.returned.fetch_add(1, Ordering::SeqCst);
            result
        }
    }
    let rig = Rig::new();
    let endpoint = Endpoint::default();
    let body = FirstWave { body: Score::new(&endpoint), entered: Mutex::new(0), changed: Condvar::new(), inside: AtomicUsize::new(0), most: AtomicUsize::new(0), failed: false, returned: AtomicUsize::new(0) };
    let landed = Mutex::new(Vec::new());
    let mut read = |as_of: &str| Ok(set(as_of, docs(40)));
    let row = rig.engine.drive(&fire(input("SELECT doc_id FROM documents", Some(T0)), &body, 4, "fire-1"), &mut read, &mut keep(&landed)).unwrap();
    assert_eq!(row.status, RunStatus::Success);
    assert_eq!(body.most.load(Ordering::SeqCst), 4, "four rows share the body at once, never five");

    let rig = Rig::new();
    let body = FirstWave { failed: true, ..body };
    *body.entered.lock().unwrap() = 0;
    body.returned.store(0, Ordering::SeqCst);
    let row = rig.engine.drive(&fire(input("SELECT doc_id FROM documents", Some(T0)), &body, 4, "fire-1"), &mut read, &mut |_| unreachable!("a failed fire lands nothing")).unwrap();
    assert_eq!(row.status, RunStatus::Failed);
    assert_eq!(*body.entered.lock().unwrap(), 4, "every worker observes its first-wave failure before admitting another row");
    assert_eq!(body.returned.load(Ordering::SeqCst), 4, "the run waits for every admitted row to return");
    assert_eq!(body.inside.load(Ordering::SeqCst), 0);

    let rig = Rig::new();
    let endpoint = Endpoint::default();
    let mut body = Score::new(&endpoint);
    body.fails = Some("d010".into());
    let row = rig.engine.drive(&fire(input("SELECT doc_id FROM documents", Some(T0)), &body, 1, "fire-1"), &mut read, &mut |_| unreachable!("a failed fire lands nothing")).unwrap();
    assert_eq!(row.status, RunStatus::Failed);
    assert!(row.error_message.unwrap().contains("row d010 is malformed"));
    let entered = body.entered.lock().unwrap().len();
    assert_eq!(entered, 11, "the sole worker admits no row after its failure");
    assert!(rig.catalog().owner_at(&job_scope(JOB)).unwrap().is_some(), "a failure after recorded calls holds the owner");
}

#[test]
fn successful_rows_may_finish_while_an_earlier_row_has_not_failed_yet() {
    struct Delayed<'a> {
        body: Score<'a>,
        entered: Mutex<usize>,
        changed: Condvar,
    }
    impl RowBody for Delayed<'_> {
        fn run(&self, row: &InputRow, calls: &dyn RowCalls) -> Result<Emitted, RowStop> {
            let failing = doc_of(row) == "d010";
            let mut entered = self.entered.lock().unwrap();
            *entered += 1;
            self.changed.notify_all();
            if failing {
                let (ready, timeout) = self.changed.wait_timeout_while(entered, Duration::from_secs(5), |n| *n < 20).unwrap();
                assert!(!timeout.timed_out(), "other workers continue before the blocked row fails");
                entered = ready;
            }
            drop(entered);
            self.body.run(row, calls)
        }
    }
    let rig = Rig::new();
    let endpoint = Endpoint::default();
    let mut score = Score::new(&endpoint);
    score.fails = Some("d010".into());
    let body = Delayed { body: score, entered: Mutex::new(0), changed: Condvar::new() };
    let mut read = |as_of: &str| Ok(set(as_of, docs(40)));
    let row = rig.engine.drive(&fire(input("SELECT doc_id FROM documents", Some(T0)), &body, 4, "fire-1"), &mut read, &mut |_| unreachable!("a failed fire lands nothing")).unwrap();
    assert_eq!(row.status, RunStatus::Failed);
    assert!(row.error_message.unwrap().contains("row d010 is malformed"));
    let entered = *body.entered.lock().unwrap();
    assert!(entered >= 20, "the failing row returns only after twenty admissions");
}

/// A failed landing holds the owner, and its resume lands every emitted row without paying again.
#[test]
fn a_failed_landing_holds_the_owner_and_its_resume_lands_without_paying_again() {
    let rig = Rig::new();
    let endpoint = Endpoint::default();
    let body = Score::new(&endpoint);
    let run = |run_id: &str, land: &mut dyn FnMut(&Emitted) -> Result<Landed, Failure>| {
        let mut read = |as_of: &str| Ok(set(as_of, docs(6)));
        rig.engine.drive(&fire(input("SELECT doc_id FROM documents", Some(T0)), &body, 3, run_id), &mut read, land).unwrap()
    };
    let row = run("fire-1", &mut |_| Err(Failure::new(FailureTag::Storage, "the store refused the commit")));
    assert_eq!(row.status, RunStatus::Failed);
    assert_eq!(endpoint.served().len(), 6);
    assert!(rig.catalog().owner_at(&job_scope(JOB)).unwrap().is_some(), "the owner holds past a failed landing");

    let landed = Mutex::new(Vec::new());
    let row = run("fire-2", &mut keep(&landed));
    assert_eq!(row.status, RunStatus::Success);
    assert_eq!(endpoint.served().len(), 6, "the resume replays every paid call");
    assert_eq!(landed.lock().unwrap()[0]["scores"].len(), 6);
    assert!(rig.catalog().owner_at(&job_scope(JOB)).unwrap().is_none(), "success retires the owner");
}

/// A store-driven run's row carries its input: the resolved `as_of`, the snapshot id per table and the input row
/// count.
// spec: run.record.input-bounds@c5d2c5a6
#[test]
fn the_run_row_carries_the_input_as_of_snapshots_and_row_count() {
    let rig = Rig::new();
    let endpoint = Endpoint::default();
    let body = Score::new(&endpoint);
    let landed = Mutex::new(Vec::new());
    let mut read = |as_of: &str| Ok(set(as_of, docs(7)));
    let row = rig.engine.drive(&fire(input("SELECT doc_id FROM documents", Some(T0)), &body, 2, "fire-1"), &mut read, &mut keep(&landed)).unwrap();
    let bounds = row.input.expect("a store-driven row carries its input");
    assert_eq!((bounds.as_of.as_str(), bounds.rows), (T0, 7));
    assert_eq!(bounds.snapshots, BTreeMap::from([("documents".to_string(), "snap-1".to_string())]));
    let text = serde_json::to_value(rig.row("fire-1")).unwrap();
    assert_eq!(text["input"]["rows"], json!(7));
}

/// Scores each row with one paid call, except `d003` and `d005`, which each await an awakeable and emit its outcome.
struct Approve<'a> {
    endpoint: &'a Endpoint,
    tokens: Mutex<BTreeMap<String, String>>,
}

impl RowBody for Approve<'_> {
    fn run(&self, row: &InputRow, calls: &dyn RowCalls) -> Result<Emitted, RowStop> {
        let doc = doc_of(row);
        if doc != "d003" && doc != "d005" {
            let score = calls.call("model", doc.as_bytes(), &mut |key| self.endpoint.serve(&doc, key))?;
            return Ok(one("scores", json!({ "doc_id": doc, "score": String::from_utf8(score).unwrap() })));
        }
        let token = calls.suspend("label", 60)?;
        self.tokens.lock().unwrap().insert(doc.clone(), token.clone());
        let label = match calls.awaited("label", &token)? {
            Woken::Resumed(p) => String::from_utf8(p).unwrap(),
            Woken::TimedOut => "timed_out".to_string(),
        };
        Ok(one("scores", json!({ "doc_id": doc, "score": label })))
    }
}

/// A store-driven row awaiting an unresolved awakeable parks and frees its slot; the other rows run on, and the
/// parked row re-enters its body once the awakeable resolves or its deadline passes.
// spec: run.suspend.row-parks@c2aaf6eb
#[test]
fn a_parked_row_frees_its_slot_and_re_enters_on_its_payload_or_its_deadline() {
    let rig = Rig::new();
    let store = Arc::new(FileAwakeableStore::open(rig.dir.path()));
    let engine = Engine { awakeables: Some(store.clone()), ..rig.engine.clone() };
    let registry = Registry::over(store, rig.engine.journal.clone());
    let endpoint = Endpoint::default();
    let body = Approve { endpoint: &endpoint, tokens: Mutex::default() };
    let landed = Mutex::new(Vec::new());
    let done = std::thread::scope(|s| {
        let driving = s.spawn(|| {
            let mut read = |as_of: &str| Ok(set(as_of, docs(10)));
            engine.drive(&fire(input("SELECT doc_id FROM documents", Some(T0)), &body, 4, "fire-1"), &mut read, &mut keep(&landed)).unwrap()
        });
        // Both waiting rows park; the eight others complete on the four slots.
        while endpoint.served().len() < 8 || body.tokens.lock().unwrap().len() < 2 {
            std::thread::sleep(Duration::from_millis(5));
        }
        while rig.row("fire-1").status != RunStatus::Waiting {
            std::thread::sleep(Duration::from_millis(5));
        }
        let tokens = body.tokens.lock().unwrap().clone();
        registry.resolve(&tokens["d005"], b"approved", rig.catalog().now().unwrap()).unwrap();
        rig.clock.advance(61);
        driving.join().unwrap()
    });
    assert_eq!(done.status, RunStatus::Success);
    let scores: BTreeMap<String, String> =
        landed.lock().unwrap()[0]["scores"].iter().map(|r| (r["doc_id"].as_str().unwrap().to_string(), r["score"].as_str().unwrap().to_string())).collect();
    assert_eq!(scores.len(), 10);
    assert_eq!(scores["d003"], "timed_out", "the deadline lands the recorded timeout value");
    assert_eq!(scores["d005"], "approved");
    assert_eq!(endpoint.served().len(), 8);
}

/// A row's wait past its deadline records a timeout value as that wait's step output, so every re-entry of the
/// body reads the identical value.
// spec: run.suspend.recorded-timeout@6a52b313
#[test]
fn a_timed_out_wait_is_a_recorded_step_every_re_entry_reads() {
    let rig = Rig::new();
    let store = Arc::new(FileAwakeableStore::open(rig.dir.path()));
    let engine = Engine { awakeables: Some(store.clone()), ..rig.engine.clone() };
    let registry = Registry::over(store, rig.engine.journal.clone());
    let endpoint = Endpoint::default();
    let body = Approve { endpoint: &endpoint, tokens: Mutex::default() };
    let checked = Mutex::new(None);
    let done = std::thread::scope(|s| {
        let driving = s.spawn(|| {
            let mut read = |as_of: &str| Ok(set(as_of, docs(4)));
            let execution = || rig.catalog().owner_at(&job_scope(JOB)).unwrap().unwrap().execution_id;
            let mut land = |e: &Emitted| {
                // Before the owner retires, the wait's step holds the timeout value.
                let token = body.tokens.lock().unwrap()["d003"].clone();
                let key = EntryKey::new(&execution(), &row_label("3", "wake:label"), token.as_bytes());
                let recorded = match rig.engine.journal.row(&key).unwrap() {
                    Some(JournalRow::Recorded { value, .. }) => rig.engine.journal.load(&value).unwrap(),
                    other => panic!("the wait records no value: {other:?}"),
                };
                *checked.lock().unwrap() = Some(Woken::decode(&recorded).unwrap());
                Ok(Landed { rows: e.values().map(|r| r.len() as u64).sum(), bytes: 0 })
            };
            engine.drive(&fire(input("SELECT doc_id FROM documents", Some(T0)), &body, 4, "fire-1"), &mut read, &mut land).unwrap()
        });
        while body.tokens.lock().unwrap().is_empty() || rig.row("fire-1").status != RunStatus::Waiting {
            std::thread::sleep(Duration::from_millis(5));
        }
        rig.clock.advance(61);
        let row = driving.join().unwrap();
        // A resolution past the deadline changes nothing the body reads.
        let token = body.tokens.lock().unwrap()["d003"].clone();
        assert!(registry.resolve(&token, b"late", rig.catalog().now().unwrap()).is_err());
        row
    });
    assert_eq!(done.status, RunStatus::Success);
    assert_eq!(checked.lock().unwrap().clone(), Some(Woken::TimedOut));
}

#[test]
// spec: run.journal.body-projection@b4109523
// spec: run.own.body-parent@24cc3aaf
fn protected_body_effects_record_rewritten_results_and_resume_without_another_paid_call() {
    use contextful_core::run::drive::{PreparedEmitted, RecordedRowBody, RecordedRowCalls};
    use contextful_core::run::effect::{EffectAdmission, EffectScope, EmissionSummary, RecordedBodyPlan, RecordedEffect};
    use contextful_core::run::ports::Types;
    struct Canonical;
    impl EffectAdmission for Canonical {
        fn identity(&self, _: &RecordedEffect) -> Result<String, Failure> { Ok("test-canonical-removal".into()) }
        fn prepare(&self, _: &RecordedEffect, mut rows:Vec<Row>, types:Types, scope:&EffectScope) -> Result<serde_json::Value, Failure> {
            for row in &mut rows { row.remove("score"); }
            Ok(json!({"rows":rows,"scope":scope,"types":types.keys().collect::<Vec<_>>() }))
        }
        fn admit(&self, _: &RecordedEffect, scope:&EffectScope, value:&serde_json::Value) -> Result<EmissionSummary, Failure> {
            assert_eq!(&serde_json::from_value::<EffectScope>(value["scope"].clone()).unwrap(), scope);
            let rows:Vec<Row> = serde_json::from_value(value["rows"].clone()).unwrap();
            assert!(rows.iter().all(|r| !r.contains_key("score")));
            Ok(EmissionSummary { rows:rows.len() as u64, columns:rows.iter().flat_map(|r| r.keys().cloned()).collect(), types:Default::default() })
        }
    }
    struct Protected<'a>(&'a AtomicUsize);
    impl RecordedRowBody for Protected<'_> {
        fn effects(&self) -> Vec<RecordedEffect> { vec![RecordedEffect { label:"model".into(), table:"scores".into(), types:Default::default(), transforms:Vec::new(), normalize:None }] }
        fn run_recorded(&self, row:&InputRow, calls:&dyn RecordedRowCalls) -> Result<PreparedEmitted, RowStop> {
            let doc = doc_of(row);
            let output = calls.call("model", doc.as_bytes(), &mut |_| {
                self.0.fetch_add(1, Ordering::SeqCst);
                Ok(serde_json::to_vec(&vec![json!({"doc_id":doc,"score":"private-paid-result-canary"})]).unwrap())
            })?;
            Ok(BTreeMap::from([("scores".into(), vec![output])]))
        }
    }
    impl RowBody for Protected<'_> {
        fn run(&self, _: &InputRow, _: &dyn RowCalls) -> Result<Emitted, RowStop> { panic!("prepared mode executes no raw body") }
        fn recorded(&self) -> Option<&dyn RecordedRowBody> { Some(self) }
    }
    let rig = Rig::new();
    let paid = AtomicUsize::new(0);
    let body = Protected(&paid);
    let plan = RecordedBodyPlan::compile(body.effects(), &["scores".into()]).unwrap();
    let first = rig.engine.drive_recorded(&fire(input("SELECT doc_id FROM documents", Some(T0)), &body, 1, "prepared-1"), &plan, &Canonical, &mut |as_of| Ok(set(as_of, docs(3))), &mut |outputs, _owner| {
        for output in &outputs["scores"] {
            let scope = output.scope();
            let value = match rig.engine.journal.row(scope.key()).unwrap().unwrap() { JournalRow::Recorded { value, .. } => value, other => panic!("{other:?}") };
            let bytes = rig.engine.journal.load(&value).unwrap();
            assert!(!String::from_utf8(bytes).unwrap().contains("private-paid-result-canary"));
        }
        Err(Failure::new(FailureTag::Storage, "commit failed"))
    }).unwrap();
    assert_eq!(first.status, RunStatus::Failed);
    assert_eq!(paid.load(Ordering::SeqCst), 3);
    let resumed = rig.engine.drive_recorded(&fire(input("SELECT doc_id FROM documents", Some(T0)), &body, 1, "prepared-2"), &plan, &Canonical, &mut |_| panic!("resume reads no source"), &mut |outputs, _owner| Ok(Landed { rows:outputs["scores"].iter().map(|o| o.summary().rows).sum(), bytes:0 })).unwrap();
    assert_eq!(resumed.status, RunStatus::Success);
    assert_eq!(resumed.rows, 3);
    assert_eq!(paid.load(Ordering::SeqCst), 3);
    struct Reused<'a> { paid: &'a AtomicUsize, previous:Mutex<Option<contextful_core::run::effect::PreparedEmission>> }
    impl RecordedRowBody for Reused<'_> {
        fn effects(&self) -> Vec<RecordedEffect> { Protected(self.paid).effects() }
        fn run_recorded(&self, row:&InputRow, calls:&dyn RecordedRowCalls) -> Result<PreparedEmitted, RowStop> {
            let mut previous = self.previous.lock().unwrap();
            let output = if let Some(previous) = previous.as_ref() { previous.clone() } else {
                let doc = doc_of(row);
                let output = calls.call("model", doc.as_bytes(), &mut |_| Ok(serde_json::to_vec(&vec![json!({"doc_id":doc,"score":"private-paid-result-canary"})]).unwrap()))?;
                *previous = Some(output.clone());
                output
            };
            Ok(BTreeMap::from([("scores".into(), vec![output])]))
        }
    }
    impl RowBody for Reused<'_> {
        fn run(&self, _: &InputRow, _: &dyn RowCalls) -> Result<Emitted, RowStop> { unreachable!() }
        fn recorded(&self) -> Option<&dyn RecordedRowBody> { Some(self) }
    }
    let rig = Rig::new();
    let reused = Reused { paid:&paid, previous:Mutex::new(None) };
    let refused = rig.engine.drive_recorded(&fire(input("SELECT doc_id FROM documents", Some(T0)), &reused, 1, "foreign-row"), &plan, &Canonical, &mut |as_of| Ok(set(as_of, docs(2))), &mut |_, _| panic!("foreign-row emission cannot reach landing")).unwrap();
    assert_eq!(refused.status, RunStatus::Failed);
    assert!(refused.error_message.unwrap().contains("outside this row's admitted calls"));
    let rig = Rig::new();
    let changed = RecordedBodyPlan::compile(vec![RecordedEffect { transforms:vec![contextful_core::pipeline::transform::TransformOp::Rename { from:"score".into(), to:"visible".into() }], ..body.effects().remove(0) }], &["scores".into()]).unwrap();
    let reads = AtomicUsize::new(0);
    let refused = rig.engine.drive_recorded(&fire(input("SELECT doc_id FROM documents", Some(T0)), &body, 1, "changed-registered-projection"), &changed, &Canonical, &mut |as_of| { reads.fetch_add(1, Ordering::SeqCst); Ok(set(as_of, Vec::new())) }, &mut |_, _| Ok(Landed::default()));
    assert!(refused.is_err(), "caller cannot substitute a projection for the registered body's closed declaration");
    assert_eq!(reads.load(Ordering::SeqCst), 0);
    #[derive(Clone, Copy, Debug, PartialEq)]
    enum Boundary { Entry, Stage, Commit }
    struct LosingParent<'a> { rig:&'a Rig, boundary:Boundary, retired:std::cell::Cell<bool>, sink:crate::support::Sink }
    impl LosingParent<'_> {
        fn lose(&self, boundary:Boundary) {
            if self.boundary != boundary || self.retired.replace(true) { return; }
            let scope = job_scope(JOB);
            let owner = self.rig.catalog().owner_at(&scope).unwrap().unwrap();
            self.rig.catalog().retire_at(&scope, &owner.execution_id, None, None).unwrap();
        }
    }
    impl contextful_core::run::ports::Destination for LosingParent<'_> {
        fn admit_effect_recorded(&self, _: &str, payload:&serde_json::Value, scope:&EffectScope) -> Result<EmissionSummary, Failure> {
            self.lose(Boundary::Entry);
            Canonical.admit(plan_effect(), scope, payload)
        }
        fn stage_effect_recorded(&mut self, mut stage:contextful_core::run::ports::Stage, payload:&serde_json::Value, _: &EffectScope) -> Result<contextful_core::run::ports::Part, Failure> {
            stage.rows = serde_json::from_value(payload["rows"].clone()).unwrap();
            let part = contextful_core::run::ports::Destination::stage_batch(&mut self.sink, stage)?;
            self.lose(Boundary::Stage);
            Ok(part)
        }
        fn stage_batch(&mut self, _:contextful_core::run::ports::Stage) -> Result<contextful_core::run::ports::Part, Failure> { panic!("prepared groups never stage raw") }
        fn commit(&mut self, commit:contextful_core::run::ports::Commit, before:&dyn Fn() -> Result<(), Failure>) -> Result<Landed, Failure> {
            self.lose(Boundary::Commit);
            contextful_core::run::ports::Destination::commit(&mut self.sink, commit, before)
        }
        fn discard(&mut self, table:&str, run:&str) -> Result<(), Failure> { contextful_core::run::ports::Destination::discard(&mut self.sink, table, run) }
        fn newest_marker(&self, pipeline:&str, table:&str) -> Result<Option<contextful_core::run::ports::Marker>, Failure> { contextful_core::run::ports::Destination::newest_marker(&self.sink, pipeline, table) }
    }
    fn plan_effect() -> &'static RecordedEffect {
        static EFFECT:std::sync::OnceLock<RecordedEffect> = std::sync::OnceLock::new();
        EFFECT.get_or_init(|| RecordedEffect { label:"model".into(), table:"scores".into(), types:Default::default(), transforms:Vec::new(), normalize:None })
    }
    let mut committed_after_loss = Vec::new();
    for boundary in [Boundary::Entry, Boundary::Stage, Boundary::Commit] {
        let rig = Rig::new();
        let mut destination = LosingParent { rig:&rig, boundary, retired:std::cell::Cell::new(false), sink:Default::default() };
        let mut output_plan = crate::support::plan("kind = 'opaque-token'", "journal = false");
        output_plan.spec.table = "scores".into();
        let spec = contextful_engine::RunSpec { connector:output_plan.connector_pin("test-artifact"), plan:output_plan, run_id:format!("land-{boundary:?}"), site_id:"site-a".into(), pid:4242, boot_id:"boot-a".into(), trace_id:None };
        let _ = rig.engine.drive_recorded(&fire(input("SELECT doc_id FROM documents", Some(T0)), &body, 1, &format!("parent-{boundary:?}")), &plan, &Canonical, &mut |as_of| Ok(set(as_of, docs(2))), &mut |outputs, owner| {
            let row = rig.engine.run_emissions(&spec, owner, &outputs["scores"], &mut destination).map_err(|e| Failure::new(FailureTag::Storage, e.to_string()))?;
            if row.status != RunStatus::Success { return Err(Failure::new(FailureTag::Storage, row.error_message.unwrap_or_default())); }
            Ok(Landed { rows:row.rows, bytes:row.bytes })
        });
        assert!(destination.retired.get(), "the control actually loses its parent at {boundary:?}");
        if !destination.sink.commits.is_empty() { committed_after_loss.push(boundary); }
    }
    assert!(committed_after_loss.is_empty(), "prepared results committed after live-parent loss at {committed_after_loss:?}");
}
