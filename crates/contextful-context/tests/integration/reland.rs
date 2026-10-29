//! `store.pull.reland`: a run the pointer's snapshot folds, committed again from the
//! snapshot's parts under a derived run id whose manifest names the run.

use crate::support::{at, decl, Fixture};
use contextful_context::fold::fold;
use contextful_context::reland::{reland, relanded_run_id};
use contextful_context::rows::table_rows;
use contextful_core::store::declare::TableDecl;
use contextful_core::store::lay_out::{NodeId, RunManifest};
use contextful_core::store::reconcile::ColumnType;
use contextful_core::store::StoreError;
use serde_json::json;

fn ids(f: &Fixture, d: &TableDecl) -> Vec<i64> {
    let mut ids: Vec<i64> = table_rows(&f.store, d, &["id"]).unwrap().iter().map(|r| r["id"].as_i64().unwrap()).collect();
    ids.sort_unstable();
    ids
}

/// A reland commits the run's snapshot rows, told apart from another node's run of the same id by the folded-runs
/// index, at the run's commit instant under `reland-<16 hex>`, its manifest naming the run; a repeat replays.
// spec: store.pull.reland-rows@25a8d1ae
// spec: store.pull.reland-lineage@fa435a40
#[test]
fn a_reland_commits_a_folded_runs_rows_again_under_a_derived_id() {
    let f = Fixture::new();
    let d = decl("name = \"events\"");
    let typed = [("seen", ColumnType::Timestamp)];
    let first = f.land_typed(&d, "run-1", json!([{"id": 1, "seen": "2030-01-01T00:00:00Z"}]), "2030-01-01T00:00:00Z", &typed).unwrap();
    f.land_on(&d, "run-1", "ingest-b", json!([{"id": 9, "seen": "2030-01-01T00:01:00Z"}]), "2030-01-01T00:01:00Z", &typed).unwrap();
    f.land(&d, "run-2", json!([{"id": 2}, {"id": 3}]), "2030-01-01T00:02:00Z").unwrap();
    fold(&f.store, &d, at("2030-01-01T00:10:00Z")).unwrap();
    let snapshot = f.store.current_snapshot("events").unwrap().unwrap().snapshot_id;
    let node = NodeId::parse("ingest-c").unwrap();

    let r = reland(&f.store, &d, "run-1/ingest-a", &node).unwrap();
    assert_eq!(r.run_id, relanded_run_id(&snapshot, "run-1/ingest-a"));
    assert!(r.run_id.starts_with("reland-") && r.run_id.len() == "reland-".len() + 16, "{}", r.run_id);
    assert_eq!((r.rows, r.held, r.replay), (1, Some(1), false));
    let dir = f.table_dir("events").join("data/runs").join(&r.run_id).join("ingest-c");
    let manifest: RunManifest = serde_json::from_slice(&std::fs::read(dir.join("_manifest.json")).unwrap()).unwrap();
    assert_eq!(manifest.relanded_from.as_deref(), Some("run-1/ingest-a"));
    assert_eq!(manifest.committed_at, first.committed_at);

    // The run's row reads beside the snapshot's, typed as it landed: only run-1/ingest-a's, not run-1/ingest-b's.
    let rows = table_rows(&f.store, &d, &["id", "seen", "_run_id", "_ingested_at"]).unwrap();
    let mine: Vec<_> = rows.iter().filter(|row| row["_run_id"] == json!(r.run_id)).collect();
    assert_eq!(mine.len(), 1, "{rows:?}");
    assert_eq!((mine[0]["id"].clone(), mine[0]["seen"].clone()), (json!(1), json!("2030-01-01T00:00:00.000000000Z")));
    assert_eq!(Some(mine[0]["_ingested_at"].as_str().unwrap()), Some(first.committed_at.to_rfc3339_nanos().as_str()));
    assert_eq!(ids(&f, &d), [1, 1, 2, 3, 9]);

    // A repeat finds the run committed and replays it.
    let again = reland(&f.store, &d, "run-1/ingest-a", &node).unwrap();
    assert_eq!((again.run_id, again.rows, again.replay), (r.run_id.clone(), 1, true));
    assert_eq!(ids(&f, &d), [1, 1, 2, 3, 9]);

    let two = reland(&f.store, &d, "run-2/ingest-a", &node).unwrap();
    assert_eq!((two.rows, two.held), (2, Some(2)));
    assert_ne!(two.run_id, r.run_id);
}

/// A keyed snapshot keeps one row per key, so a reland of a run a later run superseded in part commits the rows the
/// snapshot kept and reports beside them the rows the folded-runs index records the run held.
// spec: store.pull.reland-keyed@18d9a6d2
#[test]
fn a_reland_of_a_keyed_run_commits_the_rows_its_snapshot_kept() {
    let f = Fixture::new();
    let d = decl("name = \"events\"\nprimary_key = [\"id\"]");
    f.land(&d, "run-1", json!([{"id": 1, "v": "a"}, {"id": 2, "v": "a"}]), "2030-01-01T00:00:00Z").unwrap();
    f.land(&d, "run-2", json!([{"id": 2, "v": "b"}]), "2030-01-01T00:01:00Z").unwrap();
    fold(&f.store, &d, at("2030-01-01T00:10:00Z")).unwrap();
    let r = reland(&f.store, &d, "run-1/ingest-a", &NodeId::parse("ingest-c").unwrap()).unwrap();
    assert_eq!((r.rows, r.held), (1, Some(2)));
    let rows = table_rows(&f.store, &d, &["id", "v", "_run_id"]).unwrap();
    let mine: Vec<_> = rows.iter().filter(|row| row["_run_id"] == json!(r.run_id)).collect();
    assert_eq!(mine.len(), 1, "{rows:?}");
    assert_eq!((mine[0]["id"].clone(), mine[0]["v"].clone()), (json!(1), json!("a")));
}

/// Without a folded-runs row recording the run's commit, two nodes' runs of one id share their `_run_id` only, so a
/// reland refuses as the pull does and commits nothing.
// spec: store.pull.reland-unknown@7f374d55
#[test]
fn a_reland_refuses_a_run_its_snapshot_does_not_tell_apart() {
    let f = Fixture::new();
    let d = decl("name = \"events\"");
    f.land(&d, "run-1", json!([{"id": 1}]), "2030-01-01T00:00:00Z").unwrap();
    f.land_on(&d, "run-1", "ingest-b", json!([{"id": 9}]), "2030-01-01T00:01:00Z", &[]).unwrap();
    fold(&f.store, &d, at("2030-01-01T00:10:00Z")).unwrap();
    let snapshot = f.store.current_snapshot("events").unwrap().unwrap().snapshot_id;
    let path = f.table_dir("events").join(format!("data/snapshots/{snapshot}/_manifest.json"));
    let mut m: serde_json::Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    assert!(m.as_object_mut().unwrap().remove("folded_runs").is_some(), "the snapshot carries an index");
    std::fs::write(&path, serde_json::to_vec(&m).unwrap()).unwrap();
    let err = reland(&f.store, &d, "run-1/ingest-a", &NodeId::parse("ingest-c").unwrap()).unwrap_err();
    assert!(matches!(err.store(), Some(StoreError::SyncLocalFoldUnrecoverable(m)) if m.contains("run-1/ingest-b")), "{err}");
    let id = relanded_run_id(&snapshot, "run-1/ingest-a");
    assert!(!f.table_dir("events").join("data/runs").join(id).exists(), "no run is relanded");
}
