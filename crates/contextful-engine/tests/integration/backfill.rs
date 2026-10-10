//! `run.own`, `run.cancel` and `run.backfill`: the chunk scheduler, chunk retirement, stops and
//! rewinds, and the seeding scope beside the live table.

use crate::support::Rig;
use contextful_core::run::backfill::{Bound, ChunkRow, ChunkStatus, Window};
use contextful_core::run::cancel::Scope;
use contextful_core::run::own::{ConnectorPin, OwnerPins, OwnerScope, Pins};
use contextful_core::run::ports::{Cancellation, ExecutionPort, OpenExecution, Outcome};
use contextful_core::run::record::RunStatus;
use contextful_core::run::retry::Schedule;
use contextful_core::run::{Failure, RunError};
use contextful_engine::EngineError;
use serde_json::json;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

const PIPELINE: &str = "feed";
const TABLE: &str = "filings";

fn connector(id: &str, version: &str) -> ConnectorPin {
    ConnectorPin { id: id.into(), version: version.into(), world: "native".into(), hash: format!("h-{id}-{version}") }
}

/// An open of `scope` as attempt `run_id`, admitting connector `id` at `version`.
fn open_as(scope: OwnerScope, id: &str, version: &str, run_id: &str) -> OpenExecution {
    let pin = connector(id, version);
    OpenExecution {
        scope,
        pins: OwnerPins::Build(Pins { connector: pin.clone(), content_hash: "plan-1".into(), input_hash: String::new() }),
        run_id: run_id.into(),
        site_id: "site-a".into(),
        pid: 4242,
        boot_id: "boot-a".into(),
        trace_id: None,
        connector: Some(pin),
        schedule: Schedule::single_attempt(),
    }
}

fn open(scope: OwnerScope, run_id: &str) -> OpenExecution {
    open_as(scope, "vendor", "1.0.0", run_id)
}

fn window(start: i64, end: i64) -> Window {
    Window::new(Bound::Int(start), Bound::Int(end)).unwrap()
}

/// Plan three ten-wide chunks `c0`, `c1`, `c2` over `[0, 30)`.
fn three_chunks(rig: &Rig) -> Vec<ChunkRow> {
    let windows: Vec<(String, Window)> = (0..3).map(|i| (format!("c{i}"), window(i * 10, i * 10 + 10))).collect();
    rig.engine.backfill().plan(PIPELINE, TABLE, &windows).unwrap()
}

fn chunk(rig: &Rig, name: &str) -> ChunkRow {
    rig.catalog().chunk_at(&OwnerScope::chunk(PIPELINE, TABLE, name)).unwrap().unwrap_or_else(|| panic!("no chunk `{name}`"))
}

/// An effect that counts its runs and answers `value`.
fn counted<'a>(runs: &'a AtomicUsize, value: &'a [u8]) -> impl FnMut(&dyn Cancellation) -> Result<Vec<u8>, Failure> + 'a {
    move |_| {
        runs.fetch_add(1, Ordering::SeqCst);
        Ok(value.to_vec())
    }
}

/// Poll `holds` every 5 ms until it answers true, failing with `what` after 10 s.
fn eventually(what: &str, holds: impl Fn() -> bool) {
    let started = std::time::Instant::now();
    while !holds() {
        assert!(started.elapsed() < Duration::from_secs(10), "{what}");
        std::thread::sleep(Duration::from_millis(5));
    }
}

