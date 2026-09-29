//! The single-writer lease across a long run, the fenced commit, the cursor
//! compare-and-swap between monotonic writers, and the shared owner a refused run leaves alone.

use crate::support::{plan, three_pages, Pages, Rig, SetClock, Sink};
use contextful_core::coordinate::{CursorRow, LeaseKey};
use contextful_core::run::ports::{Cancellation, PullRequest, Source};
use contextful_core::run::record::RunStatus;
use contextful_core::run::Failure;
use contextful_engine::cancel::{Cadence, Keeper};
use contextful_engine::project::Hub;
use contextful_engine::{Engine, Journal, LocalCatalog};
use serde_json::json;
use std::sync::Arc;
use std::time::Duration;

fn key() -> LeaseKey {
    LeaseKey::Pipeline("feed/filings".into())
}

/// A source running `during` inside each pull, then serving a page.
struct During<F: FnMut(usize)> {
    during: F,
    calls: usize,
    pages: usize,
}

impl<F: FnMut(usize)> Source for During<F> {
    fn pull(&mut self, request: &PullRequest, _: &dyn Cancellation) -> Result<Vec<u8>, Failure> {
        (self.during)(self.calls);
        self.calls += 1;
        let n = self.calls;
        let more = n < self.pages;
        let _ = request;
        Ok(serde_json::to_vec(&json!({"rows": [{"id": format!("d{n}"), "updated_at": n * 10}], "cursor": format!("p{n}"), "more": more})).unwrap())
    }
}

fn fast_renewing(rig: &Rig) -> Engine {
    Engine { keeper: Keeper::new(Cadence { poll: Duration::from_millis(20), renew: Duration::from_millis(20) }), ..rig.engine.clone() }
}

#[test]
fn a_run_outliving_one_lease_ttl_keeps_its_lease() {
    let rig = Rig::new();
    let engine = fast_renewing(&rig);
    let (catalog, clock) = (rig.engine.catalog.clone(), rig.clock.clone());
    let mut intrusions = Vec::new();
    let mut source = During {
        during: |_| {
            // 25 s of catalog time pass in each pull; the keeper renews in between.
            clock.advance(25);
            std::thread::sleep(Duration::from_millis(80));
            intrusions.push(catalog.acquire(&key(), "intruder", 30).unwrap().is_some());
        },
        calls: 0,
        pages: 3,
    };
    let row = rig_run(&engine, "run-long", &mut source, &mut Sink::default());
    assert_eq!(row.status, RunStatus::Success, "{:?}", row.error_message);
    assert_eq!(intrusions, [false, false, false], "75 s into a 30 s lease, no second writer takes it");
}

#[test]
fn a_writer_whose_lease_passed_to_another_lands_nothing() {
    let rig = Rig::new();
    let (catalog, clock) = (rig.engine.catalog.clone(), rig.clock.clone());
    let mut source = During {
        during: |_| {
            // The lease lapses unrenewed and a second writer takes the next fence.
            clock.advance(31);
            assert!(catalog.acquire(&key(), "second-writer", 30).unwrap().is_some());
        },
        calls: 0,
        pages: 1,
    };
    let mut sink = Sink::default();
    let row = rig_run(&rig.engine, "run-fenced", &mut source, &mut sink);
    assert_eq!(row.status, RunStatus::Failed);
    assert!(row.error_message.unwrap().contains("LeaseFenced"));
    assert!(sink.commits.is_empty(), "the fenced writer committed nothing");
    assert_eq!(rig.catalog().cursor("feed", "filings").unwrap().position, None);
}

fn rig_run(engine: &Engine, run_id: &str, source: &mut dyn Source, sink: &mut Sink) -> contextful_core::run::record::RunRow {
    let p = plan("kind = \"opaque-token\"", "");
    let spec = contextful_engine::RunSpec {
        connector: p.connector_pin("artifact-1"),
        plan: p,
        run_id: run_id.into(),
        site_id: "site-a".into(),
        pid: 1,
        boot_id: "boot".into(),
        trace_id: None,
    };
    engine.run(&spec, source, sink).unwrap()
}

