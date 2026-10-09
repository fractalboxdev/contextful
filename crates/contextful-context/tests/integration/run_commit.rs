//! A run's commit of several batches: a part per batch, ordinals, and the position on the marker.

use crate::support::{at, decl, s, Fixture};
use contextful_context::land::{commit_parts, discard_staged, land_batches, stage_part, Batch, Position, RunContext, StagedPart};
use contextful_core::store::declare::TableDecl;
use contextful_core::store::bound_time::Bounds;
use contextful_core::store::lay_out::NodeId;
use contextful_core::store::reserve::Injection;
use serde_json::json;

fn batch(rows: serde_json::Value) -> Batch {
    Batch { rows: rows.as_array().unwrap().iter().map(|r| r.as_object().unwrap().clone()).collect(), types: Default::default() }
}

/// A landing table is created on first sight of its schema, {{store.reconcile.first-sight}}; each batch is written durably in its own call, optionally carrying its ordinal as the join key onto the run's request ledger.
// spec: run.land.batch-write@7a060194
#[cfg(feature = "read")]
#[test]
fn a_run_lands_each_batch_as_a_part_and_carries_its_position() {
    let f = Fixture::new();
    let d = decl("name = \"filings\"");
    let ctx = RunContext {
        node: NodeId::parse("ingest-a").unwrap(),
        injection: Injection { run_id: "run-b".into(), site_id: "site-a".into(), batch_seq: None, authored_by: None, taint: None },
        committed_at: at("2030-01-01T00:01:00Z"),
    };
    let position = Position { pipeline_id: Some("feed".into()), cursor: Some(json!("p3")), fence: Some(4), logged: false, replace_frontier: false };
    let batches = [batch(json!([{"id": "d1"}, {"id": "d2"}])), batch(json!([])), batch(json!([{"id": "d3"}]))];
    assert!(f.store.try_schema("filings").unwrap().is_none(), "no table exists before its first batch");
    let m = land_batches(&f.store, &d, &batches, &ctx, &position, &|| Ok(())).unwrap();
    assert!(f.store.try_schema("filings").unwrap().is_some_and(|s| s.get("id").is_some()), "the first batch's schema creates the table");
    assert_eq!((m.fence, m.logged), (Some(4), false));
    assert_eq!(m.parts.iter().map(|p| p.name.as_str()).collect::<Vec<_>>(), ["part-00000.parquet", "part-00001.parquet"]);
    assert_eq!((m.pipeline_id.as_deref(), m.cursor.clone()), (Some("feed"), Some(json!("p3"))));
    let raw: serde_json::Value = serde_json::from_slice(&std::fs::read(f.table_dir("filings").join("data/runs/run-b/ingest-a/_manifest.json")).unwrap()).unwrap();
    assert_eq!(raw["cursor"], "p3");
    let rows = f.query(&d, Bounds::default(), "SELECT id, _batch_seq, _row_seq FROM t ORDER BY id");
    // The empty batch keeps its ordinal: the third batch reads 2.
    assert_eq!(rows, [vec![s("d1"), s("0"), s("0")], vec![s("d2"), s("0"), s("1")], vec![s("d3"), s("2"), s("2")]]);
}

#[test]
fn a_refused_precommit_leaves_the_run_uncommitted() {
    let f = Fixture::new();
    let d = decl("name = \"filings\"");
    let ctx = RunContext {
        node: NodeId::parse("ingest-a").unwrap(),
        injection: Injection { run_id: "run-f".into(), site_id: "site-a".into(), batch_seq: None, authored_by: None, taint: None },
        committed_at: at("2030-01-01T00:01:00Z"),
    };
    let position = Position { pipeline_id: Some("feed".into()), cursor: Some(json!("p3")), fence: Some(1), logged: false, replace_frontier: false };
    let refused = land_batches(&f.store, &d, &[batch(json!([{"id": "d1"}]))], &ctx, &position, &|| {
        Err(contextful_context::ContextError::Invalid("LeaseFenced: a later holder took the lease".into()))
    });
    assert!(refused.unwrap_err().to_string().contains("LeaseFenced"));
    assert!(!f.table_dir("filings").join("data/runs/run-f/ingest-a/_manifest.json").exists());
    assert!(f.store.committed_runs("filings").unwrap().is_empty());
}

