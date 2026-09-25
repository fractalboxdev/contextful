//! What a push never overwrites, what a pull never regresses, and the tables a refresh reaches.

use crate::support::{at, bucket, node, Script, Scripted};
use contextful_context::fold::fold;
use contextful_core::store::declare::TableDecl;
use contextful_core::store::fold::FoldOutcome;
use contextful_core::store::object::{ObjectError, ObjectStore};
use contextful_core::store::sync::{BucketManifest, Tombstone};
use contextful_sync::{PullScope, SyncError};
use serde_json::json;
use std::sync::Arc;

const NOW: &str = "2030-01-01T01:00:00Z";

fn manifest(b: &dyn ObjectStore) -> BucketManifest {
    serde_json::from_slice(&b.get("team/manifest.json").unwrap().unwrap().0).unwrap()
}

fn schema_columns(bytes: &[u8]) -> Vec<String> {
    let schema: contextful_core::store::reconcile::Schema = serde_json::from_slice(bytes).unwrap();
    schema.columns.into_iter().map(|c| c.name).collect()
}

#[test]
fn a_push_never_uploads_a_copy_of_another_nodes_key() {
    let dir = tempfile::tempdir().unwrap();
    let b = bucket(dir.path());
    let (a, c) = (node("ingest-a", b.clone(), ""), node("ingest-b", b.clone(), ""));
    c.land("run-1", json!([{"id": 2}]), "2030-01-01T00:00:00Z");
    c.syncer.push(at(NOW)).unwrap();
    a.syncer.pull(&PullScope::default()).unwrap();
    let key = "team/research/tables/filings/data/runs/run-1/ingest-b/part-00000.parquet";
    let before = b.get(key).unwrap().unwrap().1;
    // A's copy of B's part goes stale; A's push leaves B's object and entry alone.
    std::fs::write(a.root().join("tables/filings/data/runs/run-1/ingest-b/part-00000.parquet"), b"stale").unwrap();
    let report = a.syncer.push(at(NOW)).unwrap();
    assert!(!report.uploaded.iter().any(|k| k.contains("ingest-b/")), "{:?}", report.uploaded);
    assert_eq!(b.get(key).unwrap().unwrap().1, before);
    assert_eq!(manifest(b.as_ref()).entries["research/tables/filings/data/runs/run-1/ingest-b/part-00000.parquet"].sha256, before);
}

/// A table's `schema.json` commits by merging into the bucket's copy through the one-promotion lattice and
/// replacing it on the ETag read; no copy overwrites another.
// spec: store.push.schema-cas@02f1c0e1
#[test]
fn two_nodes_landing_different_columns_both_keep_them() {
    let dir = tempfile::tempdir().unwrap();
    let b = bucket(dir.path());
    let (a, c) = (node("ingest-a", b.clone(), ""), node("ingest-b", b.clone(), ""));
    a.land("run-1", json!([{"id": 1, "title": "a"}]), "2030-01-01T00:00:00Z");
    c.land("run-1", json!([{"id": 2, "pages": 3}]), "2030-01-01T00:00:00Z");
    std::thread::scope(|s| {
        let ha = s.spawn(|| a.syncer.push(at(NOW)).unwrap());
        let hc = s.spawn(|| c.syncer.push(at(NOW)).unwrap());
        ha.join().unwrap();
        hc.join().unwrap();
    });
    let (bytes, _) = b.get("team/research/tables/filings/schema.json").unwrap().unwrap();
    let cols = schema_columns(&bytes);
    assert!(cols.contains(&"title".to_string()) && cols.contains(&"pages".to_string()), "{cols:?}");
    // The manifest lists the object the bucket holds.
    let listed = &manifest(b.as_ref()).entries["research/tables/filings/schema.json"];
    assert_eq!(listed.sha256, contextful_core::run::journal::sha256_hex(&bytes));
}

/// A pull deletes the local copy of each key a tombstone names and no entry lists.
// spec: store.pull.tombstone-applied@fc3e8655
#[test]
fn a_pull_deletes_the_copy_a_tombstone_names() {
    let dir = tempfile::tempdir().unwrap();
    let b = bucket(dir.path());
    let (a, c) = (node("ingest-a", b.clone(), ""), node("ingest-b", b.clone(), ""));
    c.land("run-1", json!([{"id": 2}]), "2030-01-01T00:00:00Z");
    c.syncer.push(at(NOW)).unwrap();
    a.syncer.pull(&PullScope::default()).unwrap();
    let local = a.root().join("tables/filings/data/runs/run-1/ingest-b/part-00000.parquet");
    assert!(local.exists());
    // B collects its run; its next push tombstones the keys.
    std::fs::remove_dir_all(c.root().join("tables/filings/data/runs/run-1")).unwrap();
    c.syncer.push(at("2030-01-01T02:00:00Z")).unwrap();
    let m = manifest(b.as_ref());
    assert_eq!(m.tombstones.get("research/tables/filings/data/runs/run-1/ingest-b/part-00000.parquet").map(|t: &Tombstone| t.owner.as_str()), Some("ingest-b"));
    let report = a.syncer.pull(&PullScope::default()).unwrap();
    assert!(!local.exists(), "the tombstoned copy is gone");
    assert!(report.removed.iter().any(|k| k.ends_with("ingest-b/part-00000.parquet")));
    // A's next push does not resurrect it.
    a.syncer.push(at("2030-01-01T03:00:00Z")).unwrap();
    assert!(!manifest(b.as_ref()).entries.contains_key("research/tables/filings/data/runs/run-1/ingest-b/part-00000.parquet"));
}

