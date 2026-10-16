//! `run.seed`: a seed chunk lands consumer-held history through the one land path under its
//! ceiling, keeps its chunk and cursor state under the `<table>#seed` scope, binds the live
//! position on commit, and compares the source's fingerprint on every later run.

use crate::support::{plan, Pages, Rig, Sink};
use contextful_core::pipeline::normalize::{Mode, Normalize};
use contextful_core::pipeline::seed::{Ceiling, SeedCeiling};
use contextful_core::run::advance::CursorKind;
use contextful_core::run::backfill::{Bound, ChunkStatus, PipelinePhase, Window};
use contextful_core::run::own::{ConnectorPin, OwnerScope};
use contextful_core::run::plan::Plan;
use contextful_core::run::ports::{Row, Shape, Unshaped};
use contextful_core::run::record::RunStatus;
use contextful_core::run::RunError;
use contextful_engine::seed::{file_fingerprint, FileStat, FingerprintProbe, SeedLoad, SeedOutcome};
use contextful_engine::{EngineError, RunSpec};
use serde_json::{json, Value};
use std::cell::Cell;

const PIPELINE: &str = "feed";
const TABLE: &str = "filings";
const SEED_TABLE: &str = "filings#seed";

fn spec(plan: &Plan, run_id: &str) -> RunSpec {
    RunSpec {
        connector: ConnectorPin { id: "dump".into(), version: "1.0.0".into(), ..plan.connector_pin("artifact-dump") },
        plan: plan.clone(),
        run_id: run_id.into(),
        site_id: "site-a".into(),
        pid: 4242,
        boot_id: "boot-a".into(),
        trace_id: None,
    }
}

fn seed_plan() -> Plan {
    plan("kind = \"opaque-token\"", "")
}

fn load(below: Value, live_kind: CursorKind) -> SeedLoad {
    SeedLoad { ceiling: SeedCeiling::new("updated_at", Ceiling::from_value(&below).unwrap()), live_kind, live_field: Some("updated_at".into()), normalize: None }
}

/// A probe answering a fixed fingerprint and counting its calls.
struct Probe {
    answer: Option<&'static str>,
    calls: Cell<u32>,
}

fn probe(answer: Option<&'static str>) -> Probe {
    Probe { answer, calls: Cell::new(0) }
}

impl FingerprintProbe for Probe {
    fn fingerprint(&self) -> Option<String> {
        self.calls.set(self.calls.get() + 1);
        self.answer.map(str::to_string)
    }
}

/// Plan `n` seed chunks of width ten.
fn chunks(rig: &Rig, n: i64) {
    let windows: Vec<(String, Window)> = (0..n).map(|i| (format!("c{i}"), Window::new(Bound::Int(i * 10), Bound::Int(i * 10 + 10)).unwrap())).collect();
    rig.engine.seeding().plan(PIPELINE, TABLE, &windows).unwrap();
}

fn stamped(id: &str, at: i64) -> Value {
    json!({"id": id, "updated_at": at})
}

/// A chain that moves every stamp down by `shift` and drops rows flagged `drop`.
struct Shift(i64);

impl Shape for Shift {
    fn shape(&self, rows: Vec<Row>) -> Result<Vec<Row>, RunError> {
        Ok(rows
            .into_iter()
            .filter(|r| r.get("drop") != Some(&json!(true)))
            .map(|mut r| {
                let at = r["updated_at"].as_i64().unwrap();
                r.insert("updated_at".into(), json!(at - self.0));
                r
            })
            .collect())
    }
}

fn ran(outcome: Result<SeedOutcome, EngineError>) -> contextful_core::run::record::RunRow {
    match outcome {
        Ok(SeedOutcome::Ran(row)) => *row,
        other => panic!("expected a seed run: {other:?}"),
    }
}