fn fenced_landing(f: &Fixture, run: &str, fence: u64) -> contextful_core::store::lay_out::RunManifest {
    let ctx = RunContext {
        node: NodeId::parse("ingest-a").unwrap(),
        injection: Injection { run_id: run.into(), site_id: "site-a".into(), batch_seq: None, authored_by: None, taint: None },
        committed_at: at("2030-01-01T00:01:00Z"),
    };
    let position = Position { pipeline_id: Some("feed".into()), cursor: Some(json!(run)), fence: Some(fence), logged: true, replace_frontier: false };
    land_batches(&f.store, &decl("name = \"filings\""), &[batch(json!([{"id": run}]))], &ctx, &position, &|| Ok(())).unwrap()
}

fn commit(f: &Fixture, run: &str, fence: u64) -> contextful_context::Result<u64> {
    use contextful_core::store::commit_log::{CommitEntry, Kind};
    let entry = CommitEntry { kind: Kind::Commit, table: "filings".into(), run_id: Some(run.into()), cursor: Some(json!(run)), fence };
    contextful_context::commit_log::append(&f.store, "feed", "ingest-a", &entry)
}

fn readable(f: &Fixture) -> Vec<String> {
    f.store.committed_runs("filings").unwrap().into_iter().map(|m| m.run_id).collect()
}

/// A commit-log create, pointer replace or catalog `UPDATE` losing its condition to a higher fence raises
/// `LeaseFenced`, and the run or snapshot it carried stays unreadable.
// spec: store.lease.stale-fence@5ca608ca
#[test]
fn a_commit_under_a_superseded_fence_loses_and_its_run_stays_unreadable() {
    let f = Fixture::new();
    contextful_context::commit_log::open_fence(&f.store, "feed", "ingest-a", "filings", 1).unwrap();
    fenced_landing(&f, "run-old", 1);
    assert!(readable(&f).is_empty(), "a fenced manifest is unreadable until the log records it");
    // A successor takes the lease: its acquisition entry carries the next fence.
    contextful_context::commit_log::open_fence(&f.store, "feed", "ingest-a", "filings", 2).unwrap();
    match commit(&f, "run-old", 1) {
        Err(contextful_context::ContextError::Store(contextful_core::store::StoreError::LeaseFenced(m))) => assert!(m.contains("fence 2"), "{m}"),
        other => panic!("{other:?}"),
    }
    let landed = readable(&f).iter().filter(|r| *r == "run-old").count();
    contextful_eval::record::emit("stale-fence-never-lands", landed as f64, 1, 0);
    assert!(readable(&f).is_empty(), "the fenced run stays unreadable");
    fenced_landing(&f, "run-new", 2);
    commit(&f, "run-new", 2).unwrap();
    assert_eq!(readable(&f), ["run-new"]);
}

/// Under a machine lease, a run commits by creating the next `cursors/<pipeline-id>/<node-id>/<seq>.json`, and an
/// acquisition creates one carrying its fence; a manifest marked `logged` is readable once that log records it.
// spec: store.lease.commit-log@6a7d76a3
#[test]
fn a_commit_created_before_the_next_acquisition_stands() {
    let f = Fixture::new();
    contextful_context::commit_log::open_fence(&f.store, "feed", "ingest-a", "filings", 1).unwrap();
    fenced_landing(&f, "run-first", 1);
    commit(&f, "run-first", 1).unwrap();
    contextful_context::commit_log::open_fence(&f.store, "feed", "ingest-a", "filings", 2).unwrap();
    assert_eq!(readable(&f), ["run-first"], "the commit point preceded the successor's acquisition");
    let names: Vec<String> = contextful_context::commit_log::read_numbered(&f.store, "feed", "ingest-a").unwrap().iter().map(|(s, e)| format!("{s}:{:?}:{}", e.kind, e.fence)).collect();
    assert_eq!(names, ["1:Acquire:1", "2:Commit:1", "3:Acquire:2"]);
    assert!(f.store.root().join("cursors/feed/ingest-a/00000000000000000002.json").exists());
    // Another table of the pipeline holds its own fences.
    let other = contextful_core::store::commit_log::CommitEntry {
        kind: contextful_core::store::commit_log::Kind::Acquire, table: "refunds".into(), run_id: None, cursor: None, fence: 9,
    };
    contextful_context::commit_log::append(&f.store, "feed", "ingest-a", &other).unwrap();
    fenced_landing(&f, "run-second", 2);
    commit(&f, "run-second", 2).unwrap();
}

