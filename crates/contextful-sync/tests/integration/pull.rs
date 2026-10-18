//! `store.pull`, `store.lease` and `store.replicate` through the syncer.

use crate::support::{at, bucket, node, Script, Scripted};
use contextful_context::fold::fold;
use contextful_core::store::declare::TableDecl;
use contextful_core::store::fold::FoldOutcome;
use contextful_core::store::object::{Condition, ObjectStore};
use contextful_core::store::sync::{BucketManifest, Entry};
use contextful_core::run::journal::sha256_hex;
use contextful_core::grant::TablePattern;
use contextful_core::store::StoreError;
use contextful_sync::{PullScope, SyncError};
use serde_json::json;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use contextful_core::issue::SignatureAlgorithm;
use contextful_core::surface::control::{receipt_file, snapshot_file, POINTER_FILE as CONTROL_POINTER};
use contextful_policy::control_receipt::ControlReceipt;
use contextful_policy::issue::SeedSigner;

const NOW: &str = "2030-01-01T01:00:00Z";
const PART_A: &str = "team/research/tables/filings/data/runs/run-1/ingest-a/part-00000.parquet";

/// A cold pull stages the listed control ancestry and leaves the local applied pointer absent.
// spec: store.pull.control-head@26468ef2
#[test]
fn a_cold_pull_stages_only_the_reachable_control_head_without_applying_it() {
    let dir = tempfile::tempdir().unwrap();
    let b = bucket(dir.path());
    let writer = node("ingest-a", b.clone(), "");
    let control = writer.syncer.control_dir.as_ref().unwrap();
    std::fs::create_dir_all(control).unwrap();
    let signer = SeedSigner::generate(SignatureAlgorithm::Ed25519);
    let first = ControlReceipt::sign("research", 1, None, b"first", &signer).unwrap();
    let orphan = ControlReceipt::sign("research", 2, Some(&first.digest()), b"orphan", &signer).unwrap();
    let head = ControlReceipt::sign("research", 3, Some(&first.digest()), b"head", &signer).unwrap();
    for (version, snapshot, receipt) in [(1, b"first".as_slice(), &first), (2, b"orphan".as_slice(), &orphan), (3, b"head".as_slice(), &head)] {
        std::fs::write(control.join(snapshot_file(version)), snapshot).unwrap();
        std::fs::write(control.join(receipt_file(version)), serde_json::to_vec(receipt).unwrap()).unwrap();
    }
    std::fs::write(control.join(CONTROL_POINTER), "3\n").unwrap();
    writer.syncer.push(at(NOW)).unwrap();
    let mut manifest: BucketManifest = serde_json::from_slice(&b.get("team/manifest.json").unwrap().unwrap().0).unwrap();
    for (name, bytes) in [(snapshot_file(2), b"orphan".to_vec()), (receipt_file(2), serde_json::to_vec(&orphan).unwrap())] {
        let key = format!("research/control/{name}");
        b.put(&format!("team/{key}"), &bytes, Condition::None).unwrap();
        manifest.entries.insert(key, Entry { sha256: sha256_hex(&bytes), size: bytes.len() as u64, owner: String::new() });
    }
    b.put("team/manifest.json", &serde_json::to_vec(&manifest).unwrap(), Condition::None).unwrap();

    let cold = node("ingest-b", b, "");
    cold.syncer.pull(&PullScope::default()).unwrap();
    let staged = cold.root().join("control");
    let pulled: contextful_core::store::sync::ControlHead = serde_json::from_slice(&std::fs::read(staged.join("head.json")).unwrap()).unwrap();
    assert_eq!(pulled.receipt_sha256, head.digest());
    for version in [1, 3] {
        assert!(staged.join(snapshot_file(version)).exists());
        assert!(staged.join(receipt_file(version)).exists());
    }
    assert!(!staged.join(snapshot_file(2)).exists());
    assert!(!staged.join(receipt_file(2)).exists());
    assert!(!cold.syncer.control_dir.as_ref().unwrap().join(CONTROL_POINTER).exists());
}

#[test]
fn a_head_version_without_its_receipt_never_stages_an_older_receipt_as_head() {
    let dir = tempfile::tempdir().unwrap();
    let b = bucket(dir.path());
    let writer = node("ingest-a", b.clone(), "");
    let control = writer.syncer.control_dir.as_ref().unwrap();
    std::fs::create_dir_all(control).unwrap();
    let signer = SeedSigner::generate(SignatureAlgorithm::Ed25519);
    let first = ControlReceipt::sign("research", 1, None, b"first", &signer).unwrap();
    let second = ControlReceipt::sign("research", 2, Some(&first.digest()), b"second", &signer).unwrap();
    for (version, snapshot, receipt) in [(1, b"first".as_slice(), &first), (2, b"second".as_slice(), &second)] {
        std::fs::write(control.join(snapshot_file(version)), snapshot).unwrap();
        std::fs::write(control.join(receipt_file(version)), serde_json::to_vec(receipt).unwrap()).unwrap();
    }
    std::fs::write(control.join(CONTROL_POINTER), "2\n").unwrap();
    writer.syncer.push(at(NOW)).unwrap();
    let mut manifest: BucketManifest = serde_json::from_slice(&b.get("team/manifest.json").unwrap().unwrap().0).unwrap();
    manifest.control_heads.get_mut("research").unwrap().version = 3;
    b.put("team/manifest.json", &serde_json::to_vec(&manifest).unwrap(), Condition::None).unwrap();

    let cold = node("ingest-b", b, "");
    assert!(matches!(cold.syncer.pull(&PullScope::default()), Err(SyncError::Store(StoreError::SyncPullDidNotConverge(_)))));
    assert!(!cold.root().join("control/head.json").exists());
}