/// A seeded row whose ordering stamp reaches `below` raises `PipelineSeedCeilingBreached` naming the value; its chunk
/// lands nothing, and the stamp is neither clamped nor dropped.
// spec: run.seed.ceiling-breached@0b3b936a
#[test]
fn a_stamp_reaching_the_ceiling_fails_its_chunk_and_lands_nothing() {
    let rig = Rig::new();
    chunks(&rig, 1);
    let mut sink = Sink::default();
    let mut source = Pages::new(vec![vec![stamped("a", 50)], vec![stamped("b", 120), stamped("c", 20)]]);
    let row = ran(rig.engine.seeding().run_next(&load(json!(100), CursorKind::Monotonic), &spec(&seed_plan(), "seed-1"), &mut source, &Unshaped, &mut sink, &probe(Some("f1"))));

    assert_eq!(row.status, RunStatus::Failed);
    let message = row.error_message.unwrap_or_default();
    assert!(message.contains("PipelineSeedCeilingBreached") && message.contains("`120`") && message.contains("`100`"), "{message}");
    assert!(sink.commits.is_empty(), "the breached chunk lands nothing");
    assert_eq!(sink.discarded, [(TABLE.to_string(), "seed-1".to_string())], "the earlier staged batch is discarded");
    let staged: Vec<Value> = sink.staged.iter().flat_map(|s| s.rows.iter().map(|r| r["updated_at"].clone())).collect();
    assert_eq!(staged, [json!(50)], "the breaching batch never stages, and no stamp is clamped");
    let chunk = rig.catalog().chunk_at(&OwnerScope::chunk(PIPELINE, SEED_TABLE, "c0")).unwrap().unwrap();
    assert_eq!(chunk.status, ChunkStatus::Failed);
    assert_eq!(rig.catalog().cursor(PIPELINE, TABLE).unwrap().position, None);
    assert!(rig.engine.seeding().state(PIPELINE, TABLE).unwrap().fingerprint.is_none(), "a failed chunk commits no seed");
}

/// The ceiling is evaluated on the root batch after normalize and the chain, before the write.
// spec: run.seed.ceiling-point@00cd2763
#[test]
fn the_ceiling_reads_the_batch_the_chain_answers() {
    let rig = Rig::new();
    chunks(&rig, 2);
    let below = load(json!(100), CursorKind::OpaqueToken);

    // Raw stamps past the ceiling land once the chain moves them below it or drops them.
    let mut sink = Sink::default();
    let mut source = Pages::new(vec![vec![stamped("a", 150), json!({"id": "b", "updated_at": 900, "drop": true})]]);
    let row = ran(rig.engine.seeding().run_next(&below, &spec(&seed_plan(), "seed-1"), &mut source, &Shift(100), &mut sink, &probe(None)));
    assert_eq!(row.status, RunStatus::Success, "{:?}", row.error_message);
    assert_eq!(sink.commits[0].batches, [vec![stamped("a", 50).as_object().unwrap().clone()]]);

    // A chain that lifts an in-bound raw stamp to the ceiling breaches it.
    let mut source = Pages::new(vec![vec![stamped("c", 60)]]);
    let row = ran(rig.engine.seeding().run_next(&below, &spec(&seed_plan(), "seed-2"), &mut source, &Shift(-40), &mut sink, &probe(None)));
    assert_eq!(row.status, RunStatus::Failed);
    assert!(row.error_message.unwrap_or_default().contains("PipelineSeedCeilingBreached"));
    assert_eq!(sink.commits.len(), 1);

    // Under relational normalize the ceiling reads the flattened root column, and the write
    // receives the rows as the chain answered them.
    let rig = Rig::new();
    chunks(&rig, 2);
    let nested = SeedLoad {
        ceiling: SeedCeiling::new("meta_updated_at", Ceiling::from_value(&json!(100)).unwrap()),
        live_kind: CursorKind::OpaqueToken,
        live_field: Some("meta_updated_at".into()),
        normalize: Some(Normalize { mode: Mode::Relational, ..Normalize::default() }),
    };
    let in_bound = json!({"id": "a", "meta": {"updated_at": 50}});
    let mut sink = Sink::default();
    let row = ran(rig.engine.seeding().run_next(&nested, &spec(&seed_plan(), "seed-1"), &mut Pages::new(vec![vec![in_bound.clone()]]), &Unshaped, &mut sink, &probe(None)));
    assert_eq!(row.status, RunStatus::Success, "{:?}", row.error_message);
    assert_eq!(sink.commits[0].batches, [vec![in_bound.as_object().unwrap().clone()]]);
    let breaching = json!({"id": "b", "meta": {"updated_at": 120}});
    let row = ran(rig.engine.seeding().run_next(&nested, &spec(&seed_plan(), "seed-2"), &mut Pages::new(vec![vec![breaching]]), &Unshaped, &mut sink, &probe(None)));
    assert_eq!(row.status, RunStatus::Failed);
    let message = row.error_message.unwrap_or_default();
    assert!(message.contains("PipelineSeedCeilingBreached") && message.contains("`120`"), "{message}");
    assert_eq!(sink.commits.len(), 1);
}

