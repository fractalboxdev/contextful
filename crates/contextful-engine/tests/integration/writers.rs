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

#[test]
fn an_append_writer_superseded_after_staging_cannot_publish() {
    let rig = Rig::new();
    let catalog = rig.engine.catalog.clone();
    let mut sink = Sink {
        on_stage: Some(Box::new(move |_| {
            let scope = contextful_core::run::own::OwnerScope::table("feed", "filings");
            let mut owner = catalog.owner_at(&scope).unwrap().expect("staged work has an owner");
            owner.execution_id = "replacement-execution".into();
            owner.attempts = vec!["replacement-attempt".into()];
            catalog.put_owner(&owner).unwrap();
        })),
        ..Sink::default()
    };
    let mut source = Pages::new(vec![vec![json!({"id":"a", "updated_at":1})]]);
    let row = rig.run(&plan("kind = \"monotonic\"\nfield = \"updated_at\"", ""), "1.0.0", "old-container", &mut source, &mut sink).unwrap();
    assert_eq!(sink.staged.len(), 1);
    assert_eq!(row.status, RunStatus::Failed);
    assert!(sink.commits.is_empty());
    assert_eq!(rig.catalog().cursor("feed", "filings").unwrap().position, None);
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
    assert_eq!(row.error_kind, Some(contextful_core::run::FailureTag::Storage));
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

/// Live run state is a best-effort in-memory projection of events the runner emits after durable state changes;
/// execution never reads it, and a restart discards it, so a subscriber recovers from the durable record.
// spec: run.project.best-effort@1ff0ae38
#[test]
fn a_run_lands_the_same_whether_its_projection_drops_every_event_or_none() {
    let run = |emitter: Option<contextful_engine::project::Emitter>| {
        let dir = tempfile::tempdir().unwrap();
        let clock = SetClock::new(crate::support::T0);
        let engine = Engine {
            catalog: Arc::new(LocalCatalog::open(dir.path(), Arc::new(clock))),
            journal: Journal::open(dir.path()),
            awakeables: None,
            keeper: Keeper::default(),
            emitter,
            worlds: Vec::new(),
        };
        let mut sink = Sink::default();
        let row = rig_run(&engine, "run-seen", &mut Pages::new(three_pages()), &mut sink);
        let stored = engine.catalog.run("run-seen").unwrap().expect("the durable row");
        (dir, row, stored, sink.commits)
    };
    let (_unprojected, row, stored, commits) = run(None);
    // A rendezvous channel no one drains drops every non-terminal delta the runner emits.
    let hub = Hub::with_capacity(crate::support::at(crate::support::T0), 0);
    let (_projected, seen_row, seen_stored, seen_commits) = run(Some(hub.emitter()));
    assert!(hub.dropped() > 0, "the projection lost events");
    assert_eq!((seen_row.status, seen_row.rows), (row.status, row.rows), "execution reads nothing back from its projection");
    assert_eq!((seen_stored.status, seen_stored.rows), (stored.status, stored.rows));
    assert_eq!(seen_commits.len(), commits.len());
    assert_eq!(seen_commits[0].batches, commits[0].batches);
    assert_eq!(seen_commits[0].cursor, commits[0].cursor);
    // The terminal transition the runner emitted after its durable close reads what the record holds.
    hub.pump(crate::support::at("2030-01-01T00:00:10Z"));
    let (snapshot, _) = hub.connect("run-seen").expect("the terminal transition survives the drops");
    assert_eq!(snapshot.status, seen_stored.status);
}

/// Where the catalog calls of one received landing are written, for the D1 replay.
const RECEIVE_LANDING_TRACE: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/catalog/receive-landing.json");

/// The batches one measured group carries.
const GROUP_MEMBERS: usize = 16;

/// Delivered batches land over a counting catalog through the calls a receiver makes per
/// batch: the admission lookup (`receive_first`), the leader's replay lookup
/// (`receive_landed`), then the run. One warm-up landing under `k0`; the measured lone
/// landing under `k1`, the figure every batch that finds no other spooled batch pays; then a
/// measured group of 16 one-row batches under `k2`..`k17` through `run_receive_group`,
/// whose figures are per received batch.
#[test]
fn a_received_batch_counts_its_catalog_calls_per_landing() {
    use crate::support::{Counted, CATALOG_WRITES};
    use contextful_core::run::ports::Unshaped;
    let dir = tempfile::tempdir().unwrap();
    let clock = SetClock::new(crate::support::T0);
    let counted = Arc::new(Counted::new(LocalCatalog::open(dir.path(), Arc::new(clock.clone()))));
    // A keeper whose poll and renewal never come due inside a landing, so every call is the runner's own.
    let engine = Engine {
        catalog: counted.clone(),
        journal: Journal::open(dir.path()),
        awakeables: None,
        keeper: Keeper::new(Cadence { poll: Duration::from_secs(3600), renew: Duration::from_secs(3600) }),
        emitter: None,
        worlds: Vec::new(),
    };
    let p = plan("kind = \"opaque-token\"", "");
    let spec = |run_id: String| contextful_engine::RunSpec {
        connector: p.connector_pin("artifact-1"),
        plan: p.clone(),
        run_id,
        site_id: "site-a".into(),
        pid: 4242,
        boot_id: "boot-a".into(),
        trace_id: None,
    };
    // A batch's lookups before its run: its first run for the retention check, then the
    // run that committed it, or the id its next attempt takes.
    let looked_up = |key: &str| -> String {
        assert!(engine.receive_first("feed", "filings", key).unwrap().is_none(), "`{key}` is a fresh key");
        engine.receive_landed("feed", "filings", key).unwrap().expect_err("a fresh key has no committed run")
    };
    let lone = |key: &str| {
        let next = looked_up(key);
        let row = engine.run_receive(&spec(next), key, &mut Pages::new(vec![vec![json!({ "id": format!("{key}-row") })]]), &Unshaped, &mut Sink::default()).unwrap();
        assert_eq!(row.status, RunStatus::Success, "{:?}", row.error_message);
        row
    };
    let first = lone("k0");
    let warmup = counted.take();
    clock.advance(1);
    let alone = lone("k1");
    let landing = counted.take();
    clock.advance(1);
    let members: Vec<String> = (2..2 + GROUP_MEMBERS).map(|i| format!("k{i}")).collect();
    for key in &members {
        looked_up(key);
    }
    let next = engine.receive_attempt(&contextful_engine::receive_stem("feed", "filings", "group-1")).unwrap().expect_err("a fresh group");
    let rows: Vec<serde_json::Value> = members.iter().map(|k| json!({ "id": format!("{k}-row") })).collect();
    let grouped = engine.run_receive_group(&spec(next), "group-1", &members, &mut Pages::new(vec![rows]), &Unshaped, &mut Sink::default()).unwrap();
    assert_eq!(grouped.status, RunStatus::Success, "{:?}", grouped.error_message);
    let group = counted.take();
    for key in &members {
        assert!(engine.receive_group("feed", "filings", key).unwrap().is_some(), "member `{key}` names its group");
        assert_eq!(engine.receive_landed("feed", "filings", key).unwrap().map(|r| r.run_id).ok(), Some(grouped.run_id.clone()), "member `{key}` replays the group run");
    }

    let writes = |calls: &[serde_json::Value]| calls.iter().filter(|c| CATALOG_WRITES.contains(&c["op"].as_str().unwrap())).count();
    assert!(!landing.is_empty() && writes(&landing) > 0, "a landing reads and writes the catalog");
    let per_batch = |n: usize| n as f64 / GROUP_MEMBERS as f64;
    let group_calls = per_batch(group.len());
    assert!(group_calls <= 12.0, "a received batch costs at most 12 catalog calls inside a group: {group_calls}");
    contextful_eval::record::emit("catalog-calls-per-landing", landing.len() as f64, 1, 0);
    contextful_eval::record::emit("catalog-calls-per-landing-writes", writes(&landing) as f64, 1, 0);
    contextful_eval::record::emit("catalog-calls-per-group-batch", group_calls, GROUP_MEMBERS as u64, 0);
    contextful_eval::record::emit("catalog-calls-per-group-batch-writes", per_batch(writes(&group)), GROUP_MEMBERS as u64, 0);

    // Execution ids are random; the trace names them by landing so its bytes stay fixed.
    let mut text = serde_json::to_string_pretty(&json!({ "warmup": warmup, "landing": landing, "group": group, "group_members": GROUP_MEMBERS })).unwrap();
    for (row, name) in [(&first, "execution-k0"), (&alone, "execution-k1"), (&grouped, "execution-group-1")] {
        text = text.replace(&row.execution_id, name);
    }
    // The D1 replay reads this trace; `CONTEXTFUL_BLESS=1` rewrites it, any other run compares.
    let path = std::path::Path::new(RECEIVE_LANDING_TRACE);
    let expected = format!("{text}\n");
    if std::env::var_os("CONTEXTFUL_BLESS").is_some() {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, &expected).unwrap();
    } else {
        let held = std::fs::read_to_string(path).unwrap_or_default();
        assert!(held == expected, "{} is stale; rerun with CONTEXTFUL_BLESS=1", path.display());
    }
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