#[test]
fn a_sparse_control_chain_reads_listed_receipts_without_walking_version_numbers() {
    let dir = tempfile::tempdir().unwrap();
    let b = bucket(dir.path());
    let writer = node("ingest-a", b.clone(), "");
    let control = writer.syncer.control_dir.as_ref().unwrap();
    std::fs::create_dir_all(control).unwrap();
    let signer = SeedSigner::generate(SignatureAlgorithm::Ed25519);
    let first = ControlReceipt::sign("research", 1, None, b"first", &signer).unwrap();
    let last = ControlReceipt::sign("research", u64::MAX, Some(&first.digest()), b"last", &signer).unwrap();
    for (version, snapshot, receipt) in [(1, b"first".as_slice(), &first), (u64::MAX, b"last".as_slice(), &last)] {
        std::fs::write(control.join(snapshot_file(version)), snapshot).unwrap();
        std::fs::write(control.join(receipt_file(version)), serde_json::to_vec(receipt).unwrap()).unwrap();
    }
    std::fs::write(control.join(CONTROL_POINTER), format!("{}\n", u64::MAX)).unwrap();
    writer.syncer.push(at(NOW)).unwrap();
    let cold = node("ingest-b", b, "");
    cold.syncer.pull(&PullScope::default()).unwrap();
    let head: contextful_core::store::sync::ControlHead = serde_json::from_slice(&std::fs::read(cold.root().join("control/head.json")).unwrap()).unwrap();
    assert_eq!(head.version, u64::MAX);
    assert_eq!(head.receipt_sha256, last.digest());
}

#[test]
fn a_sparse_control_ancestry_uses_listed_receipts_instead_of_the_version_range() {
    let (completed, result) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let dir = tempfile::tempdir().unwrap();
        let b = bucket(dir.path());
        let writer = node("ingest-a", b.clone(), "");
        let control = writer.syncer.control_dir.as_ref().unwrap();
        std::fs::create_dir_all(control).unwrap();
        let signer = SeedSigner::generate(SignatureAlgorithm::Ed25519);
        let first = ControlReceipt::sign("research", 1, None, b"first", &signer).unwrap();
        std::fs::write(control.join(snapshot_file(1)), b"first").unwrap();
        std::fs::write(control.join(receipt_file(1)), serde_json::to_vec(&first).unwrap()).unwrap();
        std::fs::write(control.join(CONTROL_POINTER), "1\n").unwrap();
        writer.syncer.push(at(NOW)).unwrap();
        let mut manifest: BucketManifest = serde_json::from_slice(&b.get("team/manifest.json").unwrap().unwrap().0).unwrap();
        let version = u64::MAX;
        let head = ControlReceipt::sign("research", version, Some(&first.digest()), b"head", &signer).unwrap();
        for (name, bytes) in [(snapshot_file(version), b"head".to_vec()), (receipt_file(version), serde_json::to_vec(&head).unwrap())] {
            let key = format!("research/control/{name}");
            b.put(&format!("team/{key}"), &bytes, Condition::None).unwrap();
            manifest.entries.insert(key, Entry { sha256: sha256_hex(&bytes), size: bytes.len() as u64, owner: String::new() });
        }
        manifest.control_heads.insert("research".into(), contextful_core::store::sync::ControlHead { version, receipt_sha256: head.digest() });
        b.put("team/manifest.json", &serde_json::to_vec(&manifest).unwrap(), Condition::None).unwrap();
        let cold = node("ingest-b", b, "");
        cold.syncer.pull(&PullScope::default()).unwrap();
        let staged = cold.root().join("control");
        assert!(staged.join(receipt_file(1)).exists());
        assert!(staged.join(receipt_file(version)).exists());
        assert!(!cold.syncer.control_dir.as_ref().unwrap().join(CONTROL_POINTER).exists());
        completed.send(()).unwrap();
    });
    result.recv_timeout(std::time::Duration::from_secs(5)).expect("two listed receipts finish without traversing the numeric version gap");
}

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

#[test]
fn a_clone_keeps_the_store_uuid_and_an_independent_store_refuses_it() {
    let dir = tempfile::tempdir().unwrap();
    let b = bucket(dir.path());
    let original = node("ingest-a", b.clone(), "");
    let identity = "11111111-1111-4111-8111-111111111111\n";
    std::fs::write(original.root().join("store-id"), identity).unwrap();
    original.syncer.push(at(NOW)).unwrap();

    let clone = node("ingest-b", b.clone(), "");
    clone.syncer.pull(&PullScope::default()).unwrap();
    assert_eq!(std::fs::read_to_string(clone.root().join("store-id")).unwrap(), identity);

    let scoped = node("ingest-d", b.clone(), "");
    scoped.syncer.pull(&PullScope { tables: vec![pattern("filings")], ..PullScope::default() }).unwrap();
    assert_eq!(std::fs::read_to_string(scoped.root().join("store-id")).unwrap(), identity);

    let independent = node("ingest-c", b, "");
    let other = "22222222-2222-4222-8222-222222222222\n";
    std::fs::write(independent.root().join("store-id"), other).unwrap();
    let refused = independent.syncer.pull(&PullScope::default()).unwrap_err();
    assert!(matches!(refused, SyncError::Store(StoreError::StoreIdentityConflict(_))), "{refused}");
    assert_eq!(std::fs::read_to_string(independent.root().join("store-id")).unwrap(), other);
}

/// A downloaded object whose digest differs from its entry raises `SyncObjectDigestMismatch` and is discarded.
// spec: store.pull.digest-mismatch@e4fabb02
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

/// A pull refused by {{store.pull.unconverged}} keeps each object it verified, so the next `sync pull` downloads
/// only the keys still differing; no pull retries past its attempts.
// spec: store.pull.unconverged-resume@ef4ad40e
#[test]
fn an_exhausted_pull_keeps_what_it_verified_and_the_next_fetches_only_the_rest() {
    let (_dir, b, a) = pushed();
    let held = a.syncer.acquire("filings", at(NOW)).unwrap();
    let FoldOutcome::Folded { snapshot_id, .. } = fold(&a.syncer.store, &TableDecl::named("filings"), at(NOW)).unwrap() else { panic!() };
    a.syncer.push(at(NOW)).unwrap();
    a.syncer.publish("filings", &snapshot_id, &held).unwrap();
    let part = format!("team/research/tables/filings/data/snapshots/{snapshot_id}/part-00000.parquet");
    let moving = part.clone();
    let gets = Arc::new(AtomicUsize::new(0));
    let counted = gets.clone();
    let flaky: Arc<dyn ObjectStore> = Arc::new(Scripted {
        inner: b.clone(),
        script: Script {
            on_get: Some(Box::new(move |key| {
                if key == moving {
                    counted.fetch_add(1, Ordering::SeqCst);
                    return Some(Ok(None));
                }
                None
            })),
            ..Script::default()
        },
    });
    let c = node("ingest-b", flaky, "");
    assert!(matches!(c.syncer.pull(&PullScope::default()), Err(SyncError::Store(StoreError::SyncPullDidNotConverge(_)))));
    assert_eq!(gets.load(Ordering::SeqCst), 3, "one get of the moving key per attempt, and no fourth");
    // Every other verified object stayed.
    let schema = c.root().join("tables/filings/schema.json");
    let snapshot_manifest = c.root().join(format!("tables/filings/data/snapshots/{snapshot_id}/_manifest.json"));
    assert!(schema.is_file() && snapshot_manifest.is_file());
    assert!(!c.root().join("tables/filings/_pointer.json").exists());

    // The next pull, against a bucket that holds still, downloads the shortfall alone.
    let mut c = c;
    c.syncer.bucket = b;
    let report = c.syncer.pull(&PullScope::default()).unwrap();
    assert_eq!(report.downloaded, [part.trim_start_matches("team/").to_string()]);
    assert_eq!(report.pointers, ["research/tables/filings/_pointer.json"]);
}