/// Retiring an owner and caching its committed position share one catalog transaction; a later fire receives a
/// fresh execution id even at a byte-identical position. A completed backfill chunk retires the same way.
// spec: run.own.retirement@5751b450
#[test]
fn a_commit_retires_its_owner_with_the_position_and_a_chunk_completes_in_that_transaction() {
    let rig = Rig::new();
    let table = OwnerScope::table(PIPELINE, TABLE);

    // A table commit caches the position and retires the owner in one rewrite of the scope row.
    let mut x = rig.engine.open_execution(&open(table.clone(), "run-1")).unwrap();
    let first = x.execution_id().to_string();
    x.step("pull-0", b"null", &mut |_| Ok(b"page".to_vec())).unwrap();
    x.commit(Some(json!({"at": 7}))).unwrap();
    let row: serde_json::Value = serde_json::from_slice(&std::fs::read(rig.dir.path().join("scopes/feed/filings.json")).unwrap()).unwrap();
    assert_eq!((row["cursor"]["position"].clone(), row["owner"].clone()), (json!({"at": 7}), serde_json::Value::Null));
    assert_eq!(rig.engine.journal.recorded(&first).unwrap(), 0, "the retired owner's journal is collected");
    x.close(Outcome::Success { rows: 1, bytes: 1, batches: 1 }).unwrap();

    // A later fire at the byte-identical position receives a fresh execution id, twice over.
    let mut y = rig.engine.open_execution(&open(table.clone(), "run-2")).unwrap();
    assert_eq!(y.position(), Some(&json!({"at": 7})));
    let second = y.execution_id().to_string();
    assert_ne!(second, first);
    y.commit(Some(json!({"at": 7}))).unwrap();
    y.close(Outcome::Success { rows: 0, bytes: 0, batches: 0 }).unwrap();
    let z = rig.engine.open_execution(&open(table, "run-3")).unwrap();
    assert!(z.execution_id() != first && z.execution_id() != second);
    drop(z);

    // A chunk's commit marks it done, caches its position and retires its owner in the same rewrite.
    let plan = three_chunks(&rig);
    assert_eq!(plan.len(), 3);
    let backfill = rig.engine.backfill();
    let c0 = backfill.next(PIPELINE, TABLE).unwrap().expect("a fresh plan has a claimable chunk");
    assert_eq!(c0.chunk, "c0");
    let mut x = backfill.open(&c0, &open(table_scope(), "run-c0")).unwrap();
    assert_eq!(x.scope(), &c0.scope());
    assert_eq!((chunk(&rig, "c0").status, chunk(&rig, "c0").attempts), (ChunkStatus::Running, 1));
    x.step("pull-0", b"null", &mut |_| Ok(b"page".to_vec())).unwrap();
    x.commit(Some(json!({"at": 10}))).unwrap();
    let stored: serde_json::Value = serde_json::from_slice(&std::fs::read(rig.dir.path().join("scopes/feed/%chunk/filings/c0.json")).unwrap()).unwrap();
    assert_eq!(stored["chunk"]["status"], json!("done"));
    assert_eq!(stored["chunk"]["cursor_committed"], json!(true));
    assert_eq!((stored["cursor"]["position"].clone(), stored["owner"].clone()), (json!({"at": 10}), serde_json::Value::Null));
    assert_eq!(x.close(Outcome::Success { rows: 1, bytes: 1, batches: 1 }).unwrap().status, RunStatus::Success);
    let done = chunk(&rig, "c0");
    assert_eq!((done.status, done.attempts, done.completed_at.is_some()), (ChunkStatus::Done, 1, true));
    assert_eq!(backfill.next(PIPELINE, TABLE).unwrap().map(|c| c.chunk), Some("c1".to_string()), "the plan resumes at the first chunk not done");

    // A commit whose cursor version moved applies nothing: the chunk stays running and its owner pending.
    let c1 = backfill.next(PIPELINE, TABLE).unwrap().unwrap();
    let mut x = backfill.open(&c1, &open(table_scope(), "run-c1")).unwrap();
    let at = c1.scope();
    let moved = rig.catalog().cursor_at(&at).unwrap();
    rig.catalog().cursor_cas_at(&at, moved.version, moved.clone(), None).unwrap();
    assert!(x.commit(Some(json!({"at": 20}))).is_err());
    assert_eq!(chunk(&rig, "c1").status, ChunkStatus::Running);
    assert!(!chunk(&rig, "c1").cursor_committed);
    assert_eq!(rig.catalog().owner_at(&at).unwrap().map(|o| o.execution_id), Some(x.execution_id().to_string()));
    drop(x);
}

/// The scope a chunk open names before the scheduler sets the chunk's own.
fn table_scope() -> OwnerScope {
    OwnerScope::table(PIPELINE, TABLE)
}