/// Seed chunk and cursor state live under a `<table>#seed` scope apart from the live pipeline's.
// spec: run.seed.scope@bdb535fa
#[test]
fn seed_chunks_and_positions_live_under_the_seed_scope() {
    let rig = Rig::new();
    chunks(&rig, 1);
    let mut sink = Sink::default();
    let mut source = Pages::new(vec![vec![stamped("a", 10)], vec![stamped("b", 20)]]);
    let row = ran(rig.engine.seeding().run_next(&load(json!(100), CursorKind::OpaqueToken), &spec(&seed_plan(), "seed-1"), &mut source, &Unshaped, &mut sink, &probe(Some("f1"))));

    assert_eq!(row.status, RunStatus::Success, "{:?}", row.error_message);
    assert_eq!(row.table, SEED_TABLE, "the run records the seed scope");
    assert_eq!(sink.commits[0].table, TABLE, "seeded rows land in the live table through the one land path");
    assert!(rig.catalog().chunks(PIPELINE, TABLE).unwrap().is_empty(), "the live table holds no seed chunk");
    let plan = rig.catalog().chunks(PIPELINE, SEED_TABLE).unwrap();
    assert_eq!(plan.iter().map(|c| c.status).collect::<Vec<_>>(), [ChunkStatus::Done]);
    let seed_cursor = rig.catalog().cursor_at(&OwnerScope::chunk(PIPELINE, SEED_TABLE, "c0")).unwrap();
    assert_eq!(seed_cursor.position, Some(json!("p2")), "the seed's position stays under its own scope");
    assert_eq!(sink.commits[0].cursor, None, "the commit marker carries the live position, unset");
    assert_eq!(rig.catalog().cursor(PIPELINE, TABLE).unwrap().position, None);
}

/// On seed commit, an unset `monotonic` live position takes the ceiling when `below` parses as an integer; under the
/// other cursor kinds the live position stays unset and starts from the source's beginning.
// spec: run.seed.ceiling-binding@b4fdc67e
#[test]
fn an_integer_ceiling_becomes_the_unset_live_position() {
    let rig = Rig::new();
    chunks(&rig, 2);
    let bound = load(json!(100), CursorKind::Monotonic);
    let mut sink = Sink::default();
    ran(rig.engine.seeding().run_next(&bound, &spec(&seed_plan(), "seed-1"), &mut Pages::new(vec![vec![stamped("a", 10)]]), &Unshaped, &mut sink, &probe(None)));
    assert_eq!(sink.commits[0].cursor, None, "a chunk short of the plan's end binds nothing");
    assert_eq!(rig.catalog().cursor(PIPELINE, TABLE).unwrap().position, None);
    rig.clock.advance(1);
    ran(rig.engine.seeding().run_next(&bound, &spec(&seed_plan(), "seed-2"), &mut Pages::new(vec![vec![stamped("b", 20)]]), &Unshaped, &mut sink, &probe(None)));
    let watermark = json!({"field": "updated_at", "at": 100});
    assert_eq!(sink.commits[1].cursor, Some(watermark.clone()), "the seed commit binds the ceiling");
    assert_eq!(rig.catalog().cursor(PIPELINE, TABLE).unwrap().position, Some(watermark));

    // A live position already set stays where the live pipeline left it.
    let rig = Rig::new();
    chunks(&rig, 2);
    let mut sink = Sink::default();
    ran(rig.engine.seeding().run_next(&bound, &spec(&seed_plan(), "seed-1"), &mut Pages::new(vec![vec![stamped("a", 10)]]), &Unshaped, &mut sink, &probe(None)));
    let live = json!({"field": "updated_at", "at": 140});
    sink.commits[0].cursor = Some(live.clone());
    ran(rig.engine.seeding().run_next(&bound, &spec(&seed_plan(), "seed-2"), &mut Pages::new(vec![vec![stamped("b", 20)]]), &Unshaped, &mut sink, &probe(None)));
    assert_eq!(sink.commits[1].cursor, Some(live));

    let text = json!({"id": "a", "updated_at": "2020-01-01T00:00:00Z"});
    for (below, kind, row) in [
        (json!("2029-01-01T00:00:00Z"), CursorKind::Monotonic, text),
        (json!(100), CursorKind::OpaqueToken, stamped("a", 10)),
        (json!(100), CursorKind::SnapshotId, stamped("a", 10)),
    ] {
        let rig = Rig::new();
        chunks(&rig, 1);
        let mut sink = Sink::default();
        let row = ran(rig.engine.seeding().run_next(&load(below.clone(), kind), &spec(&seed_plan(), "seed-1"), &mut Pages::new(vec![vec![row]]), &Unshaped, &mut sink, &probe(None)));
        assert_eq!(row.status, RunStatus::Success, "{:?}", row.error_message);
        assert_eq!(sink.commits[0].cursor, None, "{below} {kind:?}");
        assert_eq!(rig.catalog().cursor(PIPELINE, TABLE).unwrap().position, None);
    }
}

