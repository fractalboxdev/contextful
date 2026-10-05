//! `store.push.run-state` and the pull side of a run state: the run history and cursor markers
//! travelling with the store.

use crate::support::{at, bucket, node, node_in, Node};
use contextful_core::run::record::RunStatus;
use contextful_core::run::journal::sha256_hex;
use contextful_core::store::object::ObjectStore;
use contextful_core::store::sync::BucketManifest;
use contextful_sync::run_state;
use contextful_sync::{CursorMark, PullScope, RunMark, RunState};
use serde_json::json;

const NOW: &str = "2030-01-01T01:00:00Z";

fn manifest(b: &dyn ObjectStore) -> BucketManifest {
    serde_json::from_slice(&b.get("team/manifest.json").unwrap().unwrap().0).unwrap()
}

fn state(n: &Node, run: &str, started: &str, cursor: Option<(i64, &str)>) -> RunState {
    let mut runs = std::collections::BTreeMap::new();
    runs.insert("shop".to_string(), RunMark { run_id: run.into(), table: "shop_orders".into(), status: RunStatus::Success, started_at: at(started), ended_at: None });
    RunState {
        format: run_state::RUN_STATE_FORMAT,
        node_id: n.syncer.node.clone(),
        runs,
        cursors: cursor
            .map(|(v, committed)| CursorMark {
                pipeline_id: "shop".into(),
                table: "shop_orders".into(),
                position: Some(json!({ "field": "at", "at": v })),
                run_id: run.into(),
                committed_at: at(committed),
            })
            .into_iter()
            .collect(),
    }
}

/// The newest commit marker for a pipeline's table is the latest-committed one any node's run state records.
#[test]
fn the_newest_cursor_is_the_latest_committed_across_every_node() {
    let dir = tempfile::tempdir().unwrap();
    let b = bucket(dir.path());
    let (a, z, c) = (node("ingest-a", b.clone(), ""), node("ingest-b", b.clone(), ""), node("ingest-c", b, ""));
    run_state::record(&a.syncer.store, &state(&a, "f1", "2030-01-01T00:00:00Z", Some((9, "2030-01-01T00:01:00Z")))).unwrap();
    run_state::record(&z.syncer.store, &state(&z, "f2", "2030-01-01T00:05:00Z", Some((12, "2030-01-01T00:06:00Z")))).unwrap();
    a.syncer.push(at(NOW)).unwrap();
    z.syncer.push(at(NOW)).unwrap();
    c.syncer.pull(&PullScope::default()).unwrap();
    assert!(c.root().join("nodes/ingest-a/run-state.json").exists() && c.root().join("nodes/ingest-b/run-state.json").exists());
    let cursor = run_state::newest_cursor(&c.syncer.store, "shop", "shop_orders").unwrap().unwrap();
    assert_eq!((cursor.run_id.as_str(), cursor.position), ("f2", Some(json!({ "field": "at", "at": 12 }))));
    assert!(run_state::newest_cursor(&c.syncer.store, "shop", "other").unwrap().is_none());
}

/// A run state carries `format`, `1` for this layout; one whose `format` exceeds 1 contributes no run or cursor
/// to a reader.
// spec: store.push.run-state-format@f3db4597
#[test]
fn a_run_state_of_a_newer_format_contributes_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let b = bucket(dir.path());
    let (a, c) = (node("ingest-a", b.clone(), ""), node("ingest-c", b, ""));
    let mut newer = serde_json::to_value(state(&a, "f1", "2030-01-01T00:00:00Z", Some((9, "2030-01-01T00:01:00Z")))).unwrap();
    assert_eq!(newer["format"], 1);
    newer["format"] = json!(2);
    let path = run_state::state_path(a.syncer.store.root(), "ingest-a");
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, serde_json::to_vec(&newer).unwrap()).unwrap();
    a.syncer.push(at(NOW)).unwrap();
    c.syncer.pull(&PullScope::default()).unwrap();
    assert!(c.root().join("nodes/ingest-a/run-state.json").exists(), "the file travels");
    assert!(run_state::run_states(&c.syncer.store).unwrap().is_empty());
    assert!(run_state::newest_cursor(&c.syncer.store, "shop", "shop_orders").unwrap().is_none());
}