/// Restoring the recorded build and resuming to completion clears a pending owner; an explicit chunk rewind retires
/// the owners its window covers.
// spec: run.own.pin-recovery@9d05ee6c
#[test]
fn a_restored_build_resumes_to_completion_and_a_rewind_retires_the_owners_it_covers() {
    let rig = Rig::new();
    let table = OwnerScope::table(PIPELINE, TABLE);
    let pulls = AtomicUsize::new(0);

    // A run on build 1.0.0 records a pull and fails, holding its owner.
    let mut x = rig.engine.open_execution(&open_as(table.clone(), "vendor", "1.0.0", "run-1")).unwrap();
    let held = x.execution_id().to_string();
    x.step("pull-0", b"null", &mut counted(&pulls, b"page")).unwrap();
    x.close(Outcome::Failed(Failure::new(contextful_core::run::FailureTag::Transient, "vendor timed out"))).unwrap();
    assert!(rig.catalog().owner_at(&table).unwrap().is_some());

    // The moved build is refused; the restored one resumes, replays and completes, clearing the owner.
    match rig.engine.open_execution(&open_as(table.clone(), "vendor", "2.0.0", "run-2")) {
        Err(EngineError::Refused(RunError::ExecutionPinMismatch(_))) => {}
        other => panic!("expected ExecutionPinMismatch, got {:?}", other.map(|x| x.run_id().to_string())),
    }
    let mut x = rig.engine.open_execution(&open_as(table.clone(), "vendor", "1.0.0", "run-3")).unwrap();
    assert_eq!(x.execution_id(), held);
    assert_eq!(x.step("pull-0", b"null", &mut counted(&pulls, b"page")).unwrap(), b"page");
    assert_eq!(pulls.load(Ordering::SeqCst), 1, "the resumed pull replays from the journal");
    x.commit(Some(json!({"at": 1}))).unwrap();
    x.close(Outcome::Success { rows: 1, bytes: 1, batches: 1 }).unwrap();
    assert!(rig.catalog().owner_at(&table).unwrap().is_none());

    // c0 completes; c1 and c2 each hold a pending owner with a recorded pull.
    three_chunks(&rig);
    let backfill = rig.engine.backfill();
    let mut x = backfill.open(&chunk(&rig, "c0"), &open(table.clone(), "run-c0")).unwrap();
    x.commit(Some(json!({"at": 10}))).unwrap();
    x.close(Outcome::Success { rows: 1, bytes: 1, batches: 1 }).unwrap();
    let mut pending = Vec::new();
    for name in ["c1", "c2"] {
        let mut x = backfill.open(&chunk(&rig, name), &open(table.clone(), &format!("run-{name}"))).unwrap();
        x.step("pull-0", b"null", &mut |_| Ok(b"page".to_vec())).unwrap();
        pending.push(x.execution_id().to_string());
        x.close(Outcome::Failed(Failure::new(contextful_core::run::FailureTag::Transient, "vendor timed out"))).unwrap();
    }
    assert_eq!(rig.engine.journal.recorded(&pending[0]).unwrap(), 1);

    // A rewind over [5, 15) covers c0 and c1: both return to pending and c1's owner retires with its journal.
    let rewound = backfill.rewind(PIPELINE, TABLE, &window(5, 15)).unwrap();
    assert_eq!(rewound.iter().map(|c| c.chunk.as_str()).collect::<Vec<_>>(), ["c0", "c1"]);
    for name in ["c0", "c1"] {
        let c = chunk(&rig, name);
        assert_eq!((c.status, c.cursor_committed, c.completed_at), (ChunkStatus::Pending, false, None), "{name}");
        assert!(rig.catalog().owner_at(&c.scope()).unwrap().is_none(), "{name}'s owner retires");
    }
    assert_eq!(rig.engine.journal.recorded(&pending[0]).unwrap(), 0, "the retired owner's journal is collected");
    let c2 = rig.catalog().owner_at(&OwnerScope::chunk(PIPELINE, TABLE, "c2")).unwrap().expect("c2 sits outside the window");
    assert_eq!(c2.execution_id, pending[1]);
    assert_eq!(rig.engine.journal.recorded(&pending[1]).unwrap(), 1);

    // The rewound chunk's next open runs a fresh execution.
    let x = backfill.open(&chunk(&rig, "c1"), &open(table, "run-c1-again")).unwrap();
    assert_ne!(x.execution_id(), pending[0]);
    drop(x);
}