/// Each run probes the seed source for a cheap fingerprint, a stat or a HEAD and never a download, and compares it
/// with the one stamped at commit.
// spec: run.seed.fingerprint@8bf49f1e
#[test]
fn every_run_compares_the_probed_fingerprint_with_the_committed_one() {
    let rig = Rig::new();
    chunks(&rig, 2);
    let bound = load(json!(100), CursorKind::OpaqueToken);
    let mut sink = Sink::default();
    let answer = probe(Some("f1"));
    ran(rig.engine.seeding().run_next(&bound, &spec(&seed_plan(), "seed-1"), &mut Pages::new(vec![vec![stamped("a", 10)]]), &Unshaped, &mut sink, &answer));
    assert!(rig.engine.seeding().state(PIPELINE, TABLE).unwrap().fingerprint.is_none(), "a seed with a chunk left is uncommitted");
    ran(rig.engine.seeding().run_next(&bound, &spec(&seed_plan(), "seed-2"), &mut Pages::new(vec![vec![stamped("b", 20)]]), &Unshaped, &mut sink, &answer));
    assert_eq!(rig.engine.seeding().state(PIPELINE, TABLE).unwrap().fingerprint, Some(Some("f1".to_string())), "the last chunk stamps the fingerprint");

    let mut untouched = Pages::new(vec![vec![stamped("z", 1)]]);
    let outcome = rig.engine.seeding().run_next(&bound, &spec(&seed_plan(), "seed-3"), &mut untouched, &Unshaped, &mut sink, &answer).unwrap();
    assert_eq!(outcome, SeedOutcome::Unchanged);
    assert_eq!(answer.calls.get(), 3, "every run probes the source");
    assert!(untouched.calls().is_empty(), "an unchanged source is never pulled");
    assert!(rig.catalog().run("seed-3").unwrap().is_none());

    // A file seed probes with one stat: it reads metadata alone, and any write moves it.
    let dir = tempfile::tempdir().unwrap();
    let export = dir.path().join("items.jsonl");
    std::fs::write(&export, b"{\"id\":1}\n").unwrap();
    let stat = FileStat(export.clone());
    let rig = Rig::new();
    chunks(&rig, 1);
    let mut sink = Sink::default();
    ran(rig.engine.seeding().run_next(&bound, &spec(&seed_plan(), "seed-1"), &mut Pages::new(vec![vec![stamped("a", 10)]]), &Unshaped, &mut sink, &stat));
    let first = file_fingerprint(&export).expect("a present file answers a fingerprint");
    assert_eq!(rig.engine.seeding().state(PIPELINE, TABLE).unwrap().fingerprint, Some(Some(first.clone())));
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&export, std::fs::Permissions::from_mode(0o000)).unwrap();
        assert_eq!(stat.fingerprint().as_deref(), Some(first.as_str()), "a stat never opens the file");
        std::fs::set_permissions(&export, std::fs::Permissions::from_mode(0o644)).unwrap();
    }
    let mut untouched = Pages::new(vec![vec![stamped("z", 1)]]);
    assert_eq!(rig.engine.seeding().run_next(&bound, &spec(&seed_plan(), "seed-2"), &mut untouched, &Unshaped, &mut sink, &stat).unwrap(), SeedOutcome::Unchanged);
    std::fs::write(&export, b"{\"id\":1}\n{\"id\":2}\n").unwrap();
    match rig.engine.seeding().run_next(&bound, &spec(&seed_plan(), "seed-3"), &mut untouched, &Unshaped, &mut sink, &stat) {
        Err(EngineError::Refused(RunError::PipelineSeedSourceChanged(m))) => assert!(m.contains(&first), "{m}"),
        other => panic!("{other:?}"),
    }
    assert!(untouched.calls().is_empty(), "a probe never pulls the source");
    assert_eq!(FileStat(dir.path().join("absent.jsonl")).fingerprint(), None);
}

