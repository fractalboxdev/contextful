//! A run's commit of several batches: a part per batch, ordinals, and the position on the marker.

use crate::support::{at, decl, s, Fixture};
use contextful_context::land::{land_batches, Batch, Position, RunContext};
use contextful_core::store::bound_time::Bounds;
use contextful_core::store::lay_out::NodeId;
use contextful_core::store::reserve::Injection;
use serde_json::json;

fn batch(rows: serde_json::Value) -> Batch {
    Batch { rows: rows.as_array().unwrap().iter().map(|r| r.as_object().unwrap().clone()).collect(), types: Default::default() }
}

#[test]
fn a_run_lands_each_batch_as_a_part_and_carries_its_position() {
    let f = Fixture::new();
    let d = decl("name = \"filings\"");
    let ctx = RunContext {
        node: NodeId::parse("ingest-a").unwrap(),
        injection: Injection { run_id: "run-b".into(), site_id: "site-a".into(), batch_seq: None, authored_by: None, taint: None },
        committed_at: at("2030-01-01T00:01:00Z"),
    };
    let position = Position { pipeline_id: Some("feed".into()), cursor: Some(json!("p3")), fence: Some(4), logged: false };
    let batches = [batch(json!([{"id": "d1"}, {"id": "d2"}])), batch(json!([])), batch(json!([{"id": "d3"}]))];
    let m = land_batches(&f.store, &d, &batches, &ctx, &position, &|| Ok(())).unwrap();
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
    let position = Position { pipeline_id: Some("feed".into()), cursor: Some(json!("p3")), fence: Some(1), logged: false };
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
    let position = Position { pipeline_id: Some("feed".into()), cursor: Some(json!(run)), fence: Some(fence), logged: true };
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
    let position = Position { pipeline_id: Some("feed".into()), cursor: Some(json!("p9")), fence: Some(7), logged: false };
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
    let position = Position { pipeline_id: Some("feed".into()), cursor: Some(json!("p1")), fence: Some(1), logged: true };
    let d = decl("name = \"filings\"");
    land_batches(&store, &d, &[batch(json!([{"id": "d1"}]))], &ctx, &position, &|| Ok(())).unwrap();
    let again = land_batches(&store, &d, &[batch(json!([{"id": "d2"}]))], &ctx, &position, &|| Ok(()));
    assert!(again.unwrap_err().to_string().contains("already committed"));
    contextful_context::commit_log::open_fence(&store, "feed", "ingest-a", "filings", 1).unwrap();
    let entry = CommitEntry { kind: Kind::Commit, table: "filings".into(), run_id: Some("run-x".into()), cursor: Some(json!("p1")), fence: 1 };
    assert_eq!(contextful_context::commit_log::append(&store, "feed", "ingest-a", &entry).unwrap(), 2);
    assert_eq!(store.committed_runs("filings").unwrap().into_iter().map(|m| m.run_id).collect::<Vec<_>>(), ["run-x"]);
}