fn publish(n: &crate::support::Node, table: &str, now: &str) -> String {
    let held = n.syncer.acquire(table, at(now)).unwrap();
    let FoldOutcome::Folded { snapshot_id, .. } = fold(&n.syncer.store, &TableDecl::named(table), at(now)).unwrap() else { panic!() };
    n.syncer.push(at(now)).unwrap();
    n.syncer.publish(table, &snapshot_id, &held).unwrap();
    n.syncer.release(table).unwrap();
    snapshot_id
}

/// A pull advances a local pointer only to a bucket pointer carrying a higher fence, or the same fence and a
/// later snapshot, and verifies every advancing table before writing any pointer.
// spec: store.pull.pointer-advance@09aca335
#[test]
fn a_pull_never_regresses_a_pointer_and_writes_none_until_every_table_verifies() {
    let dir = tempfile::tempdir().unwrap();
    let b = bucket(dir.path());
    let (a, c) = (node("ingest-a", b.clone(), ""), node("ingest-b", b.clone(), ""));
    a.land("run-1", json!([{"id": 1}]), "2030-01-01T00:00:00Z");
    a.syncer.push(at(NOW)).unwrap();
    publish(&a, "filings", NOW);
    // B holds a newer local pointer than the bucket's: a pull leaves it.
    c.syncer.pull(&PullScope::default()).unwrap();
    let path = c.root().join("tables/filings/_pointer.json");
    let newer = r#"{"snapshot_id":"snapshot-09999999999999999999","fence":99}"#;
    std::fs::write(&path, newer).unwrap();
    c.syncer.pull(&PullScope::default()).unwrap();
    assert_eq!(std::fs::read_to_string(&path).unwrap(), newer);

    // Two tables advance; one's snapshot never arrives: neither pointer is written.
    a.land_into("orders", "run-1", json!([{"id": 5}]), "2030-01-01T02:00:00Z");
    a.land("run-2", json!([{"id": 6}]), "2030-01-01T02:00:00Z");
    a.syncer.push(at("2030-01-01T02:00:00Z")).unwrap();
    let orders = publish(&a, "orders", "2030-01-01T03:00:00Z");
    publish(&a, "filings", "2030-01-01T03:00:00Z");
    let missing = format!("team/research/tables/orders/data/snapshots/{orders}/part-00000.parquet");
    let holed: Arc<dyn ObjectStore> = Arc::new(Scripted {
        inner: b.clone(),
        script: Script { on_get: Some(Box::new(move |key| (key == missing).then_some(Ok(None)))), ..Script::default() },
    });
    let d = node("ingest-c", holed, "");
    assert!(matches!(d.syncer.pull(&PullScope::default()), Err(SyncError::Store(_))));
    assert!(!d.root().join("tables/filings/_pointer.json").exists(), "a verified table waits for every table");
    assert!(!d.root().join("tables/orders/_pointer.json").exists());
}

#[test]
fn a_nested_replicate_off_table_stays_off_the_replica() {
    let dir = tempfile::tempdir().unwrap();
    let b = bucket(dir.path());
    let a = node("ingest-a", b.clone(), "");
    a.land_into("pii/users", "run-1", json!([{"id": 1, "email": "x@example.com"}]), "2030-01-01T00:00:00Z");
    a.land("run-1", json!([{"id": 2}]), "2030-01-01T00:00:00Z");
    a.syncer.push(at(NOW)).unwrap();
    let r = node("replica-1", b, "\n[replica]\nof = \"team/research\"\n");
    r.syncer.pull(&PullScope { tables: vec![], replicate_off: vec!["pii/users".into()] }).unwrap();
    assert!(!r.root().join("tables/pii").exists(), "the nested sensitive table never lands");
    assert!(r.root().join("tables/filings/schema.json").exists());
    assert_eq!(contextful_sync::sync::table_of("research/tables/pii/users/data/runs/run-1/ingest-a/part-00000.parquet").as_deref(), Some("pii/users"));
    assert_eq!(contextful_sync::sync::table_of("research/tables/pii/users/schema.json").as_deref(), Some("pii/users"));
}

#[test]
fn a_single_writer_push_proceeds_past_an_inconclusive_probe() {
    let dir = tempfile::tempdir().unwrap();
    let b: Arc<dyn ObjectStore> = Arc::new(Scripted {
        inner: bucket(dir.path()),
        script: Script {
            on_put: Some(Box::new(|key, _| key.contains("cas-probe").then(|| Err(ObjectError::Unsupported("If-None-Match".into()))))),
            ..Script::default()
        },
    });
    let mut a = node("ingest-a", b, "");
    a.land("run-1", json!([{"id": 1}]), "2030-01-01T00:00:00Z");
    assert!(a.syncer.push(at(NOW)).is_err(), "a declared cas stops");
    a.syncer.config.coordination = Some("single-writer".into());
    a.syncer.push(at(NOW)).unwrap();
}

#[test]
fn two_nodes_commit_logs_sync_without_conflict() {
    let dir = tempfile::tempdir().unwrap();
    let b = bucket(dir.path());
    let (a, c) = (node("ingest-a", b.clone(), ""), node("ingest-b", b.clone(), ""));
    for n in [&a, &c] {
        contextful_context::commit_log::open_fence(&n.syncer.store, "feed", &n.syncer.node, "filings", 1).unwrap();
        n.syncer.push(at(NOW)).unwrap();
    }
    a.syncer.pull(&PullScope::default()).unwrap();
    c.syncer.pull(&PullScope::default()).unwrap();
    a.syncer.push(at(NOW)).unwrap();
    assert!(a.root().join("cursors/feed/ingest-b/00000000000000000001.json").exists());
    assert!(c.root().join("cursors/feed/ingest-a/00000000000000000001.json").exists());
}