/// A seed whose every chunk is done but whose stamp never landed stamps on its next run
/// rather than idling.
#[test]
fn a_done_plan_missing_its_stamp_commits_on_the_next_run() {
    let rig = Rig::new();
    chunks(&rig, 1);
    let backfill = rig.engine.backfill();
    let hold = backfill.lease(PIPELINE, SEED_TABLE, "seed-0", "node-a").unwrap().expect("the chunk leases");
    assert!(backfill.commit(&hold).unwrap());
    assert!(rig.engine.seeding().state(PIPELINE, TABLE).unwrap().fingerprint.is_none());

    let bound = load(json!(100), CursorKind::OpaqueToken);
    let mut sink = Sink::default();
    let mut untouched = Pages::new(vec![vec![stamped("z", 1)]]);
    let outcome = rig.engine.seeding().run_next(&bound, &spec(&seed_plan(), "seed-1"), &mut untouched, &Unshaped, &mut sink, &probe(Some("f1"))).unwrap();
    assert_eq!(outcome, SeedOutcome::Committed);
    assert_eq!(rig.engine.seeding().state(PIPELINE, TABLE).unwrap().fingerprint, Some(Some("f1".to_string())));
    assert!(untouched.calls().is_empty() && sink.commits.is_empty());
    let outcome = rig.engine.seeding().run_next(&bound, &spec(&seed_plan(), "seed-2"), &mut untouched, &Unshaped, &mut sink, &probe(Some("f1"))).unwrap();
    assert_eq!(outcome, SeedOutcome::Unchanged);
}

/// A differing fingerprint raises `PipelineSeedSourceChanged` naming both values; an equal one skips the load, and an
/// unavailable one skips with a warning.
// spec: run.seed.source-changed@b1f518d6
#[test]
fn a_moved_fingerprint_refuses_and_an_absent_one_skips_with_a_warning() {
    let rig = Rig::new();
    chunks(&rig, 1);
    let bound = load(json!(100), CursorKind::OpaqueToken);
    let mut sink = Sink::default();
    ran(rig.engine.seeding().run_next(&bound, &spec(&seed_plan(), "seed-1"), &mut Pages::new(vec![vec![stamped("a", 10)]]), &Unshaped, &mut sink, &probe(Some("f1"))));

    let mut source = Pages::new(vec![vec![stamped("z", 1)]]);
    match rig.engine.seeding().run_next(&bound, &spec(&seed_plan(), "seed-2"), &mut source, &Unshaped, &mut sink, &probe(Some("f2"))) {
        Err(EngineError::Refused(RunError::PipelineSeedSourceChanged(m))) => assert!(m.contains("f1") && m.contains("f2") && m.contains(TABLE), "{m}"),
        other => panic!("{other:?}"),
    }
    match rig.engine.seeding().run_next(&bound, &spec(&seed_plan(), "seed-3"), &mut source, &Unshaped, &mut sink, &probe(None)) {
        Ok(SeedOutcome::Unavailable(warning)) => assert!(warning.contains(TABLE), "{warning}"),
        other => panic!("{other:?}"),
    }
    assert_eq!(rig.engine.seeding().run_next(&bound, &spec(&seed_plan(), "seed-4"), &mut source, &Unshaped, &mut sink, &probe(Some("f1"))).unwrap(), SeedOutcome::Unchanged);
    assert!(source.calls().is_empty(), "no skipped or refused run pulls the source");
    assert_eq!(sink.commits.len(), 1);
}

