//! `store.pull`, `store.lease` and `store.replicate` through the syncer.

use crate::support::{at, bucket, node, Script, Scripted};
use contextful_context::fold::fold;
use contextful_core::store::declare::TableDecl;
use contextful_core::store::fold::FoldOutcome;
use contextful_core::store::object::ObjectStore;
use contextful_core::store::StoreError;
use contextful_sync::{PullScope, SyncError};
use serde_json::json;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

const NOW: &str = "2030-01-01T01:00:00Z";
const PART_A: &str = "team/research/tables/filings/data/runs/run-1/ingest-a/part-00000.parquet";

fn files(n: &crate::support::Node) -> Vec<String> {
    let decl = TableDecl::named("filings");
    let s = contextful_context::scan::scan(&n.syncer.store, &decl, Default::default()).unwrap();
    s.files
}

/// A node pushing one run, a second node over the same bucket, and the bucket.
fn pushed() -> (tempfile::TempDir, Arc<dyn ObjectStore>, crate::support::Node) {
    let dir = tempfile::tempdir().unwrap();
    let b = bucket(dir.path());
    let a = node("ingest-a", b.clone(), "");
    a.land("run-1", json!([{"id": 1, "title": "a"}]), "2030-01-01T00:00:00Z");
    a.syncer.push(at(NOW)).unwrap();
    (dir, b, a)
}

/// A downloaded object whose digest differs from its entry raises `SyncObjectDigestMismatch` and is discarded.
// spec: store.pull.digest-mismatch@5acf85aa
#[test]
fn an_object_whose_digest_differs_from_its_entry_is_refused_and_discarded() {
    let (dir, b, _a) = pushed();
    std::fs::write(dir.path().join("context-team").join(PART_A), b"tampered").unwrap();
    let c = node("ingest-b", b, "");
    match c.syncer.pull(&PullScope::default()) {
        Err(SyncError::Store(StoreError::SyncObjectDigestMismatch(m))) => assert!(m.contains("ingest-a/part-00000.parquet"), "{m}"),
        other => panic!("{other:?}"),
    }
    assert!(!c.root().join("tables/filings/data/runs/run-1/ingest-a/part-00000.parquet").exists());
}

/// When a named key disappears mid-download, the pull re-fetches the manifest and retries the shortfall, up to 3
/// attempts.
// spec: store.pull.convergence@554ef6af
#[test]
fn a_key_moving_mid_download_refetches_the_manifest_and_retries_the_shortfall() {
    assert_eq!(contextful_core::store::sync::PULL_CONVERGENCE, 3);
    let (_dir, b, _a) = pushed();
    let misses = Arc::new(AtomicUsize::new(0));
    let m = misses.clone();
    let moving: Arc<dyn ObjectStore> = Arc::new(Scripted {
        inner: b,
        script: Script {
            on_get: Some(Box::new(move |key| (key == PART_A && m.fetch_add(1, Ordering::SeqCst) < 2).then_some(Ok(None)))),
            ..Script::default()
        },
    });
    let c = node("ingest-b", moving, "");
    let report = c.syncer.pull(&PullScope::default()).unwrap();
    assert_eq!(report.attempts, 3, "the part came home on the third manifest fetch");
    assert_eq!(files(&c), ["tables/filings/data/runs/run-1/ingest-a/part-00000.parquet"]);
}

/// Exhausting those retries raises `SyncPullDidNotConverge`, naming the key that kept moving, and writes no
/// pointer.
// spec: store.pull.unconverged@9f110db1
#[test]
fn a_key_that_keeps_moving_refuses_and_writes_no_pointer() {
    let (_dir, b, a) = pushed();
    // A snapshot and its pointer exist in the bucket.
    let held = a.syncer.acquire("filings", at(NOW)).unwrap();
    let FoldOutcome::Folded { snapshot_id, .. } = fold(&a.syncer.store, &TableDecl::named("filings"), at(NOW)).unwrap() else { panic!() };
    a.syncer.push(at(NOW)).unwrap();
    a.syncer.publish("filings", &snapshot_id, &held).unwrap();
    let gone: Arc<dyn ObjectStore> =
        Arc::new(Scripted { inner: b, script: Script { on_get: Some(Box::new(|key| (key == PART_A).then_some(Ok(None)))), ..Script::default() } });
    let c = node("ingest-b", gone, "");
    match c.syncer.pull(&PullScope::default()) {
        Err(SyncError::Store(StoreError::SyncPullDidNotConverge(m))) => assert!(m.contains("ingest-a/part-00000.parquet"), "{m}"),
        other => panic!("{other:?}"),
    }
    assert!(!c.root().join("tables/filings/_pointer.json").exists());
}

