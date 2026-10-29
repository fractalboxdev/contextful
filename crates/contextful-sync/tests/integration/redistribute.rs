//! `authority.bound-redistribution` through the syncer: a withheld table's objects stay off
//! the bucket, a root-level object ships only from the allowlist, and an earlier push's
//! objects for a now-withheld table refuse the push.

use crate::support::{at, bucket, node, Node, Script, Scripted};
use contextful_core::store::object::{Condition, ObjectStore};
use contextful_core::store::sync::BucketManifest;
use serde_json::json;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

const NOW: &str = "2030-01-01T01:00:00Z";
const LANDED: &str = "2030-01-01T00:00:00Z";

fn manifest(b: &dyn ObjectStore) -> BucketManifest {
    serde_json::from_slice(&b.get("team/manifest.json").unwrap().unwrap().0).unwrap()
}

fn withhold(n: &mut Node, table: &str) {
    n.syncer.redistribution.withheld.insert(table.into());
}

fn write(n: &Node, rel: &str, bytes: &[u8]) {
    let path = n.root().join(rel);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, bytes).unwrap();
}

/// A withheld table's run parts and verbatim pull records stay off the bucket and out of the
/// manifest, its pointer and its plan, while every other table ships. The push reads the bound
/// from the declaration alone and admits no credential, so no grant widens it.
// spec: authority.bound-redistribution.withhold@cc12f306
// spec: authority.bound-redistribution.before-diff@ad1bc580
// spec: authority.bound-redistribution.envelopes@799ba98b
// spec: authority.bound-redistribution.outranks-grants@80c7ec03
#[test]
fn a_withheld_table_ships_no_object_and_no_manifest_name() {
    let dir = tempfile::tempdir().unwrap();
    let b = bucket(dir.path());
    let mut a = node("ingest-a", b.clone(), "");
    a.land_into("filings", "run-1", json!([{"id": 1}]), LANDED);
    a.land_into("public", "run-1", json!([{"id": 2}]), LANDED);
    write(&a, "tables/filings/requests/run-1.ingest-a.parquet", b"verbatim pull record");
    withhold(&mut a, "filings");

    let plan = a.syncer.plan_manifest().unwrap().manifest();
    assert!(!plan.entries.is_empty());
    assert!(plan.entries.keys().any(|k| k.starts_with("research/tables/public/")), "{:?}", plan.entries.keys());
    assert!(!plan.entries.keys().any(|k| k.contains("tables/filings")), "{:?}", plan.entries.keys());

    let report = a.syncer.push(at(NOW)).unwrap();
    assert!(report.uploaded.iter().any(|k| k.starts_with("research/tables/public/")), "{:?}", report.uploaded);
    assert!(!report.uploaded.iter().any(|k| k.contains("tables/filings")), "{:?}", report.uploaded);
    assert_eq!(b.list("team/research/tables/filings").unwrap(), Vec::<String>::new());
    let committed = manifest(b.as_ref());
    let named = serde_json::to_string(&committed).unwrap();
    assert!(named.contains("tables/public/"), "{named}");
    assert!(!named.contains("filings"), "{named}");

    // A table whose name extends the withheld one's still ships.
    a.land_into("filings_public", "run-2", json!([{"id": 3}]), LANDED);
    a.syncer.push(at(NOW)).unwrap();
    assert!(!b.list("team/research/tables/filings_public/").unwrap().is_empty());
}