/// An inverted, empty or scale-incomparable rewind window raises `PipelineRewindWindowInvalid`; a window overlapping
/// no chunk is a no-op.
// spec: run.backfill.rewind-invalid@930452cb
#[test]
fn an_inverted_empty_or_off_scale_window_is_refused_and_a_window_past_the_plan_rewinds_nothing() {
    let rig = Rig::new();
    let plan = three_chunks(&rig);
    assert_eq!(plan.len(), 3);
    for (start, end) in [(Bound::Int(20), Bound::Int(10)), (Bound::Int(10), Bound::Int(10)), (Bound::Int(0), Bound::Text("2030-01-01T00:00:00Z".into()))] {
        assert!(matches!(Window::new(start, end), Err(RunError::PipelineRewindWindowInvalid(_))));
    }
    let text = Window::new(Bound::Text("2030-01-01T00:00:00Z".into()), Bound::Text("2030-02-01T00:00:00Z".into())).unwrap();
    assert!(matches!(rig.engine.backfill().rewind(PIPELINE, TABLE, &text), Err(EngineError::Refused(RunError::PipelineRewindWindowInvalid(_)))));

    let mut x = rig.engine.backfill().open(&plan[0], &open(table_scope(), "run-c0")).unwrap();
    x.commit(Some(json!({"at": 10}))).unwrap();
    x.close(Outcome::Success { rows: 1, bytes: 1, batches: 1 }).unwrap();
    assert!(rig.engine.backfill().rewind(PIPELINE, TABLE, &window(100, 200)).unwrap().is_empty());
    assert_eq!(chunk(&rig, "c0").status, ChunkStatus::Done, "a window past the plan rewinds nothing");
}

/// A stopped run's recorded pull keeps its owner and replays next attempt; a stopped pull that recorded nothing
/// releases it. A stopped chunk returns to pending, its attempt count unchanged. A stop advances no position.
// spec: run.cancel.resumable-remains@58ce632b
#[test]
fn a_stopped_chunk_returns_to_pending_keeping_a_recorded_pull_and_its_position() {
    let rig = Rig::new();
    three_chunks(&rig);
    let backfill = rig.engine.backfill();
    let pulls = AtomicUsize::new(0);

    // c0 records a pull, then a stop lands.
    let c0 = backfill.next(PIPELINE, TABLE).unwrap().unwrap();
    let mut x = backfill.open(&c0, &open(table_scope(), "run-1")).unwrap();
    let held = x.execution_id().to_string();
    x.step("pull-0", b"null", &mut counted(&pulls, b"page")).unwrap();
    assert_eq!(rig.engine.cancel("run-1", Scope::Run, Some("operator".into())).unwrap(), vec!["run-1".to_string()]);
    eventually("the keeper fires the stopped run's token", || x.token().requested());
    let closed = x.close(Outcome::Failed(Failure::canceled("stopped before the second pull"))).unwrap();
    assert_eq!(closed.status, RunStatus::Canceled);

    let stopped = chunk(&rig, "c0");
    assert_eq!((stopped.status, stopped.attempts), (ChunkStatus::Pending, 1), "a stopped chunk returns to pending, its attempt count unchanged");
    assert_eq!(rig.catalog().owner_at(&c0.scope()).unwrap().map(|o| o.execution_id), Some(held.clone()), "the recorded pull keeps its owner");
    assert_eq!(rig.catalog().cursor_at(&c0.scope()).unwrap().position, None, "a stop advances no position");
    assert_eq!(backfill.next(PIPELINE, TABLE).unwrap().map(|c| c.chunk), Some("c0".to_string()));

    // The next attempt replays the recorded pull under the same execution.
    let mut x = backfill.open(&chunk(&rig, "c0"), &open(table_scope(), "run-2")).unwrap();
    assert_eq!(x.execution_id(), held);
    assert_eq!(x.step("pull-0", b"null", &mut counted(&pulls, b"page")).unwrap(), b"page");
    assert_eq!(pulls.load(Ordering::SeqCst), 1, "the pull replays instead of running again");
    assert_eq!(chunk(&rig, "c0").attempts, 2);
    drop(x);

    // c1 is stopped before its first pull records anything: the owner is released.
    let c1 = chunk(&rig, "c1");
    let x = backfill.open(&c1, &open(table_scope(), "run-3")).unwrap();
    rig.engine.cancel("run-3", Scope::Run, None).unwrap();
    eventually("the keeper fires the stopped run's token", || x.token().requested());
    x.close(Outcome::Failed(Failure::canceled("stopped before the first pull"))).unwrap();
    let stopped = chunk(&rig, "c1");
    assert_eq!((stopped.status, stopped.attempts), (ChunkStatus::Pending, 1));
    assert!(rig.catalog().owner_at(&c1.scope()).unwrap().is_none(), "a stopped pull that recorded nothing releases its owner");
    assert_eq!(rig.catalog().cursor_at(&c1.scope()).unwrap().position, None);
}