/// A replica's pull writes `replica.json` beside `derived.sqlite`, naming per table each snapshot it holds with
/// that snapshot's parts and sidecar paths present on disk; no push carries it.
// spec: store.replicate.descriptor@a083a2e5
#[test]
fn a_replica_pull_writes_its_descriptor_beside_the_catalog() {
    use contextful_context::replica::{read, DESCRIPTOR_FILE};
    let (_dir, b, a) = pushed();
    let decl = contextful_core::store::declare::TableDecl::parse_pipeline(
        "[[pipeline.tables]]\nname = \"filings\"\n[[pipeline.tables.indexes]]\nkind = \"fulltext\"\ncolumn = \"title\"\nid_column = \"id\"\n",
    )
    .unwrap()
    .remove(0);
    let held = a.syncer.acquire("filings", at(NOW)).unwrap();
    let FoldOutcome::Folded { snapshot_id, .. } = fold(&a.syncer.store, &decl, at(NOW)).unwrap() else { panic!() };
    a.syncer.push(at(NOW)).unwrap();
    a.syncer.publish("filings", &snapshot_id, &held).unwrap();
    // A writer advertises nothing.
    assert!(!a.root().join(DESCRIPTOR_FILE).exists());

    let r = node("replica-1", b.clone(), "\n[replica]\nof = \"team/research\"\n");
    r.syncer.pull(&PullScope::default()).unwrap();
    assert!(r.root().join(DESCRIPTOR_FILE).is_file() && r.root().join(DESCRIPTOR_FILE).parent() == Some(r.root().as_path()));
    let d = read(&r.syncer.store).unwrap().unwrap();
    assert_eq!(d.of, "team/research");
    let filings = &d.tables["filings"];
    assert_eq!(filings[0].snapshot_id, snapshot_id);
    assert_eq!(filings[0].parts, ["part-00000.parquet"]);
    assert_eq!(filings[0].sidecars, ["indexes/fts-title-unicode"]);

    // A writer's push carries no descriptor, and an emitted plan lists none.
    std::fs::write(a.root().join(DESCRIPTOR_FILE), b"{}").unwrap();
    a.syncer.push(at("2030-01-01T02:00:00Z")).unwrap();
    let m: BucketManifest = serde_json::from_slice(&b.get("team/manifest.json").unwrap().unwrap().0).unwrap();
    assert!(!m.entries.is_empty());
    assert!(!m.entries.keys().any(|k| k.ends_with(DESCRIPTOR_FILE)));
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

/// A pulled snapshot carrying a part by reference is home once that part is under its
/// origin's directory, so the pull writes its pointer and reads every row
/// (`store.fold.carry-by-reference`).
#[test]
fn a_pulled_snapshot_finds_its_carried_part_under_the_origin() {
    let (_dir, b, a) = pushed();
    let decl = TableDecl::named("filings");
    let held = a.syncer.acquire("filings", at(NOW)).unwrap();
    let FoldOutcome::Folded { snapshot_id: first, .. } = fold(&a.syncer.store, &decl, at(NOW)).unwrap() else { panic!() };
    a.land("run-2", json!([{"id": 2, "title": "b"}]), "2030-01-01T02:00:00Z");
    let FoldOutcome::Folded { snapshot_id: second, .. } = fold(&a.syncer.store, &decl, at("2030-01-01T03:00:00Z")).unwrap() else { panic!() };
    a.syncer.push(at("2030-01-01T03:00:00Z")).unwrap();
    a.syncer.publish("filings", &second, &held).unwrap();
    let c = node("ingest-b", b, "");
    c.syncer.pull(&PullScope::default()).unwrap();
    let (chain, _) = c.syncer.store.chain("filings").unwrap();
    assert_eq!(chain[0].snapshot_id.to_string(), second);
    assert_eq!(chain[0].parts.iter().filter_map(|p| p.origin.as_ref().map(ToString::to_string)).collect::<Vec<_>>(), [first.clone()]);
    assert_eq!(files(&c), [first, second].map(|id| format!("tables/filings/data/snapshots/{id}/part-00000.parquet")));
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
            injection: contextful_core::store::reserve::Injection { run_id: "run-x".into(), site_id: "s".into(), batch_seq: None, authored_by: None, authored_agent: None, taint: None },
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

/// Each fold pass merges a table's committed ledger files into `requests/folded-<snapshot-id>.parquet`, and a
/// replica carries ledgers with their table.
// spec: store.reserve.ledger-fold@41270f90
#[test]
fn a_pass_merges_committed_ledgers_and_a_replica_carries_the_merge() {
    use contextful_core::store::ledger::RequestRecord;
    let (_dir, b, a) = pushed();
    let call = |id: &str| RequestRecord {
        request_id: id.into(),
        vendor_request_id: None,
        connector: "http".into(),
        method: "GET".into(),
        url_host: "api.example.org".into(),
        status_code: Some(200),
        started_at: at("2030-01-01T00:00:00Z"),
        duration_ms: 1,
        batch_seq: Some(0),
    };
    let node_a = contextful_core::store::lay_out::NodeId::parse("ingest-a").unwrap();
    contextful_context::ledger::append(&a.syncer.store, "filings", "run-1", &node_a, &[call("c1"), call("c2")]).unwrap();
    contextful_context::ledger::append(&a.syncer.store, "filings", "run-9", &node_a, &[call("c9")]).unwrap();
    let held = a.syncer.acquire("filings", at(NOW)).unwrap();
    let FoldOutcome::Folded { snapshot_id, .. } = fold(&a.syncer.store, &TableDecl::named("filings"), at(NOW)).unwrap() else { panic!() };
    let merged = format!("folded-{snapshot_id}.parquet");
    let names = |n: &crate::support::Node| -> Vec<String> {
        contextful_context::ledger::files(&n.syncer.store, "filings").unwrap().iter().map(|p| p.file_name().unwrap().to_string_lossy().into_owned()).collect()
    };
    // The committed run's file merged; the run still in flight keeps its own.
    assert_eq!(names(&a), [merged.clone(), "run-9.ingest-a.parquet".to_string()]);
    a.syncer.push(at(NOW)).unwrap();
    a.syncer.publish("filings", &snapshot_id, &held).unwrap();

    let r = node("replica-1", b, "\n[replica]\nof = \"team/research\"\n");
    r.syncer.pull(&PullScope::default()).unwrap();
    assert_eq!(names(&r), [merged, "run-9.ingest-a.parquet".to_string()]);
    let mut calls: Vec<String> = contextful_context::ledger::files(&r.syncer.store, "filings")
        .unwrap()
        .iter()
        .flat_map(|p| contextful_context::ledger::read(p).unwrap())
        .map(|(run, c)| format!("{run}/{}", c.request_id))
        .collect();
    calls.sort();
    assert_eq!(calls, ["run-1/c1", "run-1/c2", "run-9/c9"]);
}

/// A refresh requesting a replicate-off table raises `ReplicaSensitiveTable`; the consumer reads through the
/// proxying face.
// spec: store.replicate.sensitive-refused@e105ab21
#[test]
fn a_refresh_requesting_a_replicate_off_table_refuses() {
    let (_dir, b, _a) = pushed();
    let r = node("replica-1", b, "\n[replica]\nof = \"team/research\"\n");
    let scope = PullScope { tables: vec![pattern("filings")], replicate_off: vec!["filings".into()], ..PullScope::default() };
    match r.syncer.pull(&scope) {
        Err(SyncError::Store(StoreError::ReplicaSensitiveTable(m))) => assert!(m.contains("filings") && m.contains("proxying face"), "{m}"),
        other => panic!("{other:?}"),
    }
    // An unscoped refresh takes every other table and leaves the replicate-off one behind.
    r.syncer.pull(&PullScope { tables: vec![], replicate_off: vec!["filings".into()], ..PullScope::default() }).unwrap();
    assert!(!r.root().join("tables/filings").exists());
}

/// A replica's refresh selects tables through the grant pattern matcher: a prefix pattern
/// takes every table it begins, and a concrete name matches only itself.
// spec: authority.grant.one-matcher@f787cc2d
#[test]
fn a_replica_refresh_selects_tables_through_the_grant_pattern_matcher() {
    let (_dir, b, _a) = pushed();
    let exact = node("replica-1", b.clone(), "\n[replica]\nof = \"team/research\"\n");
    exact.syncer.pull(&PullScope { tables: vec![pattern("fil")], ..PullScope::default() }).unwrap();
    assert!(!exact.root().join("tables/filings").exists());
    let prefixed = node("replica-2", b, "\n[replica]\nof = \"team/research\"\n");
    prefixed.syncer.pull(&PullScope { tables: vec![pattern("fil*")], ..PullScope::default() }).unwrap();
    assert!(prefixed.root().join("tables/filings").exists());
}

fn pattern(s: &str) -> TablePattern {
    TablePattern::parse(s).unwrap()
}

/// Seed of the interrupted-pull sampler.
const PULL_SAMPLER_SEED: u64 = 0x5eed_0005;
/// Sampled (kill point, fault) pairs, after one clean pull.
const PULL_SAMPLES: u64 = 32;

/// What a sampled pull meets at its `at`-th object get.
#[derive(Clone, Copy, Debug)]
enum Fault {
    /// The get fails, once.
    Transport,
    /// The object is absent, once.
    MissingOnce,
    /// The object is absent on every later get of its key.
    MissingAlways,
    /// The object arrives with other bytes.
    Tampered,
}

#[derive(Default)]
struct Sampler {
    /// The node root a reader inspects at every get; `None` while the sampler is idle.
    root: Option<std::path::PathBuf>,
    /// The kill point and its fault; `None` runs a clean pull.
    plan: Option<(usize, Fault)>,
    gets: usize,
    lost_key: Option<String>,
    /// Snapshot ids a reader met, `Err` holding a pointer ahead of its parts.
    seen: Vec<Result<Option<String>, String>>,
}

/// The snapshot a reader of `root` meets: the local pointer's, once every part its
/// manifest names is home; `Err` for a pointer ahead of its parts.
fn reader(root: &std::path::Path) -> Result<Option<String>, String> {
    let table = root.join("tables/filings");
    let Ok(bytes) = std::fs::read(table.join("_pointer.json")) else { return Ok(None) };
    let pointer: serde_json::Value = serde_json::from_slice(&bytes).map_err(|e| format!("pointer: {e}"))?;
    let id = pointer["snapshot_id"].as_str().ok_or("a pointer names no snapshot")?.to_string();
    let dir = table.join("data/snapshots").join(&id);
    let manifest: serde_json::Value = std::fs::read(dir.join("_manifest.json"))
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .ok_or_else(|| format!("{id}: no manifest"))?;
    let parts = manifest["parts"].as_array().ok_or_else(|| format!("{id}: no parts"))?;
    match parts.iter().filter_map(|p| p["name"].as_str()).find(|n| !dir.join(n).exists()) {
        Some(missing) => Err(format!("{id}: `{missing}` absent")),
        None => Ok(Some(id)),
    }
}

/// Fold and publish the landed runs as the next snapshot, returning its id.
fn publish_next(a: &crate::support::Node, now: &str) -> String {
    let held = a.syncer.acquire("filings", at(now)).unwrap();
    let FoldOutcome::Folded { snapshot_id, .. } = fold(&a.syncer.store, &TableDecl::named("filings"), at(now)).unwrap() else { panic!() };
    a.syncer.push(at(now)).unwrap();
    a.syncer.publish("filings", &snapshot_id, &held).unwrap();
    snapshot_id.to_string()
}

/// One sampled pull: a node holding a first snapshot pulls the next under `plan`, a reader
/// looking at its store before every object get and once after. Returns the prior and next
/// snapshot ids, the pull's outcome and the sampler.
fn sampled_pull(plan: Option<(usize, Fault)>) -> (String, String, Result<(), String>, Sampler) {
    let (_dir, b, a) = pushed();
    let first = publish_next(&a, NOW);
    let sampler = Arc::new(std::sync::Mutex::new(Sampler::default()));
    let s = sampler.clone();
    let scripted: Arc<dyn ObjectStore> = Arc::new(Scripted {
        inner: b.clone(),
        script: Script {
            on_get: Some(Box::new(move |key| {
                let mut s = s.lock().unwrap();
                let Some(root) = s.root.clone() else { return None };
                let seen = reader(&root);
                s.seen.push(seen);
                let at = s.gets;
                s.gets += 1;
                if s.lost_key.as_deref() == Some(key) {
                    return Some(Ok(None));
                }
                match s.plan {
                    Some((k, fault)) if k == at => match fault {
                        Fault::Transport => Some(Err(contextful_core::store::object::ObjectError::Transport("sampled".into()))),
                        Fault::MissingOnce => Some(Ok(None)),
                        Fault::MissingAlways => {
                            s.lost_key = Some(key.to_string());
                            Some(Ok(None))
                        }
                        Fault::Tampered => Some(Ok(Some((b"sampled".to_vec(), "\"sampled\"".into())))),
                    },
                    _ => None,
                }
            })),
            ..Script::default()
        },
    });
    let c = node("ingest-b", scripted, "");
    c.syncer.pull(&PullScope::default()).unwrap();
    assert_eq!(reader(&c.root()), Ok(Some(first.clone())), "the first pull brings the first snapshot home");

    a.land("run-2", json!([{"id": 2, "title": "b"}]), "2030-01-01T01:30:00Z");
    a.syncer.push(at("2030-01-01T02:00:00Z")).unwrap();
    let next = publish_next(&a, "2030-01-01T02:00:00Z");
    assert_ne!(first, next);
    {
        let mut s = sampler.lock().unwrap();
        s.root = Some(c.root());
        s.plan = plan;
    }
    let outcome = c.syncer.pull(&PullScope::default()).map(|_| ()).map_err(|e| e.to_string());
    let mut s = std::mem::take(&mut *sampler.lock().unwrap());
    s.seen.push(reader(&c.root()));
    (first, next, outcome, s)
}

/// Over a seeded sample of kill points and faults across every object get of a pull, a
/// reader never meets a pointer ahead of its parts, and an interrupted or refused pull
/// leaves the prior snapshot in place.
#[test]
fn a_seeded_sample_of_interrupted_pulls_never_exposes_a_torn_snapshot() {
    let (_, next, outcome, clean) = sampled_pull(None);
    assert_eq!(outcome, Ok(()));
    assert_eq!(clean.seen.last(), Some(&Ok(Some(next))), "a clean pull advances to the next snapshot");
    let gets = clean.gets;
    assert!(gets >= 3, "a pull reads the manifest, the parts and the pointer: {gets}");

    let faults = [Fault::Transport, Fault::MissingOnce, Fault::MissingAlways, Fault::Tampered];
    let mut state = PULL_SAMPLER_SEED;
    let mut draw = |n: usize| {
        state = state.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        ((state >> 33) % n as u64) as usize
    };
    let (mut torn, mut observations, mut failed) = (0u64, clean.seen.len() as u64, 0u64);
    for _ in 0..PULL_SAMPLES {
        let plan = (draw(gets), faults[draw(faults.len())]);
        let (first, next, outcome, s) = sampled_pull(Some(plan));
        observations += s.seen.len() as u64;
        for seen in &s.seen {
            match seen {
                Ok(Some(id)) if *id == first || *id == next => {}
                other => {
                    torn += 1;
                    eprintln!("{plan:?}: a reader met {other:?}");
                }
            }
        }
        // An interrupted or refused pull leaves the prior snapshot.
        if let Err(e) = &outcome {
            failed += 1;
            if s.seen.last() != Some(&Ok(Some(first.clone()))) {
                torn += 1;
                eprintln!("{plan:?}: the pull refused ({e}) and left {:?}", s.seen.last());
            }
        }
    }
    eprintln!("pull sampler: {PULL_SAMPLES} samples over {gets} gets, {failed} pulls refused, {torn} torn of {observations} reads");
    contextful_eval::record::emit("pull-no-torn-snapshot", torn as f64, observations, PULL_SAMPLER_SEED);
    assert!(failed > 0, "the sample interrupts at least one pull");
    assert_eq!(torn, 0, "{torn} torn reads over {observations} observations");
}

/// A store holding no run state of its own node pulls that node's keys it lacks, so a replacement
/// host reusing the node id tombstones none of them; a store that pushed keeps its own deletions.
// spec: store.pull.cold-own-keys@503cc5c7
#[test]
fn a_cold_store_pulls_its_own_node_keys_and_its_push_tombstones_none() {
    let dir = tempfile::tempdir().unwrap();
    let b = bucket(dir.path());
    let first = node("ingest-a", b.clone(), "");
    first.land("run-1", json!([{"id": 1}]), "2030-01-01T00:00:00Z");
    first.syncer.push(at(NOW)).unwrap();
    let part = "research/tables/filings/data/runs/run-1/ingest-a/part-00000.parquet";

    let replacement = node("ingest-a", b.clone(), "");
    let report = replacement.syncer.pull(&PullScope::default()).unwrap();
    assert!(report.downloaded.iter().any(|k| k == part), "{:?}", report.downloaded);
    replacement.syncer.push(at("2030-01-01T02:00:00Z")).unwrap();
    let (bytes, _) = b.get("team/manifest.json").unwrap().unwrap();
    let listed: BucketManifest = serde_json::from_slice(&bytes).unwrap();
    assert!(listed.entries.contains_key(part) && listed.tombstones.is_empty(), "{:?}", listed.tombstones.keys().collect::<Vec<_>>());

    // A store whose push recorded its node's run state treats its own absent key as its deletion.
    let store = contextful_context::Store::open(replacement._dir.path(), "research").unwrap();
    let state = contextful_sync::RunState { format: contextful_sync::run_state::RUN_STATE_FORMAT, node_id: "ingest-a".into(), runs: Default::default(), cursors: Vec::new(), control_version: None };
    contextful_sync::run_state::record(&store, &state).unwrap();
    std::fs::remove_dir_all(replacement.root().join("tables/filings/data/runs/run-1")).unwrap();
    replacement.syncer.pull(&PullScope::default()).unwrap();
    assert!(!replacement.root().join("tables/filings/data/runs/run-1").exists());
}

/// A tombstone naming a key the pulling store holds no copy of deletes nothing and raises nothing.
#[test]
fn a_tombstone_for_a_key_never_held_raises_nothing_on_pull() {
    let dir = tempfile::tempdir().unwrap();
    let b = bucket(dir.path());
    let writer = node("ingest-a", b.clone(), "");
    writer.land("run-1", json!([{"id": 1}]), "2030-01-01T00:00:00Z");
    writer.syncer.push(at(NOW)).unwrap();
    std::fs::remove_dir_all(writer.root().join("tables/filings/data/runs/run-1")).unwrap();
    writer.syncer.push(at("2030-01-01T02:00:00Z")).unwrap();

    let reader = node("ingest-b", b.clone(), "");
    let report = reader.syncer.pull(&PullScope::default()).unwrap();
    assert!(report.unverified.is_empty(), "{:?}", report.unverified);
}

/// A run state overwritten in place by a push that never committed is discarded with a warning, the pull
/// lands every other key, and the node's next push lists its fresh run state.
// spec: store.pull.run-state-torn@f0e7f46d
// spec: store.push.run-state-upload@f31449b9
#[test]
fn a_run_state_torn_by_an_uncommitted_push_is_discarded_and_the_next_push_heals_it() {
    let dir = tempfile::tempdir().unwrap();
    let b = bucket(dir.path());
    let first = node("ingest-a", b.clone(), "");
    first.land("run-1", json!([{"id": 1}]), "2030-01-01T00:00:00Z");
    let store = contextful_context::Store::open(first._dir.path(), "research").unwrap();
    let state = contextful_sync::RunState { format: contextful_sync::run_state::RUN_STATE_FORMAT, node_id: "ingest-a".into(), runs: Default::default(), cursors: Vec::new(), control_version: None };
    contextful_sync::run_state::record(&store, &state).unwrap();
    first.syncer.push(at(NOW)).unwrap();
    let key = "research/nodes/ingest-a/run-state.json";
    let part = "research/tables/filings/data/runs/run-1/ingest-a/part-00000.parquet";
    std::fs::write(dir.path().join("context-team/team").join(key), br#"{"format":1,"node_id":"ingest-a","control_version":7}"#).unwrap();

    for id in ["ingest-a", "ingest-b"] {
        let cold = node(id, b.clone(), "");
        let report = cold.syncer.pull(&PullScope::default()).unwrap();
        assert_eq!(report.discarded.len(), 1, "{:?}", report.discarded);
        assert!(report.discarded[0].contains(key), "{}", report.discarded[0]);
        assert!(report.downloaded.iter().any(|k| k == part), "{:?}", report.downloaded);
        assert!(!cold.root().join("nodes/ingest-a/run-state.json").exists());
    }

    let replacement = node("ingest-a", b.clone(), "");
    replacement.syncer.pull(&PullScope::default()).unwrap();
    let store = contextful_context::Store::open(replacement._dir.path(), "research").unwrap();
    contextful_sync::run_state::record(&store, &state).unwrap();
    replacement.syncer.push(at("2030-01-01T02:00:00Z")).unwrap();
    let reader = node("ingest-b", b, "");
    let report = reader.syncer.pull(&PullScope::default()).unwrap();
    assert!(report.discarded.is_empty(), "{:?}", report.discarded);
    assert!(reader.root().join("nodes/ingest-a/run-state.json").exists());
}

/// Bytes of one seeded object.
const OBJECT_BYTES: usize = 1024;

/// `len` bytes of a xorshift stream from `seed`.
fn seeded_bytes(seed: u64, len: usize) -> Vec<u8> {
    let mut x = seed | 1;
    (0..len)
        .map(|_| {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            x as u8
        })
        .collect()
}

/// A bucket holding `foreign` run parts owned by `ingest-a` and `own` owned by `ingest-b`, each 1,024 seeded bytes put
/// through the bucket, and `team/manifest.json` listing every one; returns the bucket and the manifest's bytes.
fn seeded_bucket(dir: &std::path::Path, foreign: usize, own: usize, seed: u64) -> (Arc<dyn ObjectStore>, Vec<u8>) {
    let b = bucket(dir);
    let mut manifest = BucketManifest::default();
    let keys = (0..foreign)
        .map(|i| (format!("research/tables/filings/data/runs/r{i:05}/ingest-a/part-00000.parquet"), "ingest-a"))
        .chain((0..own).map(|i| (format!("research/tables/filings/data/runs/b{i:02}/ingest-b/part-00000.parquet"), "ingest-b")));
    for (i, (key, owner)) in keys.enumerate() {
        let bytes = seeded_bytes(seed.wrapping_add(i as u64), OBJECT_BYTES);
        b.put(&format!("team/{key}"), &bytes, Condition::None).unwrap();
        manifest.entries.insert(key, Entry { sha256: sha256_hex(&bytes), size: bytes.len() as u64, owner: owner.into() });
    }
    let bytes = serde_json::to_vec_pretty(&manifest).unwrap();
    b.put("team/manifest.json", &bytes, Condition::None).unwrap();
    (b, bytes)
}

/// A pull with nothing to fetch still reads the whole bucket manifest, reads every local copy it lists and hashes
/// each copy another node owns, and lists the project's `tables/` prefix to find pointers. Over 1,000 and 10,000
/// foreign objects plus 10 of the puller's own, a second pull right after a cold one records the keys it listed, the
/// local bytes it read and hashed, and the manifest bytes it fetched. Under `contextful-ci measure` the sizes are
/// 1,000 and 10,000; elsewhere 100 and 1,000 exercise the path.
#[test]
fn a_noop_pull_counts_its_listed_keys_hashed_bytes_and_manifest_bytes() {
    const SEED: u64 = 0x5eed_0090;
    const OWN: usize = 10;
    let measuring = std::env::var_os(contextful_eval::record::MEASURE_DIR_VAR).is_some();
    let sizes = if measuring { [1_000, 10_000] } else { [100, 1_000] };
    for (size, label) in sizes.into_iter().zip(["1k", "10k"]) {
        let dir = tempfile::tempdir().unwrap();
        let (inner, manifest_bytes) = seeded_bucket(dir.path(), size, OWN, SEED);
        let counted = Arc::new(Scripted { inner, script: Script::default() });
        let puller = node("ingest-b", counted.clone(), "");
        let cold = puller.syncer.pull(&PullScope::default()).unwrap();
        assert_eq!(cold.downloaded.len(), size + OWN, "a cold pull fetches every listed object, its own included");
        counted.script.reset();
        let noop = puller.syncer.pull(&PullScope::default()).unwrap();
        assert!(noop.downloaded.is_empty() && noop.removed.is_empty(), "{noop:?}");
        let listed = counted.script.listed_under("team/research/tables/");
        let manifest = counted.script.fetched_bytes("team/manifest.json");
        assert_eq!(manifest % manifest_bytes.len(), 0, "the pull fetches the manifest whole");
        eprintln!(
            "no-op pull over {size} foreign objects: {listed} key(s) listed, {} B read, {} B hashed, {manifest} manifest B",
            noop.bytes_read, noop.bytes_hashed
        );
        contextful_eval::record::emit(&format!("sync-noop-pull-keys-listed-{label}"), listed as f64, size as u64, SEED);
        contextful_eval::record::emit(&format!("sync-noop-pull-hashed-{label}"), noop.bytes_hashed as f64, size as u64, SEED);
        if label == "10k" {
            contextful_eval::record::emit("sync-noop-pull-read-10k", noop.bytes_read as f64, size as u64, SEED);
            contextful_eval::record::emit("sync-noop-pull-manifest-bytes-10k", manifest as f64, size as u64, SEED);
        }
    }
}

/// Whether `key` is an object get of a pull's download loop, not a manifest or pointer read.
fn object_get(key: &str) -> bool {
    key.starts_with("team/research/tables/") && !key.ends_with("_pointer.json")
}

/// A bucket wrapper holding each object get until `bound` gets, or as many as remain, are in flight, and recording the
/// peak in flight. A first get alone for 2 s marks the downloads sequential, and nothing is held after that.
struct Gathering {
    inner: Arc<dyn ObjectStore>,
    bound: usize,
    total: usize,
    /// Gets in flight, gets finished, peak in flight, and whether the downloads read as sequential.
    state: std::sync::Mutex<(usize, usize, usize, bool)>,
    turn: std::sync::Condvar,
}

impl ObjectStore for Gathering {
    fn get(&self, key: &str) -> Result<Option<(Vec<u8>, String)>, contextful_core::store::object::ObjectError> {
        if !object_get(key) {
            return self.inner.get(key);
        }
        {
            let mut s = self.state.lock().unwrap();
            s.0 += 1;
            s.2 = s.2.max(s.0);
            self.turn.notify_all();
            let target = self.bound.min(self.total.saturating_sub(s.1)).max(1);
            let window = if s.0 == 1 && s.1 == 0 { 2 } else { 30 };
            let (mut s, wait) = self.turn.wait_timeout_while(s, std::time::Duration::from_secs(window), |s| !s.3 && s.0 < target).unwrap();
            if wait.timed_out() {
                s.3 = true;
                self.turn.notify_all();
            }
        }
        let got = self.inner.get(key);
        let mut s = self.state.lock().unwrap();
        s.0 -= 1;
        s.1 += 1;
        self.turn.notify_all();
        got
    }

    fn put(&self, key: &str, bytes: &[u8], condition: Condition) -> Result<contextful_core::store::object::Put, contextful_core::store::object::ObjectError> {
        self.inner.put(key, bytes, condition)
    }

    fn delete(&self, key: &str) -> Result<(), contextful_core::store::object::ObjectError> {
        self.inner.delete(key)
    }

    fn list(&self, prefix: &str) -> Result<Vec<String>, contextful_core::store::object::ObjectError> {
        self.inner.list(prefix)
    }

    fn cas_scope(&self) -> contextful_core::store::object::CasScope {
        self.inner.cas_scope()
    }
}

/// A cold pull of 32 seeded objects through a bucket that holds each object get until 16, or as many as remain, are in
/// flight; the record is the peak number of object gets in flight at once.
#[test]
fn a_cold_pull_reports_its_peak_in_flight_object_gets() {
    const SEED: u64 = 0x5eed_0091;
    const OBJECTS: usize = 32;
    let dir = tempfile::tempdir().unwrap();
    let (inner, _) = seeded_bucket(dir.path(), OBJECTS, 0, SEED);
    let gathering = Arc::new(Gathering { inner, bound: 16, total: OBJECTS, state: Default::default(), turn: Default::default() });
    let cold = node("ingest-b", gathering.clone(), "");
    let report = cold.syncer.pull(&PullScope::default()).unwrap();
    assert_eq!(report.downloaded.len(), OBJECTS);
    let (in_flight, finished, peak, sequential) = *gathering.state.lock().unwrap();
    assert_eq!((in_flight, finished), (0, OBJECTS), "every object get finished");
    eprintln!("a cold pull of {OBJECTS} objects peaked at {peak} object get(s) in flight (sequential: {sequential})");
    contextful_eval::record::emit("sync-cold-pull-overlap", peak as f64, OBJECTS as u64, SEED);
}

/// A bucket wrapper adding a fixed delay to each object get, as a round trip to a remote bucket.
struct Delayed {
    inner: Arc<dyn ObjectStore>,
    delay: std::time::Duration,
}

impl ObjectStore for Delayed {
    fn get(&self, key: &str) -> Result<Option<(Vec<u8>, String)>, contextful_core::store::object::ObjectError> {
        if object_get(key) {
            std::thread::sleep(self.delay);
        }
        self.inner.get(key)
    }

    fn put(&self, key: &str, bytes: &[u8], condition: Condition) -> Result<contextful_core::store::object::Put, contextful_core::store::object::ObjectError> {
        self.inner.put(key, bytes, condition)
    }

    fn delete(&self, key: &str) -> Result<(), contextful_core::store::object::ObjectError> {
        self.inner.delete(key)
    }

    fn list(&self, prefix: &str) -> Result<Vec<String>, contextful_core::store::object::ObjectError> {
        self.inner.list(prefix)
    }

    fn cas_scope(&self) -> contextful_core::store::object::CasScope {
        self.inner.cas_scope()
    }
}

/// Wall time of one cold pull of 32 seeded objects into a fresh store, each object get delayed 2 ms; the node is
/// built outside the timed span. p50 and p95 over 200 timed pulls after 20 warm-up, the median of 5 batches
/// (`assurance.measure.timing-iterations`, `assurance.measure.timing-batches`). Under `contextful-ci measure` that is
/// the schedule; elsewhere 3 pulls after 1 exercise the path.
#[test]
fn cold_pull_wall_over_a_fixed_latency_bucket() {
    const SEED: u64 = 0x5eed_0091;
    const OBJECTS: usize = 32;
    let measuring = std::env::var_os(contextful_eval::record::MEASURE_DIR_VAR).is_some();
    let (warm_up, repeats, batches) = if measuring { (20, 200, 5) } else { (1, 3, 1) };
    let dir = tempfile::tempdir().unwrap();
    let (inner, _) = seeded_bucket(dir.path(), OBJECTS, 0, SEED);
    let delayed: Arc<dyn ObjectStore> = Arc::new(Delayed { inner, delay: std::time::Duration::from_millis(2) });
    let pull_ms = || {
        let fresh = node("ingest-b", delayed.clone(), "");
        let started = std::time::Instant::now();
        let report = fresh.syncer.pull(&PullScope::default()).unwrap();
        let ms = started.elapsed().as_secs_f64() * 1e3;
        assert_eq!(report.downloaded.len(), OBJECTS);
        ms
    };
    for _ in 0..warm_up {
        pull_ms();
    }
    let timed: Vec<Vec<f64>> = (0..batches).map(|_| (0..repeats).map(|_| pull_ms()).collect()).collect();
    let (p50, p95) = contextful_eval::trend::median_batch(&timed).unwrap();
    eprintln!("a cold pull of {OBJECTS} objects at 2 ms per get: p50 {p50:.1} ms, p95 {p95:.1} ms");
    assert!(p50 > 0.0 && p95 >= p50);
    contextful_eval::record::emit("sync-cold-pull-wall-p50", p50, repeats as u64, SEED);
    contextful_eval::record::emit("sync-cold-pull-wall-p95", p95, repeats as u64, SEED);
}

/// Objects in the cold-pull fixture, the size of a deployed store whose `_runs` records dominate.
const COLD_PULL_OBJECTS: usize = 7_700;
/// The seed the cold-pull fixture's figures record against.
const COLD_PULL_SEED: u64 = 0x5eed_0643;

/// A cold pull over a bucket holding each request for 2 ms keeps 16 fetches in flight and
/// lands every object; it reads each object once.
// spec: store.pull.transfer-width@baaf5f7a
#[test]
fn a_cold_pull_keeps_sixteen_fetches_in_flight_and_reads_each_object_once() {
    let dir = tempfile::tempdir().unwrap();
    let inner = bucket(dir.path());
    let keys = crate::support::seed_objects(inner.as_ref(), COLD_PULL_OBJECTS);
    let slow = Arc::new(crate::support::Slow::new(inner, std::time::Duration::from_millis(2)));
    let cold = node("ingest-b", slow.clone(), "");
    let started = std::time::Instant::now();
    let report = cold.syncer.pull(&PullScope::default()).unwrap();
    let wall = started.elapsed().as_secs_f64();
    assert_eq!(report.downloaded.len(), keys.len());
    assert!(keys.iter().all(|k| cold.root().join(k.strip_prefix("research/").unwrap()).exists()));
    let peak = slow.peak.load(Ordering::SeqCst);
    assert_eq!(peak, 16, "requests in flight at once");
    let gets = slow.gets.load(Ordering::SeqCst);
    let per_object = gets as f64 / keys.len() as f64;
    eprintln!("cold pull: {} objects, {gets} gets, peak {peak} in flight, {wall:.2} s", keys.len());
    contextful_eval::record::emit("sync-cold-pull-requests-per-object", per_object, keys.len() as u64, COLD_PULL_SEED);
    contextful_eval::record::emit("sync-cold-pull-wall", wall, keys.len() as u64, COLD_PULL_SEED);
    assert!(per_object <= 1.01, "{gets} gets for {} objects", keys.len());
}

/// A pull that meets a tampered object keeps every other object it verified.
#[test]
fn a_refused_concurrent_pull_keeps_each_object_it_verified() {
    let dir = tempfile::tempdir().unwrap();
    let inner = bucket(dir.path());
    let keys = crate::support::seed_objects(inner.as_ref(), 64);
    let tampered = format!("team/{}", keys[40]);
    let scripted = Arc::new(Scripted {
        inner,
        script: Script { on_get: Some(Box::new(move |k| (k == tampered).then(|| Ok(Some((b"tampered".to_vec(), "\"t\"".into())))))), ..Script::default() },
    });
    let cold = node("ingest-b", scripted, "");
    let err = cold.syncer.pull(&PullScope::default()).unwrap_err();
    assert!(matches!(err, SyncError::Store(StoreError::SyncObjectDigestMismatch(_))), "{err}");
    assert!(!cold.root().join(keys[40].strip_prefix("research/").unwrap()).exists());
    for key in keys.iter().filter(|k| **k != keys[40]) {
        let path = cold.root().join(key.strip_prefix("research/").unwrap());
        if let Ok(bytes) = std::fs::read(&path) {
            assert!(bytes.starts_with(b"{\"run\":"), "`{key}` landed verified");
        }
    }
}

/// A pull reads no local file whose size, modification time, change time and inode match its
/// digest record, and a rewrite restoring the size and modification time still re-downloads.
// spec: store.pull.digest-record@4a0497aa
#[test]
fn a_pull_takes_a_matching_files_digest_from_its_record() {
    let dir = tempfile::tempdir().unwrap();
    let b = bucket(dir.path());
    let keys = crate::support::seed_objects(b.as_ref(), 4);
    let cold = node("ingest-b", b, "");
    cold.syncer.pull(&PullScope::default()).unwrap();
    let again = cold.syncer.pull(&PullScope::default()).unwrap();
    assert_eq!((again.bytes_read, again.bytes_hashed), (0, 0), "every record stands for its file");

    let path = cold.root().join(keys[0].strip_prefix("research/").unwrap());
    let original = std::fs::read(&path).unwrap();
    let modified = std::fs::metadata(&path).unwrap().modified().unwrap();
    let forged: Vec<u8> = original.iter().map(|b| if *b == b'0' { b'9' } else { *b }).collect();
    assert_ne!(forged, original);
    std::fs::write(&path, &forged).unwrap();
    std::fs::File::options().write(true).open(&path).unwrap().set_modified(modified).unwrap();
    assert_eq!(std::fs::metadata(&path).unwrap().modified().unwrap(), modified);
    let report = cold.syncer.pull(&PullScope::default()).unwrap();
    assert_eq!(report.downloaded, vec![keys[0].clone()], "the rewrite moved the change time");
    assert_eq!(std::fs::read(&path).unwrap(), original);
}