/// While a table is withheld, a root-level object neither list in Shapes names refuses the push
/// before any upload; the mirrored permission tables, the stale-memory index, the node run
/// state and the control chain ship, and the verbatim pull records and catalogs are withheld.
// spec: authority.bound-redistribution.root-allowlist@9a218c3c
// spec: authority.bound-redistribution.root-not-allowlisted@1f7d0d39
#[test]
fn a_root_object_ships_beside_a_withheld_table_only_from_the_allowlist() {
    let dir = tempfile::tempdir().unwrap();
    let b = bucket(dir.path());
    let mut a = node("ingest-a", b.clone(), "");
    a.land_into("filings", "run-1", json!([{"id": 1}]), LANDED);
    a.land_into("public", "run-1", json!([{"id": 2}]), LANDED);
    write(&a, "access/grants.parquet", b"mirrored permissions");
    write(&a, "stale_memory.idx", b"stale-memory index");
    write(&a, "nodes/ingest-a/run-state.json", br#"{"node_id":"ingest-a"}"#);
    write(&a, "records/run-1.json", b"a verbatim record");
    write(&a, "catalog.db", b"catalog");
    write(&a, "memory.db", b"memory");
    write(&a, "notes/run-1.txt", b"an unlisted root object");
    withhold(&mut a, "filings");

    let e = a.syncer.push(at(NOW)).unwrap_err().to_string();
    assert!(e.starts_with("EnforceRootObjectNotAllowlisted"), "{e}");
    assert!(e.contains("notes/run-1.txt"), "{e}");
    for shipped in ["research/access/grants.parquet", "research/stale_memory.idx", "research/nodes/", "research/records/", "research/catalog.db", "research/memory.db"] {
        assert!(!e.contains(shipped), "{shipped} refused: {e}");
    }
    assert_eq!(b.list("team/research/").unwrap(), Vec::<String>::new(), "a refused push uploads nothing");

    std::fs::remove_dir_all(a.root().join("notes")).unwrap();
    let plan = a.syncer.plan_manifest().unwrap().manifest();
    a.syncer.push(at(NOW)).unwrap();
    let entries = manifest(b.as_ref()).entries;
    for shipped in ["research/access/grants.parquet", "research/stale_memory.idx", "research/nodes/ingest-a/run-state.json"] {
        assert!(entries.contains_key(shipped), "{shipped}: {:?}", entries.keys());
    }
    let listed = b.list("team/research/").unwrap();
    for withheld in ["research/records/run-1.json", "research/catalog.db", "research/memory.db"] {
        assert!(!plan.entries.contains_key(withheld), "{withheld} planned: {:?}", plan.entries.keys());
        assert!(!entries.contains_key(withheld), "{withheld} committed: {:?}", entries.keys());
        assert!(!listed.contains(&format!("team/{withheld}")), "{withheld} uploaded: {listed:?}");
    }

    // With no table withheld, a root-level object ships unlisted.
    let other = tempfile::tempdir().unwrap();
    let open = node("ingest-b", bucket(other.path()), "");
    write(&open, "records/run-1.json", b"a verbatim record");
    assert!(open.syncer.push(at(NOW)).unwrap().uploaded.contains(&"research/records/run-1.json".to_string()));
}

/// Deleting a withheld table's objects from the bucket clears the refusal: the next push
/// commits a manifest naming the table in no entry, tombstone or pointer, though the
/// manifest it read named them.
#[test]
fn deleting_the_left_behind_objects_clears_the_refusal() {
    let dir = tempfile::tempdir().unwrap();
    let b = bucket(dir.path());
    let mut a = node("ingest-a", b.clone(), "");
    a.land_into("filings", "run-1", json!([{"id": 1}]), LANDED);
    a.land_into("public", "run-1", json!([{"id": 2}]), LANDED);
    a.syncer.push(at(NOW)).unwrap();
    assert!(serde_json::to_string(&manifest(b.as_ref())).unwrap().contains("tables/filings/"));

    withhold(&mut a, "filings");
    let e = a.syncer.push(at(NOW)).unwrap_err().to_string();
    assert!(e.starts_with("EnforceStaleRedistributedObjects"), "{e}");
    for key in b.list("team/research/tables/filings/").unwrap() {
        b.delete(&key).unwrap();
    }

    a.syncer.push(at(NOW)).unwrap();
    let committed = manifest(b.as_ref());
    let named = serde_json::to_string(&committed).unwrap();
    assert!(named.contains("tables/public/"), "{named}");
    assert!(!named.contains("filings"), "{named}");
    assert_eq!(b.list("team/research/tables/filings/").unwrap(), Vec::<String>::new());
    a.syncer.push(at(NOW)).unwrap();
    assert!(!serde_json::to_string(&manifest(b.as_ref())).unwrap().contains("filings"));
}

/// Clearing the flag over a table an earlier push uploaded refuses the next push, naming each
/// object and the remedy; every object stays in the bucket and the manifest is not rewritten.
// spec: authority.bound-redistribution.left-behind@4e51550f
#[test]
fn clearing_the_flag_after_a_push_refuses_and_deletes_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let b = bucket(dir.path());
    let mut a = node("ingest-a", b.clone(), "");
    a.land_into("filings", "run-1", json!([{"id": 1}]), LANDED);
    a.syncer.push(at(NOW)).unwrap();
    let before = b.list("team/").unwrap();
    let pushed = b.list("team/research/tables/filings/").unwrap();
    assert!(!pushed.is_empty());
    let generation = manifest(b.as_ref()).generation;

    a.land_into("public", "run-2", json!([{"id": 2}]), LANDED);
    withhold(&mut a, "filings");
    let e = a.syncer.push(at(NOW)).unwrap_err().to_string();
    assert!(e.starts_with("EnforceStaleRedistributedObjects"), "{e}");
    for key in &pushed {
        assert!(e.contains(key.strip_prefix("team/").unwrap()), "{key} unnamed: {e}");
    }
    assert!(e.contains("delete"), "the refusal states the remedy: {e}");
    assert_eq!(b.list("team/").unwrap(), before, "the push deleted nothing and uploaded nothing");
    assert_eq!(manifest(b.as_ref()).generation, generation);
}