#[test]
fn a_fenced_manifest_written_before_the_commit_log_stays_readable() {
    let f = Fixture::new();
    // A store written before the commit-log protocol: a fenced manifest with no mark and no `cursors/`.
    let ctx = RunContext {
        node: NodeId::parse("ingest-a").unwrap(),
        injection: Injection { run_id: "run-upgraded".into(), site_id: "site-a".into(), batch_seq: None, authored_by: None, taint: None },
        committed_at: at("2030-01-01T00:01:00Z"),
    };
    let position = Position { pipeline_id: Some("feed".into()), cursor: Some(json!("p9")), fence: Some(7), logged: false, replace_frontier: false };
    land_batches(&f.store, &decl("name = \"filings\""), &[batch(json!([{"id": "u1"}]))], &ctx, &position, &|| Ok(())).unwrap();
    let raw = std::fs::read_to_string(f.table_dir("filings").join("data/runs/run-upgraded/ingest-a/_manifest.json")).unwrap();
    assert!(!raw.contains("logged"), "an unmarked manifest carries no mark: {raw}");
    assert!(!f.store.root().join("cursors").exists());
    assert_eq!(readable(&f), ["run-upgraded"]);
}

#[test]
fn each_node_keeps_its_own_commit_log() {
    let f = Fixture::new();
    use contextful_core::store::commit_log::{CommitEntry, Kind};
    let entry = |fence| CommitEntry { kind: Kind::Acquire, table: "filings".into(), run_id: None, cursor: None, fence };
    contextful_context::commit_log::append(&f.store, "feed", "ingest-a", &entry(9)).unwrap();
    // Another node's log starts at its own first fence, unrefused by this node's higher one.
    assert_eq!(contextful_context::commit_log::append(&f.store, "feed", "ingest-b", &entry(1)).unwrap(), 1);
    assert!(f.store.root().join("cursors/feed/ingest-b/00000000000000000001.json").exists());
}

/// A store on an exFAT volume, which holds no hard links, commits a run's manifest and a
/// commit-log entry, and refuses a second commit of the same run.
#[cfg(target_os = "macos")]
#[test]
fn a_store_on_exfat_commits_a_run_and_its_log_entry() {
    use contextful_core::store::commit_log::{CommitEntry, Kind};
    let volume = contextful_fs::test_volume::ExfatVolume::mount();
    let store = contextful_context::Store::open(volume.path(), "research").unwrap();
    let ctx = RunContext {
        node: NodeId::parse("ingest-a").unwrap(),
        injection: Injection { run_id: "run-x".into(), site_id: "site-a".into(), batch_seq: None, authored_by: None, taint: None },
        committed_at: at("2030-01-01T00:01:00Z"),
    };
    let position = Position { pipeline_id: Some("feed".into()), cursor: Some(json!("p1")), fence: Some(1), logged: true, replace_frontier: false };
    let d = decl("name = \"filings\"");
    land_batches(&store, &d, &[batch(json!([{"id": "d1"}]))], &ctx, &position, &|| Ok(())).unwrap();
    let again = land_batches(&store, &d, &[batch(json!([{"id": "d2"}]))], &ctx, &position, &|| Ok(()));
    assert!(again.unwrap_err().to_string().contains("already committed"));
    contextful_context::commit_log::open_fence(&store, "feed", "ingest-a", "filings", 1).unwrap();
    let entry = CommitEntry { kind: Kind::Commit, table: "filings".into(), run_id: Some("run-x".into()), cursor: Some(json!("p1")), fence: 1 };
    assert_eq!(contextful_context::commit_log::append(&store, "feed", "ingest-a", &entry).unwrap(), 2);
    assert_eq!(store.committed_runs("filings").unwrap().into_iter().map(|m| m.run_id).collect::<Vec<_>>(), ["run-x"]);
}

fn staging(run: &str, now: &str) -> RunContext {
    RunContext {
        node: NodeId::parse("ingest-a").unwrap(),
        injection: Injection { run_id: run.into(), site_id: "site-a".into(), batch_seq: None, authored_by: None, taint: None },
        committed_at: at(now),
    }
}