#[test]
fn a_monotonic_writer_retiring_last_never_moves_the_cursor_back() {
    let rig = Rig::new();
    let catalog = rig.engine.catalog.clone();
    let p = plan("kind = \"monotonic\"\nfield = \"updated_at\"", "");
    // While this run pulls rows up to 10, a concurrent writer commits position 50.
    let mut source = During {
        during: |_| {
            let v = catalog.cursor("feed", "filings").unwrap().version;
            let ahead = CursorRow { position: Some(json!({"field": "updated_at", "at": 50})), marker_run_id: Some("run-ahead".into()), ..CursorRow::default() };
            assert_eq!(catalog.cursor_cas("feed", "filings", v, ahead, None).unwrap(), contextful_core::coordinate::Cas::Applied);
        },
        calls: 0,
        pages: 1,
    };
    let mut sink = Sink::default();
    let row = rig.run(&p, "1.0.0", "run-behind", &mut source, &mut sink).unwrap();
    assert_eq!(row.status, RunStatus::Success);
    let cursor = rig.catalog().cursor("feed", "filings").unwrap();
    assert_eq!(cursor.position, Some(json!({"field": "updated_at", "at": 50})), "the highest position wins");
    assert_eq!(cursor.marker_run_id.as_deref(), Some("run-ahead"));
    assert_eq!(sink.commits[0].cursor, Some(json!({"field": "updated_at", "at": 10})));
}

#[test]
fn the_cached_marker_instant_is_the_commits_own() {
    let rig = Rig::new();
    let mut sink = Sink::default();
    rig.run(&plan("kind = \"opaque-token\"", ""), "1.0.0", "run-1", &mut Pages::new(three_pages()), &mut sink).unwrap();
    assert_eq!(rig.catalog().cursor("feed", "filings").unwrap().marker_committed_at, Some(sink.commits[0].committed_at));
}

#[test]
fn a_run_refused_the_lease_leaves_the_holders_owner_and_journal_alone() {
    let rig = Rig::new();
    let engine = rig.engine.clone();
    let mut refused = None;
    let mut source = During {
        during: |call| {
            if call == 1 {
                // A second run fires while this one holds the lease and a recorded pull.
                let mut other = Pages::new(three_pages());
                refused = Some(rig_run(&engine, "run-second", &mut other, &mut Sink::default()));
                assert!(other.calls().is_empty());
            }
        },
        calls: 0,
        pages: 3,
    };
    let mut sink = Sink::default();
    let row = rig_run(&rig.engine, "run-first", &mut source, &mut sink);
    let second = refused.unwrap();
    assert_eq!(second.status, RunStatus::Failed);
    assert_eq!(row.status, RunStatus::Success, "{:?}", row.error_message);
    assert_eq!(sink.commits.len(), 1);
    assert_eq!(sink.commits[0].batches.len(), 3, "the first run's recorded pulls survived the second run's failure");
}

#[test]
fn the_runner_projects_each_step_and_its_terminal_status() {
    let dir = tempfile::tempdir().unwrap();
    let clock = SetClock::new(crate::support::T0);
    let hub = Hub::new(crate::support::at(crate::support::T0));
    let engine = Engine {
        catalog: Arc::new(LocalCatalog::open(dir.path(), Arc::new(clock.clone()))),
        journal: Journal::open(dir.path()),
        awakeables: None,
        keeper: Keeper::default(),
        emitter: Some(hub.emitter()),
        worlds: Vec::new(),
    };
    rig_run(&engine, "run-seen", &mut Pages::new(three_pages()), &mut Sink::default());
    hub.pump(crate::support::at("2030-01-01T00:00:10Z"));
    let (snapshot, _) = hub.connect("run-seen").expect("the run is projected");
    assert_eq!(snapshot.status, RunStatus::Success);
    let steps: Vec<(&str, _)> = snapshot.steps.iter().map(|s| (s.label.as_str(), s.status)).collect();
    use contextful_core::run::project::StepStatus::Completed;
    assert_eq!(steps, [("pull-0", Completed), ("pull-1", Completed), ("pull-2", Completed)]);
}

#[test]
fn a_refused_fence_record_releases_the_lease() {
    let rig = Rig::new();
    let mut sink = Sink { refuse_fence: true, ..Sink::default() };
    let row = rig.run(&plan("kind = \"opaque-token\"", ""), "1.0.0", "run-1", &mut Pages::new(three_pages()), &mut sink).unwrap();
    assert_eq!(row.status, RunStatus::Failed);
    let lease = rig.catalog().lease_row(&key()).unwrap();
    assert_eq!(lease.holder, None, "the lease was released");
    assert!(rig.catalog().acquire(&key(), "next", 30).unwrap().is_some());
}
