//! `run.cancel`: the stop mark, the token and what a stop leaves behind.

use crate::support::{plan, three_pages, Pages, Rig, Sink};
use contextful_core::coordinate::{Cas, Catalog, CursorRow, Lease, LeaseKey, LeaseRow};
use contextful_core::run::cancel::Scope;
use contextful_core::run::own::ExecutionOwner;
use contextful_core::run::ports::{Cancellation, PullRequest, Source};
use contextful_core::run::record::{RunRow, RunStatus};
use contextful_core::run::{Failure, FailureTag, RunError};
use contextful_core::time::Instant;
use contextful_engine::cancel::{Cadence, CancelToken, Keeper};
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
    let token = CancelToken::default();
    let _k = Keeper::start(c.clone(), "run-early", token.clone(), Cadence::default());
    assert!(token.requested());
    // A later stop is seen within one interval.
    c.put_run(&crate::support_row("run-late", RunStatus::Running)).unwrap();
    let token = CancelToken::default();
    let _k = Keeper::start(c.clone(), "run-late", token.clone(), Cadence::default());
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
        cursor(pipeline_id: &str, table: &str) -> Result<CursorRow, Failure>;
        cursor_cas(pipeline_id: &str, table: &str, expected_version: u64, next: CursorRow, fence: Option<&Lease>) -> Result<Cas, Failure>;
        owner(pipeline_id: &str, table: &str) -> Result<Option<ExecutionOwner>, Failure>;
        put_owner(owner: &ExecutionOwner) -> Result<(), Failure>;
        retire(pipeline_id: &str, table: &str, execution_id: &str, cursor: Option<(CursorRow, u64)>, fence: Option<&Lease>) -> Result<Cas, Failure>;
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
    let _k = Keeper::start(flaky.clone(), "run-1", token.clone(), Cadence { poll: Duration::from_millis(20), renew: Duration::from_secs(10) });
    assert!(!token.requested(), "the first read failed and fired nothing");
    let started = std::time::Instant::now();
    while !token.requested() {
        assert!(started.elapsed() < Duration::from_secs(2), "polling stopped after a failed read");
        std::thread::sleep(Duration::from_millis(5));
    }
}