/// A finishing table releases nothing another table's unfinished execution holds, and a seeding scope carries its own
/// source identity.
// spec: run.own.scope-independence@40947285
#[test]
fn a_finishing_table_releases_no_other_scope_and_a_seed_pins_its_own_source() {
    let rig = Rig::new();
    let live = OwnerScope::table(PIPELINE, TABLE);
    let seed = OwnerScope::seed(PIPELINE, TABLE);
    let other = OwnerScope::table(PIPELINE, "orders");
    assert_ne!(seed, live);

    // The seed pulls a bulk dump under its own scope, pinning the dump's identity, while the live table runs on the
    // vendor connector; neither refuses the other's pins.
    let mut s = rig.engine.open_execution(&open_as(seed.clone(), "dump", "1.0.0", "seed-1")).unwrap();
    s.step("pull-0", b"null", &mut |_| Ok(b"archive".to_vec())).unwrap();
    let mut o = rig.engine.open_execution(&open(other.clone(), "orders-1")).unwrap();
    o.step("pull-0", b"null", &mut |_| Ok(b"page".to_vec())).unwrap();
    let mut x = rig.engine.open_execution(&open(live.clone(), "live-1")).unwrap();
    x.step("pull-0", b"null", &mut |_| Ok(b"page".to_vec())).unwrap();
    let seed_owner = rig.catalog().owner_at(&seed).unwrap().expect("the seed holds its own owner");
    let OwnerPins::Build(pins) = &seed_owner.pins else { panic!("a seed owner pins a build") };
    assert_eq!(pins.connector, connector("dump", "1.0.0"));
    let OwnerPins::Build(live_pins) = rig.catalog().owner_at(&live).unwrap().unwrap().pins else { panic!("a table owner pins a build") };
    assert_eq!(live_pins.connector, connector("vendor", "1.0.0"));

    // The live table finishes.
    x.commit(Some(json!({"at": 30}))).unwrap();
    x.close(Outcome::Success { rows: 1, bytes: 1, batches: 1 }).unwrap();
    assert!(rig.catalog().owner_at(&live).unwrap().is_none());

    // The seed and the other table keep their owners, recorded pulls and positions.
    assert_eq!(rig.catalog().owner_at(&seed).unwrap(), Some(seed_owner.clone()));
    assert_eq!(rig.engine.journal.recorded(&seed_owner.execution_id).unwrap(), 1);
    assert_eq!(rig.catalog().owner_at(&other).unwrap().map(|w| w.execution_id), Some(o.execution_id().to_string()));
    assert_eq!(rig.engine.journal.recorded(o.execution_id()).unwrap(), 1);
    assert_eq!(rig.catalog().cursor_at(&seed).unwrap().position, None, "the live commit leaves the seed's cursor alone");
    assert_eq!(rig.catalog().cursor_at(&live).unwrap().position, Some(json!({"at": 30})));
    assert_eq!(s.execution_id(), seed_owner.execution_id);
    drop((s, o));
}
