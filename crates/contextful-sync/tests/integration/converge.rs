//! What a push never overwrites, what a pull never regresses, and the tables a refresh reaches.

use crate::support::{at, bucket, node, trust, Script, Scripted};
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
fn encrypted_schema_merge_keeps_both_columns_without_plaintext() {
    use contextful_context::encrypt::{AesGcmFileCipher, MetadataFiles};
    use contextful_core::store::reconcile::{Column, ColumnType, Schema};
    use contextful_sync::sync::merge_schema_bytes;

    let key = AesGcmFileCipher::new([0x37; 32], 1);
    let codec = MetadataFiles::sealed(&key);
    let path = std::path::Path::new("schema.json");
    let first = serde_json::to_vec(&Schema { columns: vec![Column::new("metadata-canary-5f1e", ColumnType::Utf8, true)] }).unwrap();
    let second = serde_json::to_vec(&Schema { columns: vec![Column::new("pages", ColumnType::Int64, true)] }).unwrap();
    let mine = codec.seal_bytes(path, &first).unwrap();
    let theirs = codec.seal_bytes(path, &second).unwrap();
    let merged = merge_schema_bytes(&codec, path, &mine, &theirs).unwrap();
    assert!(!merged.is_empty(), "the exclusion below ranges over no element");
    assert!(!merged.windows(b"metadata-canary-5f1e".len()).any(|w| w == b"metadata-canary-5f1e"));
    let clear = codec.open_bytes(path, &merged).unwrap();
    let columns = schema_columns(&clear);
    assert!(columns.contains(&"metadata-canary-5f1e".to_string()) && columns.contains(&"pages".to_string()));
    assert_eq!(merge_schema_bytes(&codec, path, &merged, &theirs).unwrap(), merged);
    assert!(merge_schema_bytes(&MetadataFiles::sealed(&AesGcmFileCipher::new([0x42; 32], 1)), path, &mine, &theirs).is_err());
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
    assert_eq!(report.uploaded.iter().filter(|k| k.contains("ingest-b/")).count(), 0, "{:?}", report.uploaded);
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
    trust(&[&a, &c], "2030-01-01T00:00:00Z");
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

const RUN_B: &str = "research/tables/filings/data/runs/run-1/ingest-b/part-00000.parquet";

/// B lands and pushes a run, A pulls it, then B collects it and pushes its tombstone.
fn tombstoned(b: &Arc<dyn ObjectStore>) -> (crate::support::Node, crate::support::Node) {
    let (a, c) = (node("ingest-a", b.clone(), ""), node("ingest-b", b.clone(), ""));
    c.land("run-1", json!([{"id": 2}]), "2030-01-01T00:00:00Z");
    c.syncer.push(at(NOW)).unwrap();
    a.syncer.pull(&PullScope::default()).unwrap();
    std::fs::remove_dir_all(c.root().join("tables/filings/data/runs/run-1")).unwrap();
    c.syncer.push(at("2030-01-01T02:00:00Z")).unwrap();
    (a, c)
}

fn unhex(text: &str) -> Vec<u8> {
    (0..text.len()).step_by(2).map(|i| u8::from_str_radix(&text[i..i + 2], 16).unwrap()).collect()
}

/// A push signs each tombstone it writes with its node's issuer key, recording `signer` and `signature` over the key, owner and `deleted_at`; a node holding no issuer key writes it unsigned.
// spec: store.merge.tombstone-signed@72655178
#[test]
fn a_push_signs_each_tombstone_with_its_issuer_key() {
    use contextful_policy::issue::SignerKey;
    let dir = tempfile::tempdir().unwrap();
    let b = bucket(dir.path());
    let (_a, c) = tombstoned(&b);
    let t = manifest(b.as_ref()).tombstones[RUN_B].clone();
    assert_eq!(t.owner, "ingest-b");
    let key = SignerKey::of(c.signer.as_ref());
    assert_eq!(t.signer.as_deref(), Some(key.to_string().as_str()));
    let signature = unhex(t.signature.as_deref().unwrap());
    assert!(key.verifies(&t.message(RUN_B), &signature));
    // The signature covers the key, the owner and the instant.
    assert!(!key.verifies(&t.message("research/tables/filings/schema.json"), &signature));
    let moved = Tombstone { deleted_at: at("2030-01-02T00:00:00Z"), ..t.clone() };
    assert!(!key.verifies(&moved.message(RUN_B), &signature));

    // A node with no issuer key tombstones unsigned.
    let dir = tempfile::tempdir().unwrap();
    let b = bucket(dir.path());
    let mut c = node("ingest-b", b.clone(), "");
    c.syncer.signer = None;
    c.land("run-1", json!([{"id": 2}]), "2030-01-01T00:00:00Z");
    c.syncer.push(at(NOW)).unwrap();
    std::fs::remove_dir_all(c.root().join("tables/filings/data/runs/run-1")).unwrap();
    c.syncer.push(at("2030-01-01T02:00:00Z")).unwrap();
    let t = &manifest(b.as_ref()).tombstones[RUN_B];
    assert_eq!((t.signer.as_deref(), t.signature.as_deref()), (None, None));
}

/// A tombstone unsigned, failing its signature, or signed by a key the key-set ledger does not record as verifying at its `deleted_at` raises `SyncTombstoneUnverified`; a merge keeps the entry it names and a pull deletes nothing.
// spec: store.merge.tombstone-unverified@48537fa8
#[test]
fn an_unverified_tombstone_deletes_nothing() {
    use contextful_core::revoke::KeySetLedger;
    let unverified = |report: &contextful_sync::sync::PullReport| report.unverified.iter().any(|m| m.starts_with("SyncTombstoneUnverified") && m.contains(RUN_B));

    // No ledger records B's key: A keeps its copy and names the tombstone.
    let dir = tempfile::tempdir().unwrap();
    let b = bucket(dir.path());
    let (a, c) = tombstoned(&b);
    let local = a.root().join("tables/filings/data/runs/run-1/ingest-b/part-00000.parquet");
    let report = a.syncer.pull(&PullScope::default()).unwrap();
    assert!(local.exists() && report.removed.is_empty());
    assert!(unverified(&report), "{:?}", report.unverified);
    // Recorded, the same tombstone applies.
    trust(&[&a, &c], "2030-01-01T00:00:00Z");
    assert!(a.syncer.pull(&PullScope::default()).unwrap().unverified.is_empty());
    assert!(!local.exists());

    // A key the ledger retired before the deletion verifies nothing it signed after.
    let dir = tempfile::tempdir().unwrap();
    let b = bucket(dir.path());
    let (a, c) = tombstoned(&b);
    let mut ledger = KeySetLedger::default();
    ledger.retire_now(&c.signer.public_key_text(), at("2030-01-01T01:30:00Z"));
    let path = a.syncer.key_set.clone().unwrap();
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, ledger.to_toml()).unwrap();
    let report = a.syncer.pull(&PullScope::default()).unwrap();
    assert!(unverified(&report), "{:?}", report.unverified);
    assert!(a.root().join("tables/filings/data/runs/run-1/ingest-b/part-00000.parquet").exists());

    // A tombstone whose signature fails, or one written unsigned, removes no entry at a merge.
    for forged in [true, false] {
        let dir = tempfile::tempdir().unwrap();
        let b = bucket(dir.path());
        let (a, c) = (node("ingest-a", b.clone(), ""), node("ingest-b", b.clone(), ""));
        trust(&[&a, &c], "2030-01-01T00:00:00Z");
        c.land("run-1", json!([{"id": 2}]), "2030-01-01T00:00:00Z");
        c.syncer.push(at(NOW)).unwrap();
        let mut m = manifest(b.as_ref());
        let mut t = Tombstone { owner: "ingest-b".into(), deleted_at: at(NOW), signer: None, signature: None };
        if forged {
            t.signer = Some(contextful_policy::issue::SignerKey::of(c.signer.as_ref()).to_string());
            t.signature = Some("00".repeat(64));
        }
        m.tombstones.insert(RUN_B.into(), t);
        b.put("team/manifest.json", &serde_json::to_vec(&m).unwrap(), contextful_core::store::object::Condition::None).unwrap();
        let report = a.syncer.push(at("2030-01-01T02:00:00Z")).unwrap();
        assert!(report.refused.iter().any(|r| r.starts_with("SyncTombstoneUnverified") && r.contains(RUN_B)), "{:?}", report.refused);
        let after = manifest(b.as_ref());
        assert!(after.entries.contains_key(RUN_B) && !after.tombstones.contains_key(RUN_B), "forged: {forged}");
    }
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
    r.syncer.pull(&PullScope { tables: vec![], replicate_off: vec!["pii/users".into()], ..PullScope::default() }).unwrap();
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