/// An object the manifest does not name stays fetchable by its key, so the left-behind check
/// finds it in the bucket's listing.
// spec: authority.bound-redistribution.manifest-is-an-index@c3eefad8
#[test]
fn an_object_the_manifest_never_named_still_refuses() {
    let dir = tempfile::tempdir().unwrap();
    let b = bucket(dir.path());
    let mut a = node("ingest-a", b.clone(), "");
    a.land_into("public", "run-1", json!([{"id": 1}]), LANDED);
    a.syncer.push(at(NOW)).unwrap();
    let key = "team/research/tables/filings/data/runs/run-0/ingest-z/part-00000.parquet";
    b.put(key, b"rows", Condition::IfNoneMatch).unwrap();
    let entries = manifest(b.as_ref()).entries;
    assert!(!entries.is_empty());
    assert!(!entries.keys().any(|k| k.contains("filings")));

    withhold(&mut a, "filings");
    let e = a.syncer.push(at(NOW)).unwrap_err().to_string();
    assert!(e.starts_with("EnforceStaleRedistributedObjects"), "{e}");
    assert!(e.contains("research/tables/filings/data/runs/run-0/ingest-z/part-00000.parquet"), "{e}");
    assert_eq!(b.get(key).unwrap().unwrap().0, b"rows");
}

/// A left-behind report names 100 objects and states the total found.
// spec: authority.bound-redistribution.report@fdf049fd
#[test]
fn a_left_behind_report_names_100_objects_and_the_total() {
    let dir = tempfile::tempdir().unwrap();
    let b = bucket(dir.path());
    let mut a = node("ingest-a", b.clone(), "");
    for i in 0..105 {
        b.put(&format!("team/research/tables/filings/data/runs/run-{i:03}/ingest-z/part-00000.parquet"), b"rows", Condition::IfNoneMatch).unwrap();
    }
    withhold(&mut a, "filings");
    let e = a.syncer.push(at(NOW)).unwrap_err().to_string();
    assert!(e.starts_with("EnforceStaleRedistributedObjects"), "{e}");
    assert_eq!(e.matches("/ingest-z/part-00000.parquet").count(), 100, "{e}");
    assert!(e.contains("105"), "{e}");
}

/// An object a concurrent writer uploads for a withheld table during the upload loop refuses
/// the push at the second check, before the manifest commits.
// spec: authority.bound-redistribution.two-checks@51d28312
#[test]
fn the_left_behind_check_runs_again_after_the_upload_loop() {
    let dir = tempfile::tempdir().unwrap();
    let inner = bucket(dir.path());
    let planted = Arc::new(AtomicBool::new(false));
    let (plant_into, flag) = (inner.clone(), planted.clone());
    let script = Script {
        on_put: Some(Box::new(move |key: &str, _: &Condition| {
            if key.contains("/tables/public/") && !flag.swap(true, Ordering::SeqCst) {
                let stray = "team/research/tables/filings/data/runs/run-9/ingest-z/part-00000.parquet";
                plant_into.put(stray, b"rows", Condition::IfNoneMatch).unwrap();
            }
            None
        })),
        ..Script::default()
    };
    let b: Arc<dyn ObjectStore> = Arc::new(Scripted { inner: inner.clone(), script });
    let mut a = node("ingest-a", b, "");
    a.land_into("public", "run-1", json!([{"id": 1}]), LANDED);
    withhold(&mut a, "filings");
    let e = a.syncer.push(at(NOW)).unwrap_err().to_string();
    assert!(planted.load(Ordering::SeqCst));
    assert!(e.starts_with("EnforceStaleRedistributedObjects"), "{e}");
    assert!(e.contains("run-9/ingest-z"), "{e}");
    assert!(inner.get("team/manifest.json").unwrap().is_none(), "no manifest commits past the second check");
}