/// A pulled run state exposes the applied control version for a replica to verify before it adopts any snapshot.
#[test]
fn a_pulled_run_state_carries_the_applied_control_version() {
    let dir = tempfile::tempdir().unwrap();
    let b = bucket(dir.path());
    let (a, c) = (node("ingest-a", b.clone(), ""), node("ingest-c", b, ""));
    let path = run_state::state_path(a.syncer.store.root(), "ingest-a");
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(
        &path,
        serde_json::to_vec(&serde_json::json!({
            "format": 1,
            "node_id": "ingest-a",
            "runs": {},
            "cursors": [],
            "control_version": 7
        }))
        .unwrap(),
    )
    .unwrap();
    a.syncer.push(at(NOW)).unwrap();
    c.syncer.pull(&PullScope::default()).unwrap();

    let states = run_state::run_states(&c.syncer.store).unwrap();
    assert_eq!(states["ingest-a"].control_version, Some(7));
}

/// A generation pull holds no run state to `store.pull.generation-diverged`: a run state the generation does not
/// list stays as it is. A project name of several segments reads its run states alike.
// spec: store.pull.generation-run-state@ba323cb2
#[test]
fn a_restore_keeps_a_run_state_its_generation_does_not_list() {
    for project in ["research", "acme/research"] {
        let dir = tempfile::tempdir().unwrap();
        let a = node_in(project, "ingest-a", bucket(dir.path()), "");
        a.land("run-1", json!([{"id": 1}]), "2030-01-01T00:00:00Z");
        a.syncer.push(at(NOW)).unwrap();
        let path = run_state::record(&a.syncer.store, &state(&a, "f1", "2030-01-01T00:00:00Z", None)).unwrap();
        a.syncer.pull(&PullScope { generation: Some(1), ..PullScope::default() }).unwrap_or_else(|e| panic!("{project}: {e}"));
        assert!(path.exists(), "{project}");
    }
}

/// A generation pull exempts only `nodes/<node-id>/run-state.json`: any other file under `nodes/` the generation
/// does not list raises `SyncGenerationDiverged`.
#[test]
fn a_restore_refuses_another_file_under_nodes() {
    for stray in ["nodes/ingest-e/other.json", "nodes/x.json", "nodes/ingest-e/deeper/run-state.json"] {
        let dir = tempfile::tempdir().unwrap();
        let a = node("ingest-a", bucket(dir.path()), "");
        a.land("run-1", json!([{"id": 1}]), "2030-01-01T00:00:00Z");
        a.syncer.push(at(NOW)).unwrap();
        let path = a.syncer.store.root().join(stray);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, b"{}").unwrap();
        let err = a.syncer.pull(&PullScope { generation: Some(1), ..PullScope::default() }).unwrap_err();
        assert!(matches!(err, contextful_sync::SyncError::Store(contextful_core::store::StoreError::SyncGenerationDiverged(_))), "{stray}: {err}");
    }
}

/// A node holding a pulled copy of another node's run state never uploads it: the key is that node's own, in a
/// project of several segments as in one.
#[test]
fn a_pulled_run_state_stays_its_nodes_own() {
    for project in ["research", "acme/research"] {
        let dir = tempfile::tempdir().unwrap();
        let b = bucket(dir.path());
        let (a, c) = (node_in(project, "ingest-a", b.clone(), ""), node_in(project, "ingest-c", b.clone(), ""));
        run_state::record(&a.syncer.store, &state(&a, "f1", "2030-01-01T00:00:00Z", None)).unwrap();
        a.syncer.push(at(NOW)).unwrap();
        c.syncer.pull(&PullScope::default()).unwrap();
        let key = format!("{project}/nodes/ingest-a/run-state.json");
        assert_eq!(manifest(b.as_ref()).entries[&key].owner, "ingest-a", "{project}");

        // A newer state from its owner wins over the older copy the other node still holds.
        run_state::record(&a.syncer.store, &state(&a, "f2", "2030-01-01T00:05:00Z", None)).unwrap();
        a.syncer.push(at("2030-01-01T02:00:00Z")).unwrap();
        c.syncer.push(at("2030-01-01T02:01:00Z")).unwrap();
        let held = std::fs::read(a.syncer.store.root().join("nodes/ingest-a/run-state.json")).unwrap();
        let committed = manifest(b.as_ref()).entries[&key].sha256.clone();
        assert_eq!(committed, sha256_hex(&held), "{project}");
    }
}