/// Seeded rows travel {{run.land.stage-order}} unchanged; attribution survives only in the load id and a run recorded
/// under the `seeding` phase.
// spec: run.seed.one-land-path@e297356e
#[test]
fn a_seed_chunk_lands_in_the_live_table_under_the_seeding_phase() {
    let rig = Rig::new();
    chunks(&rig, 1);
    let mut sink = Sink::default();
    let row = ran(rig.engine.seeding().run_next(&load(json!(100), CursorKind::OpaqueToken), &spec(&seed_plan(), "seed-1"), &mut Pages::new(vec![vec![stamped("a", 10)]]), &Unshaped, &mut sink, &probe(None)));
    assert_eq!(row.status, RunStatus::Success, "{:?}", row.error_message);
    assert_eq!(row.pipeline_phase, Some(PipelinePhase::Seeding));
    assert_eq!(rig.row("seed-1").pipeline_phase, Some(PipelinePhase::Seeding), "the catalog row records the phase");
    assert_eq!(serde_json::to_value(rig.row("seed-1")).unwrap()["pipeline_phase"], json!("seeding"));
    assert_eq!((sink.commits[0].table.as_str(), sink.commits[0].run_id.as_str()), (TABLE, "seed-1"), "the live table receives the rows under the seed's load id");
    assert_eq!(sink.commits[0].batches, [vec![stamped("a", 10).as_object().unwrap().clone()]], "the rows land as pulled");

    let live = rig.run(&seed_plan(), "1.0.0", "live-1", &mut Pages::new(vec![vec![stamped("b", 200)]]), &mut sink).unwrap();
    assert_eq!(live.status, RunStatus::Success, "{:?}", live.error_message);
    assert_eq!(live.pipeline_phase, None, "a live run records no seeding phase");
    assert!(serde_json::to_value(&live).unwrap().get("pipeline_phase").is_none());
}

/// A seeding run is exempt from {{run.own.pinned-plan-changed}} and records its connector identity as provenance.
// spec: run.seed.connector-pin@835c66e9
#[test]
fn a_pending_seed_owner_resumes_under_a_moved_connector_and_records_it() {
    let rig = Rig::new();
    chunks(&rig, 1);
    let bound = load(json!(100), CursorKind::OpaqueToken);
    let mut sink = Sink::default();
    let mut source = Pages::new(vec![vec![stamped("a", 10)], vec![stamped("b", 20)]]);
    source.die_after = Some(0);
    let died = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        rig.engine.seeding().run_next(&bound, &spec(&seed_plan(), "seed-1"), &mut source, &Unshaped, &mut sink, &probe(None))
    }));
    assert!(died.is_err(), "the seed run dies mid-chunk");
    let chunk = OwnerScope::chunk(PIPELINE, SEED_TABLE, "c0");
    let held = rig.catalog().owner_at(&chunk).unwrap().expect("the dead run leaves its owner pending");
    rig.clock.advance(400);

    let mut moved = spec(&seed_plan(), "seed-2");
    moved.connector.version = "2.0.0".into();
    let row = ran(rig.engine.seeding().run_next(&bound, &moved, &mut source, &Unshaped, &mut sink, &probe(None)));
    assert_eq!(row.status, RunStatus::Success, "{:?}", row.error_message);
    assert_eq!(row.execution_id, held.execution_id, "the seed resumes its pending execution");
    assert_eq!((row.connector_id.as_str(), row.connector_version.as_str()), ("dump", "2.0.0"), "the row records the build it ran");
    assert_eq!(rig.row("seed-1").connector_version, "1.0.0", "the first attempt keeps its own provenance");
    assert!(rig.catalog().owner_at(&chunk).unwrap().is_none(), "the completed chunk retires its owner");
}