/// A pull writes a table's pointer only after every Parquet part of the snapshot it names is home, so no reader
/// meets a pointer ahead of its data.
// spec: store.pull.pointer-last@d68b649f
#[test]
fn a_pointer_is_written_only_once_its_snapshot_is_home() {
    let (_dir, b, a) = pushed();
    let held = a.syncer.acquire("filings", at(NOW)).unwrap();
    let FoldOutcome::Folded { snapshot_id, .. } = fold(&a.syncer.store, &TableDecl::named("filings"), at(NOW)).unwrap() else { panic!() };
    a.syncer.push(at(NOW)).unwrap();
    a.syncer.publish("filings", &snapshot_id, &held).unwrap();
    // Record the order the pull reads objects in.
    let part = format!("team/research/tables/filings/data/snapshots/{snapshot_id}/part-00000.parquet");
    let order = Arc::new(std::sync::Mutex::new(Vec::new()));
    let o = order.clone();
    let recording: Arc<dyn ObjectStore> = Arc::new(Scripted {
        inner: b.clone(),
        script: Script {
            on_get: Some(Box::new(move |key| {
                o.lock().unwrap().push(key.to_string());
                None
            })),
            ..Script::default()
        },
    });
    let c = node("ingest-b", recording, "");
    c.syncer.pull(&PullScope::default()).unwrap();
    let order = order.lock().unwrap();
    let part_at = order.iter().position(|k| *k == part).unwrap();
    let pointer_at = order.iter().position(|k| k.ends_with("_pointer.json")).unwrap();
    assert!(part_at < pointer_at, "the part lands before the pointer is read and written");
    assert_eq!(files(&c), [format!("tables/filings/data/snapshots/{snapshot_id}/part-00000.parquet")]);
}

/// A pulled `schema.json` differing from the local copy merges into it column by column through
/// {{store.reconcile.incompatible}}'s lattice rather than replacing it.
// spec: store.pull.schema-merge@36368f45
#[test]
fn a_pulled_schema_merges_into_the_local_one() {
    let (_dir, b, _a) = pushed();
    let c = node("ingest-b", b, "");
    c.land("run-9", json!([{"id": 2, "pages": 12}]), "2030-01-01T00:00:00Z");
    c.syncer.pull(&PullScope::default()).unwrap();
    let schema: serde_json::Value = serde_json::from_slice(&std::fs::read(c.root().join("tables/filings/schema.json")).unwrap()).unwrap();
    let text = schema.to_string();
    assert!(text.contains("\"pages\"") && text.contains("\"title\""), "{text}");
}

/// Taking a table's compaction lease raises the fence stored in its bucket pointer, so a publish carrying a lower
/// fence loses its condition.
// spec: store.lease.pointer-fence@037c9e49
#[test]
fn a_publish_under_a_superseded_fence_loses_its_condition() {
    let (_dir, b, a) = pushed();
    let c = node("ingest-b", b.clone(), "");
    let old = a.syncer.acquire("filings", at("2030-01-01T02:00:00Z")).unwrap();
    // The grant lapses past the skew bound, and the second node takes the next fence.
    let new = c.syncer.acquire("filings", at("2030-01-01T02:11:00Z")).unwrap();
    assert_eq!(new.lease.fence, old.lease.fence + 1);
    let pointer: serde_json::Value = serde_json::from_slice(&b.get("team/research/tables/filings/_pointer.json").unwrap().unwrap().0).unwrap();
    assert_eq!(pointer["fence"], new.lease.fence, "acquisition raised the pointer's fence");
    match a.syncer.publish("filings", "snapshot-01893459780000000000", &old) {
        Err(SyncError::Store(StoreError::LeaseFenced(m))) => assert!(m.contains("unreadable"), "{m}"),
        other => panic!("{other:?}"),
    }
    c.syncer.publish("filings", "snapshot-01893459780000000000", &new).unwrap();
}

