//! `run.cancel`: the stop mark, the token and what a stop leaves behind.

use crate::support::{plan, three_pages, Pages, Rig, Sink};
use contextful_core::coordinate::{Cas, Catalog, CursorRow, Lease, LeaseKey, LeaseRow};
use contextful_core::run::cancel::Scope;
use contextful_core::run::own::{ExecutionOwner, OwnerScope};
use contextful_core::run::ports::{Cancellation, PullRequest, Source};
use contextful_core::run::record::{RunRow, RunStatus};
use contextful_core::run::{Failure, FailureTag, RunError};
use contextful_core::time::Instant;
use contextful_engine::cancel::{Cadence, CancelToken, Due, Keeper, Schedule};
use contextful_engine::EngineError;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

fn opaque() -> contextful_core::run::plan::Plan {
    plan("kind = \"opaque-token\"", "[retry]\nbackoff = \"fixed\"\nbase_ms = 60000\njitter_ms = 0")
}

/// A source that marks its own run stopped from inside the pull, then fails transiently,
/// so the runner meets the stop in its retry sleep.
struct StopsItself {
    rig_catalog: Arc<dyn Catalog + Send + Sync>,
    engine: contextful_engine::Engine,
    serve_first: bool,
    calls: usize,
}

impl Source for StopsItself {
    fn pull(&mut self, request: &PullRequest, _: &dyn Cancellation) -> Result<Vec<u8>, Failure> {
        self.calls += 1;
        if self.serve_first && request.position.is_none() {
            return Ok(br#"{"rows":[{"id":"d1"}],"cursor":"p1","more":true}"#.to_vec());
        }
        let _ = &self.rig_catalog;
        self.engine.cancel("run-1", Scope::Run, Some("operator asked".into())).unwrap();
        Err(Failure::new(FailureTag::Transient, "reset"))
    }
}

/// A stop is written onto the run row as a requested-at instant, a scope and an optional reason; the catalog is
/// the only channel.
// spec: run.cancel.catalog-channel@de98c78d
#[test]
fn a_stop_is_a_mark_on_the_run_row() {
    let rig = Rig::new();
    let mut source = StopsItself { rig_catalog: rig.engine.catalog.clone(), engine: rig.engine.clone(), serve_first: false, calls: 0 };
    let row = rig.run(&opaque(), "1.0.0", "run-1", &mut source, &mut Sink::default()).unwrap();
    let stop = row.stop.clone().unwrap();
    assert_eq!(stop.requested_at, crate::support::at(crate::support::T0));
    assert_eq!(stop.scope, "run");
    assert_eq!(stop.reason.as_deref(), Some("operator asked"));
    // The runner learned of it from the row alone: the source never touched the run's token.
    assert_eq!(row.status, RunStatus::Canceled);
}

/// Abandoned work surfaces as the `Canceled` tag through the ordinary failure path, closing the record and
/// leaving the position alone.
#[test]
fn a_stop_in_the_retry_sleep_closes_canceled_and_holds_the_position() {
    let rig = Rig::new();
    rig.catalog().cursor_cas("feed", "filings", 0, CursorRow { position: None, ..CursorRow::default() }, None).unwrap();
    let before = rig.catalog().cursor("feed", "filings").unwrap();
    let mut source = StopsItself { rig_catalog: rig.engine.catalog.clone(), engine: rig.engine.clone(), serve_first: false, calls: 0 };
    let started = std::time::Instant::now();
    let row = rig.run(&opaque(), "1.0.0", "run-1", &mut source, &mut Sink::default()).unwrap();
    assert!(started.elapsed() < Duration::from_secs(10), "the 60 s retry sleep was cut short");
    assert_eq!(source.calls, 1);
    assert_eq!((row.status, row.error_kind), (RunStatus::Canceled, Some(FailureTag::Canceled)));
    assert!(row.ended_at.is_some());
    assert_eq!(rig.catalog().cursor("feed", "filings").unwrap().position, before.position);
}

/// A stopped run's recorded pull keeps its owner and replays next attempt; a stopped pull that recorded nothing
/// releases it.
#[test]
fn a_stop_keeps_a_recorded_pull_for_the_next_attempt() {
    // Nothing recorded: the owner is released.
    let rig = Rig::new();
    let mut source = StopsItself { rig_catalog: rig.engine.catalog.clone(), engine: rig.engine.clone(), serve_first: false, calls: 0 };
    rig.run(&opaque(), "1.0.0", "run-1", &mut source, &mut Sink::default()).unwrap();
    assert!(rig.catalog().owner("feed", "filings").unwrap().is_none());

    // One pull recorded: the owner holds and the next attempt replays it.
    let rig = Rig::new();
    let mut source = StopsItself { rig_catalog: rig.engine.catalog.clone(), engine: rig.engine.clone(), serve_first: true, calls: 0 };
    let row = rig.run(&opaque(), "1.0.0", "run-1", &mut source, &mut Sink::default()).unwrap();
    assert_eq!(row.status, RunStatus::Canceled);
    let owner = rig.catalog().owner("feed", "filings").unwrap().unwrap();
    assert_eq!(owner.execution_id, row.execution_id);
    assert_eq!(rig.catalog().cursor("feed", "filings").unwrap().position, None, "a stop advances no position");
    let mut pages = Pages::new(three_pages());
    let mut sink = Sink::default();
    let resumed = rig.run(&opaque(), "1.0.0", "run-2", &mut pages, &mut sink).unwrap();
    assert_eq!(resumed.status, RunStatus::Success);
    assert_eq!(resumed.execution_id, row.execution_id);
    assert_eq!(crate::support::ids(&sink.commits[0])[0], ["d1"], "the recorded pull replayed");
    assert!(!pages.calls().is_empty(), "the exclusion below ranges over no element");
    assert!(pages.calls().iter().all(|(p, _)| p.is_some()), "the first page was not re-issued");
}

/// Only a pending, running or waiting row accepts a mark, and a pipeline-scoped stop reaches every in-flight run
/// of its pipeline.
#[test]
fn a_stop_matches_only_in_flight_runs() {
    let rig = Rig::new();
    let c = rig.catalog();
    let mut done = crate::support_row("run-done", RunStatus::Success);
    done.pipeline_id = "feed".into();
    c.put_run(&done).unwrap();
    c.put_run(&crate::support_row("run-a", RunStatus::Running)).unwrap();
    c.put_run(&crate::support_row("run-b", RunStatus::Waiting)).unwrap();
    for id in ["run-done", "run-missing"] {
        assert!(matches!(rig.engine.cancel(id, Scope::Run, None), Err(EngineError::Refused(RunError::CancelTargetNotInFlight(_)))), "{id}");
    }
    let mut marked = rig.engine.cancel("run-a", Scope::Pipeline, None).unwrap();
    marked.sort();
    assert_eq!(marked, ["run-a", "run-b"]);
    assert!(c.run("run-done").unwrap().unwrap().stop.is_none());
}

/// The token is fed by one catalog read before the run's first await and then one every 500 ms, the cancellation
/// arm evaluated ahead of the work arm.
// spec: run.cancel.poll-interval@248eef1d
#[test]
fn the_token_reads_the_catalog_before_the_first_await_and_every_500_ms() {
    assert_eq!(Cadence::default().poll, Duration::from_millis(500));
    let rig = Rig::new();
    let c = rig.engine.catalog.clone();
    // A stop written before the run's first await is seen before the keeper returns.
    c.put_run(&crate::support_row("run-early", RunStatus::Running)).unwrap();
    rig.engine.cancel("run-early", Scope::Run, None).unwrap();
    let keeper = Keeper::new(Cadence::default());
    let token = CancelToken::default();
    let _r = keeper.register(c.clone(), "run-early", token.clone(), Arc::default());
    assert!(token.requested());
    // A later stop is seen within one interval.
    c.put_run(&crate::support_row("run-late", RunStatus::Running)).unwrap();
    let token = CancelToken::default();
    let _r = keeper.register(c.clone(), "run-late", token.clone(), Arc::default());
    assert!(!token.requested());
    rig.engine.cancel("run-late", Scope::Run, None).unwrap();
    let marked = std::time::Instant::now();
    while !token.requested() {
        assert!(marked.elapsed() < Duration::from_millis(900), "no poll within 500 ms");
        std::thread::sleep(Duration::from_millis(5));
    }
    // The cancellation arm runs first: a stop the token holds when a pull completes ends the run before the next one.
    let rig = Rig::new();
    let mut source = StopsThenServes { engine: rig.engine.clone(), calls: 0 };
    let row = rig.run(&opaque(), "1.0.0", "run-1", &mut source, &mut Sink::default()).unwrap();
    assert_eq!(source.calls, 1);
    assert_eq!(row.status, RunStatus::Canceled);
}

/// A source that marks its run stopped, waits past a poll, and serves a page promising more.
struct StopsThenServes {
    engine: contextful_engine::Engine,
    calls: usize,
}

impl Source for StopsThenServes {
    fn pull(&mut self, _: &PullRequest, _: &dyn Cancellation) -> Result<Vec<u8>, Failure> {
        self.calls += 1;
        self.engine.cancel("run-1", Scope::Run, None).unwrap();
        std::thread::sleep(Duration::from_millis(100));
        Ok(br#"{"rows":[{"id":"d1"}],"cursor":"p1","more":true}"#.to_vec())
    }
}

/// A catalog whose run reads fail for a while.
struct Flaky {
    inner: Arc<dyn Catalog + Send + Sync>,
    failures: AtomicUsize,
}

macro_rules! delegate {
    ($($name:ident($($arg:ident: $ty:ty),*) -> $ret:ty;)*) => {
        $(fn $name(&self, $($arg: $ty),*) -> $ret { self.inner.$name($($arg),*) })*
    };
}

impl Catalog for Flaky {
    fn run(&self, run_id: &str) -> Result<Option<RunRow>, Failure> {
        if self.failures.load(Ordering::SeqCst) > 0 {
            self.failures.fetch_sub(1, Ordering::SeqCst);
            return Err(Failure::new(FailureTag::Storage, "catalog unreachable"));
        }
        self.inner.run(run_id)
    }
    delegate! {
        now() -> Result<Instant, Failure>;
        acquire(key: &LeaseKey, holder: &str, ttl_secs: u64) -> Result<Option<Lease>, Failure>;
        release(lease: &Lease) -> Result<(), Failure>;
        lease_row(key: &LeaseKey) -> Result<LeaseRow, Failure>;
        cursor_at(scope: &OwnerScope) -> Result<CursorRow, Failure>;
        cursor_cas_at(scope: &OwnerScope, expected_version: u64, next: CursorRow, fence: Option<&Lease>) -> Result<Cas, Failure>;
        owner_at(scope: &OwnerScope) -> Result<Option<ExecutionOwner>, Failure>;
        put_owner(owner: &ExecutionOwner) -> Result<(), Failure>;
        retire_at(scope: &OwnerScope, execution_id: &str, cursor: Option<(CursorRow, u64)>, fence: Option<&Lease>) -> Result<Cas, Failure>;
        retired_at(scope: &OwnerScope) -> Result<Option<String>, Failure>;
        renew(lease: &Lease, ttl_secs: u64) -> Result<Option<Lease>, Failure>;
        lease_holds(lease: &Lease) -> Result<bool, Failure>;
        put_run(row: &RunRow) -> Result<(), Failure>;
        runs(pipeline_id: Option<&str>) -> Result<Vec<RunRow>, Failure>;
        update_run(run_id: &str, f: &mut dyn FnMut(&mut RunRow) -> Result<(), RunError>) -> Result<Option<Result<RunRow, RunError>>, Failure>;
    }
}

/// A failed poll read warns and keeps polling.
// spec: run.cancel.storage-blip@185b546a
#[test]
fn a_failed_poll_keeps_polling() {
    let rig = Rig::new();
    rig.catalog().put_run(&crate::support_row("run-1", RunStatus::Running)).unwrap();
    rig.engine.cancel("run-1", Scope::Run, None).unwrap();
    let flaky: Arc<dyn Catalog + Send + Sync> = Arc::new(Flaky { inner: rig.engine.catalog.clone(), failures: AtomicUsize::new(3) });
    let token = CancelToken::default();
    let keeper = Keeper::new(Cadence { poll: Duration::from_millis(20), renew: Duration::from_secs(10) });
    let _r = keeper.register(flaky.clone(), "run-1", token.clone(), Arc::default());
    assert!(!token.requested(), "the first read failed and fired nothing");
    let started = std::time::Instant::now();
    while !token.requested() {
        assert!(started.elapsed() < Duration::from_secs(2), "polling stopped after a failed read");
        std::thread::sleep(Duration::from_millis(5));
    }
}

/// A catalog whose run reads panic after the first `serves` of them.
struct Panics {
    inner: Arc<dyn Catalog + Send + Sync>,
    serves: AtomicUsize,
    panicked: Arc<AtomicUsize>,
}

impl Catalog for Panics {
    fn run(&self, run_id: &str) -> Result<Option<RunRow>, Failure> {
        if self.serves.load(Ordering::SeqCst) == 0 {
            self.panicked.fetch_add(1, Ordering::SeqCst);
            panic!("the catalog adapter panicked reading `{run_id}`");
        }
        self.serves.fetch_sub(1, Ordering::SeqCst);
        self.inner.run(run_id)
    }
    delegate! {
        now() -> Result<Instant, Failure>;
        acquire(key: &LeaseKey, holder: &str, ttl_secs: u64) -> Result<Option<Lease>, Failure>;
        release(lease: &Lease) -> Result<(), Failure>;
        lease_row(key: &LeaseKey) -> Result<LeaseRow, Failure>;
        cursor_at(scope: &OwnerScope) -> Result<CursorRow, Failure>;
        cursor_cas_at(scope: &OwnerScope, expected_version: u64, next: CursorRow, fence: Option<&Lease>) -> Result<Cas, Failure>;
        owner_at(scope: &OwnerScope) -> Result<Option<ExecutionOwner>, Failure>;
        put_owner(owner: &ExecutionOwner) -> Result<(), Failure>;
        retire_at(scope: &OwnerScope, execution_id: &str, cursor: Option<(CursorRow, u64)>, fence: Option<&Lease>) -> Result<Cas, Failure>;
        retired_at(scope: &OwnerScope) -> Result<Option<String>, Failure>;
        renew(lease: &Lease, ttl_secs: u64) -> Result<Option<Lease>, Failure>;
        lease_holds(lease: &Lease) -> Result<bool, Failure>;
        put_run(row: &RunRow) -> Result<(), Failure>;
        runs(pipeline_id: Option<&str>) -> Result<Vec<RunRow>, Failure>;
        update_run(run_id: &str, f: &mut dyn FnMut(&mut RunRow) -> Result<(), RunError>) -> Result<Option<Result<RunRow, RunError>>, Failure>;
    }
}

/// A keeper job that panics warns and leaves the keeper running: every other registration keeps its cadence, the
/// panicking one retries at its next deadline, and dropping it returns.
// spec: run.cancel.keeper-panic@32089033
#[test]
fn a_panicking_keeper_job_leaves_the_keeper_running() {
    let rig = Rig::new();
    for id in ["run-a", "run-b"] {
        rig.catalog().put_run(&crate::support_row(id, RunStatus::Running)).unwrap();
    }
    let panicked = Arc::new(AtomicUsize::new(0));
    let panics: Arc<dyn Catalog + Send + Sync> = Arc::new(Panics { inner: rig.engine.catalog.clone(), serves: AtomicUsize::new(1), panicked: panicked.clone() });
    let keeper = Keeper::new(Cadence { poll: Duration::from_millis(20), renew: Duration::from_secs(10) });
    // Held undropped until the drop under test, so a failed assertion unwinds without blocking on it.
    let a = std::mem::ManuallyDrop::new(keeper.register(panics, "run-a", CancelToken::default(), Arc::default()));
    let token_b = CancelToken::default();
    let b = keeper.register(rig.engine.catalog.clone(), "run-b", token_b.clone(), Arc::default());

    let started = std::time::Instant::now();
    while panicked.load(Ordering::SeqCst) < 2 {
        assert!(started.elapsed() < Duration::from_secs(5), "the panicking registration was not retried at its next deadline");
        std::thread::sleep(Duration::from_millis(5));
    }
    assert_eq!(keeper.threads(), 1);

    // The other registration still polls: a stop on run-b fires its token.
    rig.engine.cancel("run-b", Scope::Run, None).unwrap();
    let marked = std::time::Instant::now();
    while !token_b.requested() {
        assert!(marked.elapsed() < Duration::from_secs(5), "a panic in another registration's job stopped polling");
        std::thread::sleep(Duration::from_millis(5));
    }

    // Dropping the panicking registration returns rather than waiting on a job that never finishes.
    let (dropped_tx, dropped_rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        drop(std::mem::ManuallyDrop::into_inner(a));
        dropped_tx.send(()).unwrap();
    });
    dropped_rx.recv_timeout(Duration::from_secs(5)).expect("dropping the panicking registration deadlocked");
    drop(b);
    let started = std::time::Instant::now();
    while keeper.threads() > 0 {
        assert!(started.elapsed() < Duration::from_secs(5), "the keeper thread did not exit");
        std::thread::sleep(Duration::from_millis(5));
    }
}

/// A wait on the token returns at the fire, not at its deadline: a token fired before the
/// wait or during it ends a 60 s wait, and an unfired one waits its whole duration. Bounds
/// sit far from the deadlines, so a loaded host cannot flip them.
#[test]
fn a_fired_token_ends_a_wait_at_once() {
    let token = CancelToken::default();
    let started = std::time::Instant::now();
    assert!(token.wait_timeout(Duration::from_millis(30)), "an unfired wait runs to its deadline");
    assert!(started.elapsed() >= Duration::from_millis(30));

    let fired = CancelToken::default();
    fired.fire();
    let started = std::time::Instant::now();
    assert!(!fired.wait_timeout(Duration::from_secs(60)));
    assert!(started.elapsed() < Duration::from_secs(30), "a fired token waits for nothing");

    let (ready_tx, ready_rx) = std::sync::mpsc::channel();
    let waiter = {
        let token = token.clone();
        std::thread::spawn(move || {
            ready_tx.send(()).unwrap();
            let started = std::time::Instant::now();
            (token.wait_timeout(Duration::from_secs(60)), started.elapsed())
        })
    };
    ready_rx.recv().unwrap();
    std::thread::sleep(Duration::from_millis(20));
    token.fire();
    let (ran_out, waited) = waiter.join().unwrap();
    assert!(!ran_out, "the fire ended the wait");
    assert!(waited < Duration::from_secs(30), "the wait returned at the fire, not its 60 s deadline");
}

/// The keeper wakes only at a poll or renewal deadline: over 10 s at the default cadence it
/// wakes 20 times, polling each time and renewing once, and a late wakeup runs each job
/// once rather than catching up. The schedule takes its instants as arguments, so no wall
/// clock enters the assertion.
#[test]
fn the_keeper_wakes_only_on_its_deadlines() {
    let cadence = Cadence::default();
    assert_eq!(cadence.renew, Duration::from_secs(10));
    let t0 = std::time::Instant::now();
    let mut schedule = Schedule::new(cadence, t0);
    let (mut wakeups, mut polls, mut renewals) = (0, 0, 0);
    while schedule.next() <= t0 + Duration::from_secs(10) {
        let at = schedule.next();
        let due = schedule.take(at);
        assert!(due.poll || due.renew, "a wakeup with nothing due at {:?}", at - t0);
        wakeups += 1;
        polls += usize::from(due.poll);
        renewals += usize::from(due.renew);
    }
    assert_eq!((wakeups, polls, renewals), (20, 20, 1));

    // Nothing is due before the next deadline.
    let mut schedule = Schedule::new(cadence, t0);
    assert_eq!(schedule.take(t0 + Duration::from_millis(499)), Due::default());
    // Woken 3 s late, the keeper polls once and renews once, then resumes from that instant.
    let late = t0 + Duration::from_secs(13);
    assert_eq!(schedule.take(late), Due { poll: true, renew: true });
    assert_eq!(schedule.next(), late + Duration::from_millis(500));
}

/// Dropping the last registration of a keeper whose next deadline is an hour away returns
/// at once, and its thread exits without waiting on that deadline.
#[test]
fn a_dropped_registration_ends_the_keeper_thread_at_once() {
    let rig = Rig::new();
    rig.catalog().put_run(&crate::support_row("run-1", RunStatus::Running)).unwrap();
    let hour = Duration::from_secs(3600);
    let keeper = Keeper::new(Cadence { poll: hour, renew: hour });
    let registration = keeper.register(rig.engine.catalog.clone(), "run-1", CancelToken::default(), Arc::default());
    assert_eq!((keeper.threads(), keeper.registered()), (1, 1));
    let started = std::time::Instant::now();
    drop(registration);
    assert_eq!(keeper.registered(), 0);
    while keeper.threads() > 0 {
        assert!(started.elapsed() < Duration::from_secs(30), "the keeper thread waited on its deadline");
        std::thread::sleep(Duration::from_millis(5));
    }
}

/// A table run's own stop does not cut landing; bytes already home finish landing and record the unfulfilled stop.
/// Prepared body outputs additionally require {{run.own.body-parent}}.
// spec: run.cancel.land-path-uncut@cbd2730d
#[test]
fn a_stop_arriving_once_the_last_batch_is_home_lets_it_land_and_records_the_stop() {
    let rig = Rig::new();
    let engine = rig.engine.clone();
    let mut sink = Sink {
        on_stage: Some(Box::new(move |_| {
            engine.cancel("run-1", Scope::Run, Some("too late".into())).unwrap();
            // The keeper polls every 20 ms; the token has fired before the land path runs.
            std::thread::sleep(Duration::from_millis(150));
        })),
        ..Sink::default()
    };
    let mut source = Pages::new(vec![vec![serde_json::json!({ "id": "only" })]]);
    let row = rig.run(&plan("kind = \"opaque-token\"", ""), "1.0.0", "run-1", &mut source, &mut sink).unwrap();
    assert_eq!(row.status, RunStatus::Success, "the stop did not cut the land path: {:?}", row.error_message);
    assert_eq!(sink.commits.len(), 1, "the batch already home landed");
    assert_eq!(rig.catalog().cursor("feed", "filings").unwrap().position, Some(serde_json::json!("p1")));
    let stored = rig.catalog().run("run-1").unwrap().unwrap();
    assert_eq!(stored.stop.as_ref().and_then(|s| s.reason.as_deref()), Some("too late"), "the unfulfilled stop stays recorded");
}