fn stage(f: &Fixture, d: &TableDecl, b: &Batch, ctx: &RunContext, ordinal: u32, row_offset: u64) -> contextful_context::Result<StagedPart> {
    stage_part(&f.store, d, b, &ctx.node, &ctx.injection, ordinal, row_offset)
}

fn fed(cursor: &str) -> Position {
    Position { pipeline_id: Some("feed".into()), cursor: Some(json!(cursor)), fence: None, logged: false, replace_frontier: false }
}

/// A commit makes a run's rows visible for one table in one step; a crash before it leaves a recoverable partial run.
// spec: run.land.commit-visibility@cdc5abf5
#[cfg(feature = "read")]
#[test]
fn staged_parts_join_the_file_list_only_at_their_commit() {
    let f = Fixture::new();
    let d = decl("name = \"filings\"");
    let ctx = staging("run-s", "2030-01-01T00:01:00Z");
    let a = stage(&f, &d, &batch(json!([{"id": "d1", "n": 1}, {"id": "d2", "n": 2}])), &ctx, 0, 0).unwrap();
    let b = stage(&f, &d, &batch(json!([{"id": "d3", "n": 2.5}])), &ctx, 1, 2).unwrap();
    assert_eq!((a.rows, b.rows), (2, 1));
    assert!(a.bytes > 0 && b.bytes > 0);
    assert!(readable(&f).is_empty(), "staged parts are in flight");
    let node_dir = f.table_dir("filings").join("data/runs/run-s/ingest-a");
    assert!(!node_dir.join("part-00000.parquet").exists(), "a staged part sits apart from the parts a manifest names");
    assert!(node_dir.join("stage.staging").is_dir(), "an uncommitted run's staged parts stay on disk for recovery");

    let m = commit_parts(&f.store, &d, &[a.name, b.name], &staging("run-s", "2030-01-01T00:05:00Z"), &fed("p2"), &|| Ok(()), &|_| Ok(())).unwrap();
    assert_eq!((m.committed_at, m.cursor.clone()), (at("2030-01-01T00:05:00Z"), Some(json!("p2"))));
    assert_eq!(m.parts.iter().map(|p| p.name.as_str()).collect::<Vec<_>>(), ["part-00000.parquet", "part-00001.parquet"]);
    assert_eq!(readable(&f), ["run-s"]);
    let rows = f.query(&d, Bounds::default(), "SELECT id, CAST(n AS DOUBLE), _batch_seq, _row_seq, CAST(_ingested_at AS VARCHAR) FROM t ORDER BY id");
    let row = |id: &str, n: &str, seq: &str, r: &str| vec![s(id), s(n), s(seq), s(r), s("2030-01-01 00:05:00+00")];
    assert_eq!(rows, [row("d1", "1.0", "0", "0"), row("d2", "2.0", "0", "1"), row("d3", "2.5", "1", "2")]);
}

/// A staged part carries no `_commit_seq`: the commit assigns it under the table's commit lock and writes it into
/// every part it names, so a run committing between a stage and its commit takes the lower value.
// spec: run.own.stage-commit-seq@a75bdfd0
#[cfg(feature = "read")]
#[test]
fn a_staged_run_takes_its_commit_seq_at_its_commit() {
    let f = Fixture::new();
    let d = decl("name = \"filings\"");
    let staged = stage(&f, &d, &batch(json!([{"id": "s1"}, {"id": "s2"}])), &staging("run-s", "2030-01-01T00:01:00Z"), 0, 0).unwrap();
    let between = land_batches(&f.store, &d, &[batch(json!([{"id": "c1"}]))], &staging("run-c", "2030-01-01T00:02:00Z"), &fed("c"), &|| Ok(())).unwrap();
    let m = commit_parts(&f.store, &d, &[staged.name], &staging("run-s", "2030-01-01T00:03:00Z"), &fed("s"), &|| Ok(()), &|_| Ok(())).unwrap();
    assert_eq!((between.commit_seq, m.commit_seq), (Some(1), Some(2)));
    let rows = f.query(&d, Bounds::default(), "SELECT id, _commit_seq FROM t ORDER BY id");
    assert_eq!(rows, [vec![s("c1"), s("1")], vec![s("s1"), s("2")], vec![s("s2"), s("2")]]);
    // The commit point runs once the manifest exists, and a refusal there surfaces.
    let late = stage(&f, &d, &batch(json!([{"id": "l1"}])), &staging("run-l", "2030-01-01T00:04:00Z"), 0, 0).unwrap();
    let refused = commit_parts(&f.store, &d, &[late.name], &staging("run-l", "2030-01-01T00:05:00Z"), &fed("l"), &|| Ok(()), &|m| {
        assert_eq!(m.commit_seq, Some(3));
        Err(contextful_context::ContextError::Invalid("LeaseFenced: a later holder took the lease".into()))
    });
    assert!(refused.unwrap_err().to_string().contains("LeaseFenced"));
}