/// A chunk a killed seed run left running idles while that run's owner lease holds, then settles failed and runs
/// to the seed commit once the lease lapses.
#[test]
fn a_chunk_a_dead_seed_run_left_running_is_retaken_once_its_lease_lapses() {
    let rig = Rig::new();
    chunks(&rig, 1);
    let bound = load(json!(100), CursorKind::OpaqueToken);
    let mut sink = Sink::default();
    let mut source = Pages::new(vec![vec![stamped("a", 10)], vec![stamped("b", 20)]]);
    source.die_after = Some(0);
    let died = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        rig.engine.seeding().run_next(&bound, &spec(&seed_plan(), "seed-1"), &mut source, &Unshaped, &mut sink, &probe(Some("f1")))
    }));
    assert!(died.is_err(), "the seed run dies mid-chunk");
    let chunk = OwnerScope::chunk(PIPELINE, SEED_TABLE, "c0");
    assert_eq!(rig.catalog().chunk_at(&chunk).unwrap().unwrap().status, ChunkStatus::Running, "the dead run leaves its chunk running");
    let mut idle = Pages::new(vec![vec![stamped("z", 1)]]);
    assert_eq!(rig.engine.seeding().run_next(&bound, &spec(&seed_plan(), "seed-2"), &mut idle, &Unshaped, &mut sink, &probe(Some("f1"))).unwrap(), SeedOutcome::Idle, "a held lease keeps the chunk");
    assert!(idle.calls().is_empty());

    rig.clock.advance(400);
    let row = ran(rig.engine.seeding().run_next(&bound, &spec(&seed_plan(), "seed-3"), &mut source, &Unshaped, &mut sink, &probe(Some("f1"))));
    assert_eq!(row.status, RunStatus::Success, "{:?}", row.error_message);
    assert_eq!(rig.catalog().chunk_at(&chunk).unwrap().unwrap().status, ChunkStatus::Done);
    assert!(rig.engine.seeding().state(PIPELINE, TABLE).unwrap().committed(), "the retaken chunk completes the seed");
}

/// `seed reset` clears the seed's chunk plan and commit stamp; the next run plans and loads the seed again, and rows
/// the seed landed stay.
#[test]
fn a_reset_clears_the_seed_plan_and_stamp_so_the_next_run_reloads() {
    let rig = Rig::new();
    chunks(&rig, 1);
    let bound = load(json!(100), CursorKind::OpaqueToken);
    let mut sink = Sink::default();
    ran(rig.engine.seeding().run_next(&bound, &spec(&seed_plan(), "seed-1"), &mut Pages::new(vec![vec![stamped("a", 10)]]), &Unshaped, &mut sink, &probe(Some("f1"))));
    assert!(rig.engine.seeding().state(PIPELINE, TABLE).unwrap().committed());

    let removed = rig.engine.seeding().reset(PIPELINE, TABLE).unwrap();
    assert_eq!(removed.iter().map(|c| c.chunk.as_str()).collect::<Vec<_>>(), ["c0"]);
    let state = rig.engine.seeding().state(PIPELINE, TABLE).unwrap();
    assert!(state.chunks.is_empty() && !state.committed(), "{state:?}");
    assert_eq!(rig.catalog().cursor_at(&OwnerScope::chunk(PIPELINE, SEED_TABLE, "c0")).unwrap().position, None, "the chunk scope's position goes with it");
    assert_eq!(sink.commits.len(), 1, "landed rows stay");
    assert!(rig.engine.seeding().reset(PIPELINE, TABLE).unwrap().is_empty(), "a second reset finds nothing");

    // The moved source reloads rather than refusing.
    let mut source = Pages::new(vec![vec![stamped("b", 20)]]);
    assert_eq!(rig.engine.seeding().run_next(&bound, &spec(&seed_plan(), "seed-2"), &mut source, &Unshaped, &mut sink, &probe(Some("f2"))).unwrap(), SeedOutcome::Idle, "an unplanned seed idles");
    chunks(&rig, 1);
    let row = ran(rig.engine.seeding().run_next(&bound, &spec(&seed_plan(), "seed-3"), &mut source, &Unshaped, &mut sink, &probe(Some("f2"))));
    assert_eq!(row.status, RunStatus::Success, "{:?}", row.error_message);
    assert_eq!(rig.engine.seeding().state(PIPELINE, TABLE).unwrap().fingerprint, Some(Some("f2".to_string())));
}