/// A write verb against a replica raises `ReplicaWriteRefused`, naming the canonical store.
// spec: store.replicate.write-refused@8e2b8fb9
#[test]
fn a_replica_refuses_a_write_verb_naming_the_canonical_store() {
    let (_dir, b, _a) = pushed();
    let r = node("replica-1", b, "\n[replica]\nof = \"team/research\"\n");
    let e = contextful_context::land::land(
        &r.syncer.store,
        &TableDecl::named("filings"),
        &contextful_context::land::Batch { rows: vec![json!({"id": 1}).as_object().unwrap().clone()], types: Default::default() },
        &contextful_context::land::RunContext {
            node: contextful_core::store::lay_out::NodeId::parse("replica-1").unwrap(),
            injection: contextful_core::store::reserve::Injection { run_id: "run-x".into(), site_id: "s".into(), batch_seq: None, authored_by: None },
            committed_at: at(NOW),
        },
    )
    .unwrap_err();
    assert!(e.to_string().starts_with("ReplicaWriteRefused") && e.to_string().contains("team/research"), "{e}");
    let f = fold(&r.syncer.store, &TableDecl::named("filings"), at(NOW)).unwrap_err();
    assert!(f.to_string().starts_with("ReplicaWriteRefused"), "{f}");
    // Reading is what a replica does.
    r.syncer.pull(&PullScope::default()).unwrap();
    assert_eq!(files(&r).len(), 1);
}

/// A replica holding a strict subset of a snapshot's Parquet raises `ReplicaPartialParquet` at refresh and leaves
/// that snapshot unpublished.
// spec: store.replicate.partial-parquet@8dc55930
#[test]
fn a_replica_missing_a_snapshot_part_refuses_and_leaves_it_unpublished() {
    let (_dir, b, a) = pushed();
    let held = a.syncer.acquire("filings", at(NOW)).unwrap();
    let FoldOutcome::Folded { snapshot_id, .. } = fold(&a.syncer.store, &TableDecl::named("filings"), at(NOW)).unwrap() else { panic!() };
    a.syncer.push(at(NOW)).unwrap();
    a.syncer.publish("filings", &snapshot_id, &held).unwrap();
    let part = format!("team/research/tables/filings/data/snapshots/{snapshot_id}/part-00000.parquet");
    // The bucket's manifest omits the part: a subset of the snapshot reaches the replica.
    let mut m: contextful_core::store::sync::BucketManifest = serde_json::from_slice(&b.get("team/manifest.json").unwrap().unwrap().0).unwrap();
    m.entries.remove(part.trim_start_matches("team/"));
    b.put("team/manifest.json", &serde_json::to_vec(&m).unwrap(), contextful_core::store::object::Condition::None).unwrap();
    let r = node("replica-1", b, "\n[replica]\nof = \"team/research\"\n");
    match r.syncer.pull(&PullScope::default()) {
        Err(SyncError::Store(StoreError::ReplicaPartialParquet(msg))) => assert!(msg.contains(&snapshot_id), "{msg}"),
        other => panic!("{other:?}"),
    }
    assert!(!r.root().join("tables/filings/_pointer.json").exists());
}

/// A refresh requesting a replicate-off table raises `ReplicaSensitiveTable`; the consumer reads through the
/// proxying face.
// spec: store.replicate.sensitive-refused@e105ab21
#[test]
fn a_refresh_requesting_a_replicate_off_table_refuses() {
    let (_dir, b, _a) = pushed();
    let r = node("replica-1", b, "\n[replica]\nof = \"team/research\"\n");
    let scope = PullScope { tables: vec!["filings".into()], replicate_off: vec!["filings".into()] };
    match r.syncer.pull(&scope) {
        Err(SyncError::Store(StoreError::ReplicaSensitiveTable(m))) => assert!(m.contains("filings") && m.contains("proxying face"), "{m}"),
        other => panic!("{other:?}"),
    }
    // An unscoped refresh takes every other table and leaves the replicate-off one behind.
    r.syncer.pull(&PullScope { tables: vec![], replicate_off: vec!["filings".into()] }).unwrap();
    assert!(!r.root().join("tables/filings").exists());
}