/// A commit naming a part no stage wrote refuses and leaves the run uncommitted; a commit naming none still
/// records its position.
#[test]
fn a_commit_names_only_staged_parts() {
    let f = Fixture::new();
    let d = decl("name = \"filings\"");
    let missing = commit_parts(&f.store, &d, &["stage-00000.parquet".to_string()], &staging("run-m", "2030-01-01T00:01:00Z"), &fed("p1"), &|| Ok(()), &|_| Ok(()));
    assert!(missing.unwrap_err().to_string().contains("stage-00000.parquet"));
    assert!(readable(&f).is_empty());
    assert!(f.store.try_schema("filings").unwrap().is_none(), "the refusal leaves the schema as it stood");

    let m = commit_parts(&f.store, &d, &[], &staging("run-e", "2030-01-01T00:01:00Z"), &fed("p1"), &|| Ok(()), &|_| Ok(())).unwrap();
    assert!(m.parts.is_empty());
    assert_eq!(readable(&f), ["run-e"]);
    assert_eq!(f.store.committed_runs("filings").unwrap()[0].cursor, Some(json!("p1")));
}

#[test]
fn a_relational_group_becomes_visible_only_after_its_marker() {
    use contextful_context::land::{commit_parts_group, publish_group};
    let f = Fixture::new();
    let ctx = staging("run-group", "2030-01-01T00:01:00Z");
    let root = decl("name = \"filings\"");
    let child = decl("name = \"filings_events\"");
    let a = stage(&f, &root, &batch(json!([{"id": "d1"}])), &ctx, 0, 0).unwrap();
    let b = stage(&f, &child, &batch(json!([{"parent_id": "d1", "list_index": 0}])), &ctx, 0, 0).unwrap();
    commit_parts_group(&f.store, &child, &[b.name], &ctx, &fed("p1"), "filings", &[], &|| Ok(()), &|_| Ok(())).unwrap();
    commit_parts_group(&f.store, &root, &[a.name], &ctx, &fed("p1"), "filings", &[], &|| Ok(()), &|_| Ok(())).unwrap();
    assert!(f.store.committed_runs("filings").unwrap().is_empty());
    assert!(f.store.committed_runs("filings_events").unwrap().is_empty());
    publish_group(&f.store, "filings", &["filings".into(), "filings_events".into()], &ctx).unwrap();
    assert_eq!(f.store.committed_runs("filings").unwrap().len(), 1);
    assert_eq!(f.store.committed_runs("filings_events").unwrap().len(), 1);
}

/// A stage merges its columns into the run's own staged schema and leaves `schema.json` as it stood; the commit
/// merges them into `schema.json`, so a run that never commits types no column of the table.
// spec: run.own.stage-schema@f39243cf
#[test]
fn a_staged_run_types_the_table_only_at_its_commit() {
    let f = Fixture::new();
    let d = decl("name = \"filings\"");
    let ctx = staging("run-s", "2030-01-01T00:01:00Z");
    let staged = stage(&f, &d, &batch(json!([{"id": "d1", "ts": "2030-01-01T00:00:00Z"}])), &ctx, 0, 0).unwrap();
    assert!(f.store.try_schema("filings").unwrap().is_none(), "a stage writes no schema.json");
    // A later stage of the run reconciles against its earlier ones.
    stage(&f, &d, &batch(json!([{"id": "d2", "n": 1}])), &ctx, 1, 1).unwrap();
    let clash = stage(&f, &d, &batch(json!([{"id": "d3", "n": "x"}])), &ctx, 2, 2);
    assert!(clash.unwrap_err().to_string().contains("StoreSchemaIncompatible"));

    // Another run lands `ts` as a timestamp: the uncommitted run froze no type.
    let mut typed = batch(json!([{"id": "c1", "ts": "2030-01-01T00:00:00Z"}]));
    typed.types.insert("ts".into(), contextful_core::store::reconcile::ColumnType::Timestamp);
    land_batches(&f.store, &d, &[typed], &staging("run-c", "2030-01-01T00:02:00Z"), &fed("p1"), &|| Ok(())).unwrap();

    // The staged run's commit meets the moved schema and refuses, uncommitted.
    let refused = commit_parts(&f.store, &d, &[staged.name], &staging("run-s", "2030-01-01T00:03:00Z"), &fed("p1"), &|| Ok(()), &|_| Ok(()));
    assert!(refused.unwrap_err().to_string().contains("StoreSchemaIncompatible"));
    assert_eq!(readable(&f), ["run-c"]);
}

/// A staged part carries no `_ingested_at`; the commit writes its own instant into every part it names, so of two
/// runs writing one key the later committer survives the keyed read, whichever staged first.
// spec: run.own.stage-instant@9305df8b
#[cfg(feature = "read")]
#[test]
fn the_later_committer_wins_a_key_whichever_run_staged_first() {
    let f = Fixture::new();
    let d = decl("name = \"filings\"\nprimary_key = [\"id\"]");
    let a = stage(&f, &d, &batch(json!([{"id": "k", "v": "from-a"}])), &staging("run-a", "2030-01-01T00:01:00Z"), 0, 0).unwrap();
    let staged = f.table_dir("filings").join("data/runs/run-a/ingest-a/stage.staging").join(&a.name);
    let columns = contextful_context::parquet_io::columns(&staged).unwrap();
    assert!(!columns.is_empty(), "the exclusion below ranges over no element");
    assert!(!columns.iter().any(|c| c == "_ingested_at" || c == "_commit_seq"), "{columns:?}");
    land_batches(&f.store, &d, &[batch(json!([{"id": "k", "v": "from-b"}]))], &staging("run-b", "2030-01-01T00:03:00Z"), &fed("b"), &|| Ok(())).unwrap();
    commit_parts(&f.store, &d, &[a.name], &staging("run-a", "2030-01-01T00:05:00Z"), &fed("a"), &|| Ok(()), &|_| Ok(())).unwrap();
    let rows = f.query(&d, Bounds::default(), "SELECT v, _commit_seq, CAST(_ingested_at AS VARCHAR) FROM t");
    assert_eq!(rows, [vec![s("from-a"), s("2"), s("2030-01-01 00:05:00+00")]]);
}

/// A discard removes every part a run staged; a commit that fails before its manifest exists leaves neither the
/// parts it wrote nor the staged ones.
#[test]
fn a_failed_run_leaves_no_staged_part() {
    let f = Fixture::new();
    let d = decl("name = \"filings\"");
    let node = NodeId::parse("ingest-a").unwrap();
    let ctx = staging("run-d", "2030-01-01T00:01:00Z");
    stage(&f, &d, &batch(json!([{"id": "d1"}])), &ctx, 0, 0).unwrap();
    let stage_dir = f.table_dir("filings").join("data/runs/run-d/ingest-a/stage.staging");
    assert!(stage_dir.is_dir());
    discard_staged(&f.store, "filings", &node, "run-d").unwrap();
    assert!(!stage_dir.exists(), "the discard removes the staged parts");
    discard_staged(&f.store, "filings", &node, "run-n").unwrap();

    // A precommit refusal follows the copy: the copied parts and the staged ones are gone, and nothing is readable.
    let ctx = staging("run-p", "2030-01-01T00:02:00Z");
    let one = stage(&f, &d, &batch(json!([{"id": "p1"}])), &ctx, 0, 0).unwrap();
    let two = stage(&f, &d, &batch(json!([{"id": "p2"}])), &ctx, 1, 1).unwrap();
    let refused = commit_parts(&f.store, &d, &[one.name, two.name], &ctx, &fed("p"), &|| Err(contextful_context::ContextError::Invalid("LeaseFenced".into())), &|_| Ok(()));
    assert!(refused.unwrap_err().to_string().contains("LeaseFenced"));
    let node_dir = f.table_dir("filings").join("data/runs/run-p/ingest-a");
    assert!(!node_dir.join("stage.staging").exists());
    assert!(!node_dir.join("part-00000.parquet").exists() && !node_dir.join("part-00001.parquet").exists());
    assert!(readable(&f).is_empty());
}
