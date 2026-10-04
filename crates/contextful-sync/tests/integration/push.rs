//! `store.push`, `store.probe` and `store.merge` through the syncer.

use crate::support::{at, bucket, node, Script, Scripted};
use contextful_core::store::object::{Condition, ObjectError, ObjectStore, Put};
use contextful_core::store::sync::{generation_key, BucketManifest, Coordination};
use contextful_core::store::StoreError;
use contextful_sync::{FsBucket, SyncError, VolumeClass};
use serde_json::json;
use std::sync::Arc;
use contextful_core::issue::SignatureAlgorithm;
use contextful_core::surface::control::{receipt_file, snapshot_file, POINTER_FILE as CONTROL_POINTER};
use contextful_policy::control_receipt::ControlReceipt;
use contextful_policy::issue::SeedSigner;

const NOW: &str = "2030-01-01T01:00:00Z";

fn manifest(b: &dyn ObjectStore) -> BucketManifest {
    serde_json::from_slice(&b.get("team/manifest.json").unwrap().unwrap().0).unwrap()
}

fn applied_control(node: &crate::support::Node, version: u64, parent: Option<&str>, body: &str) -> ControlReceipt {
    let dir = node.syncer.control_dir.as_ref().unwrap();
    std::fs::create_dir_all(dir).unwrap();
    let signer = SeedSigner::generate(SignatureAlgorithm::Ed25519);
    let receipt = ControlReceipt::sign(&node.syncer.project, version, parent, body.as_bytes(), &signer).unwrap();
    std::fs::write(dir.join(snapshot_file(version)), body).unwrap();
    std::fs::write(dir.join(receipt_file(version)), serde_json::to_vec(&receipt).unwrap()).unwrap();
    std::fs::write(dir.join(CONTROL_POINTER), format!("{version}\n")).unwrap();
    receipt
}

/// A push commits each signed snapshot, receipt and project head under the bucket prefix.
// spec: store.push.control-artifact@ee79730f
#[test]
fn a_push_commits_the_signed_control_chain_and_project_head() {
    let dir = tempfile::tempdir().unwrap();
    let b = bucket(dir.path());
    let a = node("ingest-a", b.clone(), "");
    let first = applied_control(&a, 1, None, "[pipeline]\nname = 'one'\n");
    a.syncer.push(at(NOW)).unwrap();
    let second = applied_control(&a, 2, Some(&first.digest()), "[pipeline]\nname = 'two'\n");
    a.syncer.push(at(NOW)).unwrap();
    let m = manifest(b.as_ref());
    assert_eq!(m.control_heads["research"].receipt_sha256, second.digest());
    assert_eq!(m.control_heads["research"].version, 2);
    for version in [1, 2] {
        for name in [snapshot_file(version), receipt_file(version)] {
            let key = format!("research/control/{name}");
            assert!(m.entries.contains_key(&key), "{key}");
            assert!(b.get(&format!("team/{key}")).unwrap().is_some(), "{key}");
        }
    }
}

/// A sibling head raises `SyncControlDiverged` and keeps the bucket's signed head.
// spec: store.push.control-diverged@cbc7c52d
#[test]
fn a_sibling_control_head_refuses_without_changing_the_bucket_head() {
    let dir = tempfile::tempdir().unwrap();
    let b = bucket(dir.path());
    let a = node("ingest-a", b.clone(), "");
    let c = node("ingest-b", b.clone(), "");
    let first = applied_control(&a, 1, None, "first");
    a.syncer.push(at(NOW)).unwrap();
    applied_control(&c, 1, None, "sibling");
    match c.syncer.push(at(NOW)) {
        Err(SyncError::Store(StoreError::SyncControlDiverged(message))) => {
            assert!(message.contains(&first.digest()), "{message}");
        }
        other => panic!("{other:?}"),
    }
    assert_eq!(manifest(b.as_ref()).control_heads["research"].receipt_sha256, first.digest());
}

#[test]
fn a_skipped_local_version_still_uploads_its_signed_predecessor() {
    let dir = tempfile::tempdir().unwrap();
    let b = bucket(dir.path());
    let a = node("ingest-a", b.clone(), "");
    let first = applied_control(&a, 1, None, "first");
    applied_control(&a, 2, Some(&first.digest()), "orphan");
    applied_control(&a, 3, Some(&first.digest()), "third");
    a.syncer.push(at(NOW)).unwrap();
    let m = manifest(b.as_ref());
    assert_eq!(m.control_heads["research"].version, 3);
    assert!(m.entries.contains_key("research/control/receipt@v1.json"));
    assert!(m.entries.contains_key("research/control/receipt@v3.json"));
    assert!(!m.entries.contains_key("research/control/receipt@v2.json"));
}

#[test]
fn a_receipt_under_the_wrong_version_name_is_not_a_predecessor() {
    let dir = tempfile::tempdir().unwrap();
    let b = bucket(dir.path());
    let a = node("ingest-a", b.clone(), "");
    let first = applied_control(&a, 1, None, "first");
    let control = a.syncer.control_dir.as_ref().unwrap();
    std::fs::write(control.join(snapshot_file(2)), "first").unwrap();
    std::fs::copy(control.join(receipt_file(1)), control.join(receipt_file(2))).unwrap();
    applied_control(&a, 3, Some(&first.digest()), "third");
    a.syncer.push(at(NOW)).unwrap();
    let m = manifest(b.as_ref());
    assert!(m.entries.contains_key("research/control/receipt@v1.json"));
    assert!(m.entries.contains_key("research/control/receipt@v3.json"));
    assert!(!m.entries.contains_key("research/control/receipt@v2.json"));
}

#[test]
fn a_corrupted_control_receipt_prevents_manifest_publication() {
    let dir = tempfile::tempdir().unwrap();
    let b = bucket(dir.path());
    let a = node("ingest-a", b.clone(), "");
    applied_control(&a, 1, None, "first");
    let receipt = a.syncer.control_dir.as_ref().unwrap().join(receipt_file(1));
    let mut value: serde_json::Value = serde_json::from_slice(&std::fs::read(&receipt).unwrap()).unwrap();
    value["signature"] = json!("00");
    std::fs::write(receipt, serde_json::to_vec(&value).unwrap()).unwrap();
    assert!(matches!(a.syncer.push(at(NOW)), Err(SyncError::Surface(_))));
    assert!(b.get("team/manifest.json").unwrap().is_none());
}

/// A push uploads each file it owns, or no node owns, whose digest the bucket lacks under
/// `<prefix>/<project>/<path>` by a conditional put; machine catalogs, `config.toml`, locks and staging directories
/// stay local, and table pointers travel by {{store.push.pointer-carry}}.
// spec: store.push.wire-format@cc766ab8
#[test]
fn a_push_uploads_store_files_under_the_prefix_and_keeps_machine_state_local() {
    let dir = tempfile::tempdir().unwrap();
    let b = bucket(dir.path());
    let a = node("ingest-a", b.clone(), "");
    a.land("run-1", json!([{"id": 1}]), "2030-01-01T00:00:00Z");
    std::fs::write(a.root().join("machine.sqlite"), "local").unwrap();
    std::fs::write(a.root().join("tables/filings/_pointer.json"), "{}").unwrap();
    let report = a.syncer.push(crate::support::at(NOW)).unwrap();
    let keys = b.list("team/").unwrap();
    assert!(keys.contains(&"team/research/tables/filings/schema.json".to_string()), "{keys:?}");
    assert!(keys.contains(&"team/research/tables/filings/data/runs/run-1/ingest-a/part-00000.parquet".to_string()));
    for local in ["config.toml", "machine.sqlite", "_pointer.json", ".lock", ".push.lock"] {
        assert!(!keys.iter().any(|k| k.ends_with(local)), "{local} left the machine: {keys:?}");
    }
    // A second push uploads nothing whose digest the bucket already lists.
    assert!(!report.uploaded.is_empty());
    assert!(a.syncer.push(at(NOW)).unwrap().uploaded.is_empty());
}

/// SQLite writes a rollback journal or a write-ahead log beside each catalog while a
/// transaction runs; those files are the catalog's own state and stay on the machine too.
#[test]
fn a_push_keeps_the_catalogs_transaction_files_local() {
    let dir = tempfile::tempdir().unwrap();
    let b = bucket(dir.path());
    let a = node("ingest-a", b.clone(), "");
    a.land("run-1", json!([{"id": 1}]), "2030-01-01T00:00:00Z");
    let sidecars = ["machine.sqlite-journal", "machine.sqlite-wal", "machine.sqlite-shm", "derived.sqlite-journal", "derived.sqlite-wal"];
    for f in ["derived.sqlite"].iter().chain(sidecars.iter()) {
        std::fs::write(a.root().join(f), "local").unwrap();
    }
    a.syncer.push(at(NOW)).unwrap();
    let keys = b.list("team/").unwrap();
    for local in ["derived.sqlite"].iter().chain(sidecars.iter()) {
        assert!(!keys.iter().any(|k| k.ends_with(local)), "{local} left the machine: {keys:?}");
    }
    assert!(keys.iter().any(|k| k.ends_with("part-00000.parquet")), "{keys:?}");
}

/// A push commits when the bucket manifest, `<prefix>/manifest.json` listing each key's sha256, size and owner,
/// replaces the copy it read under `If-Match` on that copy's ETag.
// spec: store.push.manifest-commit@77fc6a34
#[test]
fn the_manifest_commits_by_replace_on_the_etag_read() {
    let dir = tempfile::tempdir().unwrap();
    let b = bucket(dir.path());
    let (a, c) = (node("ingest-a", b.clone(), ""), node("ingest-b", b.clone(), ""));
    a.land("run-1", json!([{"id": 1}]), "2030-01-01T00:00:00Z");
    c.land("run-1", json!([{"id": 2}]), "2030-01-01T00:00:00Z");
    // Both push at once; each commit that loses its ETag re-reads and re-merges.
    std::thread::scope(|s| {
        let ha = s.spawn(|| a.syncer.push(at(NOW)).unwrap());
        let hc = s.spawn(|| c.syncer.push(at(NOW)).unwrap());
        ha.join().unwrap();
        hc.join().unwrap();
    });
    let m = manifest(b.as_ref());
    let run_a = m.entries.get("research/tables/filings/data/runs/run-1/ingest-a/part-00000.parquet").unwrap();
    assert_eq!(run_a.owner, "ingest-a");
    assert!(m.entries.contains_key("research/tables/filings/data/runs/run-1/ingest-b/part-00000.parquet"));
    assert!(run_a.size > 0 && run_a.sha256.len() == 64);
}

/// A second push of one store on one machine raises `SyncPushInFlight`, naming the holder of the push guard.
// spec: store.push.in-flight@d101bdd0
#[test]
fn a_second_push_of_one_store_refuses_while_the_first_runs() {
    let dir = tempfile::tempdir().unwrap();
    let (gate_tx, gate_rx) = std::sync::mpsc::channel::<()>();
    let (entered_tx, entered_rx) = std::sync::mpsc::channel::<()>();
    let gate = std::sync::Mutex::new(Some(gate_rx));
    let entered = std::sync::Mutex::new(Some(entered_tx));
    let slow: Arc<dyn ObjectStore> = Arc::new(Scripted {
        inner: bucket(dir.path()),
        script: Script {
            on_get: Some(Box::new(move |key: &str| {
                if key.ends_with("manifest.json") {
                    if let Some(tx) = entered.lock().unwrap().take() {
                        tx.send(()).unwrap();
                        gate.lock().unwrap().take().unwrap().recv().unwrap();
                    }
                }
                None
            })),
            ..Script::default()
        },
    });
    let a = node("ingest-a", slow, "");
    a.land("run-1", json!([{"id": 1}]), "2030-01-01T00:00:00Z");
    std::thread::scope(|s| {
        let first = s.spawn(|| a.syncer.push(at(NOW)));
        entered_rx.recv().unwrap();
        match a.syncer.push(at(NOW)) {
            Err(SyncError::Store(StoreError::SyncPushInFlight(m))) => assert!(m.contains(&std::process::id().to_string()), "{m}"),
            other => panic!("{other:?}"),
        }
        gate_tx.send(()).unwrap();
        first.join().unwrap().unwrap();
    });
}

/// The probe creates a sentinel under `_contextful/cas-probe/` and demonstrates `cas` when a second create and a
/// stale `If-Match` both fail and a current `If-Match` replaces it, deleting the sentinel after.
// spec: store.probe.sentinel@988a8bb4
#[test]
fn the_probe_demonstrates_cas_and_leaves_no_sentinel() {
    let dir = tempfile::tempdir().unwrap();
    let b = bucket(dir.path());
    let a = node("ingest-a", b.clone(), "");
    assert_eq!(a.syncer.probe().unwrap(), contextful_core::store::sync::Coordination::Cas);
    assert!(b.list("team/_contextful/cas-probe/").unwrap().is_empty());
    let ignoring: Arc<dyn ObjectStore> = Arc::new(Scripted { inner: b, script: Script { ignore_conditions: true, ..Script::default() } });
    let c = node("ingest-b", ignoring, "");
    assert_eq!(c.syncer.probe().unwrap(), contextful_core::store::sync::Coordination::SingleWriter);
}

/// An unsupported-method response, a forbidden response or a transport error raises `SyncProbeInconclusive` and
/// counts as capability not demonstrated.
// spec: store.probe.inconclusive@881cfb99
#[test]
fn a_refused_method_credential_or_transport_is_inconclusive() {
    for err in [ObjectError::Unsupported("If-None-Match".into()), ObjectError::Forbidden("403".into()), ObjectError::Transport("reset".into())] {
        let dir = tempfile::tempdir().unwrap();
        let e = err.clone();
        let b: Arc<dyn ObjectStore> = Arc::new(Scripted { inner: bucket(dir.path()), script: Script { on_put: Some(Box::new(move |_, _| Some(Err(e.clone())))), ..Script::default() } });
        let a = node("ingest-a", b, "");
        assert!(matches!(a.syncer.probe(), Err(SyncError::Store(StoreError::SyncProbeInconclusive(_)))), "{err}");
    }
}

/// Declaring `cas` against a backend the probe did not demonstrate raises `SyncCoordinationUnproven` and stops
/// the push.
// spec: store.probe.unproven@542cacbe
#[test]
fn a_declared_cas_against_an_undemonstrated_backend_stops_the_push() {
    let dir = tempfile::tempdir().unwrap();
    let inner = bucket(dir.path());
    let ignoring: Arc<dyn ObjectStore> = Arc::new(Scripted { inner: inner.clone(), script: Script { ignore_conditions: true, ..Script::default() } });
    let a = node("ingest-a", ignoring, "");
    a.land("run-1", json!([{"id": 1}]), "2030-01-01T00:00:00Z");
    assert!(matches!(a.syncer.push(at(NOW)), Err(SyncError::Store(StoreError::SyncCoordinationUnproven(_)))));
    assert!(inner.list("team/research/").unwrap().is_empty(), "nothing uploaded");
}

/// A filesystem bucket whose mount type is not apfs, hfs, ext4, xfs, btrfs, zfs, tmpfs or overlay resolves
/// `single-writer` without the sentinel, and a declared `cas` there meets `store.probe.unproven` naming the mount type.
// spec: store.probe.network-volume@ae257e4e
#[test]
fn a_bucket_on_a_network_volume_resolves_single_writer_and_refuses_a_declared_cas() {
    let dir = tempfile::tempdir().unwrap();
    let share = FsBucket::open_with_volume(dir.path(), "context-team", VolumeClass::Network("smbfs".into())).unwrap();
    assert_eq!(share.volume(), &VolumeClass::Network("smbfs".into()));
    let share: Arc<dyn ObjectStore> = Arc::new(share);
    let a = node("ingest-a", share.clone(), "");
    let (coordination, why) = a.syncer.probe_with_reason().unwrap();
    assert_eq!(coordination, Coordination::SingleWriter);
    assert!(why.contains("smbfs"), "the probe names the mount type: {why}");
    a.land("run-1", json!([{"id": 1}]), "2030-01-01T00:00:00Z");
    match a.syncer.push(at(NOW)) {
        Err(SyncError::Store(StoreError::SyncCoordinationUnproven(m))) => assert!(m.contains("smbfs"), "{m}"),
        other => panic!("a declared cas on a network share proceeded: {other:?}"),
    }
    assert!(share.list("team/research/").unwrap().is_empty(), "nothing uploaded");

    // The same bucket on a local volume still demonstrates `cas` and pushes.
    let dir = tempfile::tempdir().unwrap();
    let local: Arc<dyn ObjectStore> = Arc::new(FsBucket::open_with_volume(dir.path(), "context-team", VolumeClass::Local("apfs".into())).unwrap());
    let b = node("ingest-a", local, "");
    assert_eq!(b.syncer.probe().unwrap(), Coordination::Cas);
    b.land("run-1", json!([{"id": 1}]), "2030-01-01T00:00:00Z");
    assert!(!b.syncer.push(at(NOW)).unwrap().uploaded.is_empty());
}

/// A compaction lease on a network share refuses as `store.probe.unproven` naming the mount type,
/// writing no lease object and no pointer fence; a local volume grants it.
// spec: store.lease.network-volume@1d922578
#[test]
fn a_compaction_lease_on_a_network_volume_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let share: Arc<dyn ObjectStore> = Arc::new(FsBucket::open_with_volume(dir.path(), "context-team", VolumeClass::Network("nfs".into())).unwrap());
    let a = node("ingest-a", share.clone(), "");
    match a.syncer.acquire("filings", at(NOW)) {
        Err(SyncError::Store(StoreError::SyncCoordinationUnproven(m))) => assert!(m.contains("nfs"), "{m}"),
        other => panic!("a lease on a network share was granted: {other:?}"),
    }
    assert!(share.list("").unwrap().is_empty(), "no lease object and no pointer written");
    assert!(a.syncer.held("filings").unwrap().is_none(), "nothing recorded as held");

    let dir = tempfile::tempdir().unwrap();
    let local: Arc<dyn ObjectStore> = Arc::new(FsBucket::open_with_volume(dir.path(), "context-team", VolumeClass::Local("ext4".into())).unwrap());
    let b = node("ingest-a", local, "");
    assert_eq!(b.syncer.acquire("filings", at(NOW)).unwrap().lease.fence, 1);
}

/// The local allowlist decides a mount type: anything off it, a FUSE or unknown type included, is a network volume.
#[test]
fn a_mount_type_off_the_local_allowlist_classifies_as_network() {
    for local in ["apfs", "hfs", "ext4", "xfs", "btrfs", "zfs", "tmpfs", "overlay"] {
        assert_eq!(VolumeClass::of(local), VolumeClass::Local(local.into()));
    }
    for remote in ["smbfs", "nfs", "afpfs", "webdav", "cifs", "fuse", "macfuse", "9p", "unknown"] {
        assert_eq!(VolumeClass::of(remote), VolumeClass::Network(remote.into()));
    }
}

/// `[sync] push_retries` bounds the re-commit loop, defaulting to 5 attempts.
// spec: store.merge.retries@84dd83ad
#[test]
fn the_re_commit_loop_takes_five_rounds_by_default_and_the_declared_bound_otherwise() {
    assert_eq!(contextful_core::store::sync::PUSH_RETRIES, 5);
    for (declared, expected) in [(None, 5usize), (Some(2), 2)] {
        let dir = tempfile::tempdir().unwrap();
        let rounds = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let r = rounds.clone();
        let losing: Arc<dyn ObjectStore> = Arc::new(Scripted {
            inner: bucket(dir.path()),
            script: Script {
                on_put: Some(Box::new(move |key, _| {
                    key.ends_with("/manifest.json").then(|| {
                        r.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                        Ok(Put::ConditionFailed)
                    })
                })),
                ..Script::default()
            },
        });
        let mut a = node("ingest-a", losing, "");
        a.syncer.config.push_retries = declared;
        a.land("run-1", json!([{"id": 1}]), "2030-01-01T00:00:00Z");
        assert!(a.syncer.push(at(NOW)).is_err());
        assert_eq!(rounds.load(std::sync::atomic::Ordering::SeqCst), expected);
    }
}

/// Exhausting those retries raises `SyncManifestRebaseExhausted`, reports every uploaded object as already in the
/// bucket, and asks for a re-run.
// spec: store.merge.exhausted@1170c7b9
#[test]
fn exhausted_rounds_refuse_and_report_every_uploaded_object() {
    let dir = tempfile::tempdir().unwrap();
    let inner = bucket(dir.path());
    let losing: Arc<dyn ObjectStore> = Arc::new(Scripted {
        inner: inner.clone(),
        script: Script { on_put: Some(Box::new(|key, _| key.ends_with("/manifest.json").then_some(Ok(Put::ConditionFailed)))), ..Script::default() },
    });
    let a = node("ingest-a", losing, "");
    a.land("run-1", json!([{"id": 1}]), "2030-01-01T00:00:00Z");
    match a.syncer.push(at(NOW)) {
        Err(SyncError::Store(StoreError::SyncManifestRebaseExhausted(m))) => {
            assert!(m.contains("research/tables/filings/data/runs/run-1/ingest-a/part-00000.parquet") && m.contains("run the push again"), "{m}");
        }
        other => panic!("{other:?}"),
    }
    assert!(inner.get("team/research/tables/filings/data/runs/run-1/ingest-a/part-00000.parquet").unwrap().is_some(), "the object is durable");
}

const POINTER: &str = "team/research/tables/filings/_pointer.json";

fn bucket_pointer(b: &dyn ObjectStore) -> Option<serde_json::Value> {
    b.get(POINTER).unwrap().map(|(bytes, _)| serde_json::from_slice(&bytes).unwrap())
}

fn folded(n: &crate::support::Node, now: &str) -> String {
    use contextful_core::store::fold::FoldOutcome;
    let decl = contextful_core::store::declare::TableDecl::named("filings");
    let FoldOutcome::Folded { snapshot_id, .. } = contextful_context::fold::fold(&n.syncer.store, &decl, at(now)).unwrap() else { panic!("nothing folded") };
    snapshot_id
}

fn files(n: &crate::support::Node) -> Vec<String> {
    let decl = contextful_core::store::declare::TableDecl::named("filings");
    contextful_context::scan::scan(&n.syncer.store, &decl, Default::default()).unwrap().files
}

/// After its manifest commit, a push publishes each local table pointer whose snapshot is whole and whose
/// {{store.lay-out.ancestors}} name the bucket pointer's, or the bucket pointer names none, by a conditional put
/// keeping the bucket's fence.
// spec: store.push.pointer-carry@1a90af7f
#[test]
fn a_push_carries_a_local_folds_pointer_and_a_second_node_reads_its_snapshot() {
    let dir = tempfile::tempdir().unwrap();
    let b = bucket(dir.path());
    let a = node("ingest-a", b.clone(), "");
    a.land("run-1", json!([{"id": 1}, {"id": 2}]), "2030-01-01T00:00:00Z");
    let first = folded(&a, NOW);
    let report = a.syncer.push(at(NOW)).unwrap();
    assert_eq!(report.pointers, ["research/tables/filings/_pointer.json"]);
    assert_eq!(bucket_pointer(b.as_ref()).unwrap(), json!({"snapshot_id": first, "fence": 0}));
    let m = manifest(b.as_ref());
    assert_eq!(m.pointers["research/tables/filings/_pointer.json"].snapshot_id.as_deref(), Some(first.as_str()), "the generation names the carried pointer");
    assert_eq!((report.generation, m.generation), (2, 2), "a second commit records the published pointer");
    assert_eq!(generation_pointer(b.as_ref(), 1), None, "the first commit precedes the publish");
    assert_eq!(generation_pointer(b.as_ref(), 2), Some(first.clone()));
    // A second push carries nothing the bucket already names.
    assert!(a.syncer.push(at(NOW)).unwrap().pointers.is_empty());

    // A cold node pulls the pointer with the snapshot it names.
    let c = node("ingest-b", b.clone(), "");
    let pulled = c.syncer.pull(&contextful_sync::PullScope::default()).unwrap();
    assert_eq!(pulled.pointers, ["research/tables/filings/_pointer.json"]);
    assert_eq!(files(&c), [format!("tables/filings/data/snapshots/{first}/part-00000.parquet")]);

    // A later fold descends from the published snapshot and replaces it under the bucket's fence.
    a.land("run-2", json!([{"id": 3}]), "2030-01-01T02:00:00Z");
    let second = folded(&a, "2030-01-01T03:00:00Z");
    assert_eq!(a.syncer.push(at("2030-01-01T03:00:00Z")).unwrap().pointers.len(), 1);
    assert_eq!(bucket_pointer(b.as_ref()).unwrap()["snapshot_id"], json!(second));

    // A snapshot descending from none the bucket names stays local.
    let d = node("ingest-d", b.clone(), "");
    d.land("run-9", json!([{"id": 9}]), "2030-01-01T04:00:00Z");
    folded(&d, "2030-01-01T05:00:00Z");
    assert!(d.syncer.push(at("2030-01-01T05:00:00Z")).unwrap().pointers.is_empty());
    assert_eq!(bucket_pointer(b.as_ref()).unwrap()["snapshot_id"], json!(second));
}

/// A push publishes no pointer for a table whose compaction lease a holder keeps unexpired; that holder publishes
/// under its fence.
// spec: store.push.pointer-leased@3cb569bd
#[test]
fn a_push_leaves_a_leased_tables_pointer_to_the_lease_holder() {
    let dir = tempfile::tempdir().unwrap();
    let b = bucket(dir.path());
    let (a, c) = (node("ingest-a", b.clone(), ""), node("ingest-b", b.clone(), ""));
    a.land("run-1", json!([{"id": 1}]), "2030-01-01T00:00:00Z");
    let held = c.syncer.acquire("filings", at(NOW)).unwrap();
    let snapshot = folded(&a, NOW);
    assert!(a.syncer.push(at(NOW)).unwrap().pointers.is_empty());
    assert_eq!(bucket_pointer(b.as_ref()).unwrap(), json!({"snapshot_id": null, "fence": held.lease.fence}));
    // Released, the lease no longer reserves the publish, and the bucket's fence stays.
    c.syncer.release("filings").unwrap();
    assert_eq!(a.syncer.push(at(NOW)).unwrap().pointers.len(), 1);
    assert_eq!(bucket_pointer(b.as_ref()).unwrap(), json!({"snapshot_id": snapshot, "fence": held.lease.fence}));
}

/// A local pointer carrying a fence below the bucket pointer's stays local, and the push reports it as a warning
/// beside its commit.
// spec: store.push.pointer-fenced@b95542a0
#[test]
fn a_local_pointer_under_a_superseded_fence_stays_local() {
    let dir = tempfile::tempdir().unwrap();
    let b = bucket(dir.path());
    let (a, c) = (node("ingest-a", b.clone(), ""), node("ingest-b", b.clone(), ""));
    a.land("run-1", json!([{"id": 1}]), "2030-01-01T00:00:00Z");
    let snapshot = folded(&a, NOW);
    a.syncer.push(at(NOW)).unwrap();
    // A pointer published under fence 1 once a later acquisition raised the bucket's to 2.
    let first = c.syncer.acquire("filings", at(NOW)).unwrap();
    c.syncer.release("filings").unwrap();
    c.syncer.acquire("filings", at(NOW)).unwrap();
    c.syncer.release("filings").unwrap();
    a.land("run-2", json!([{"id": 2}]), "2030-01-01T02:00:00Z");
    let later = folded(&a, "2030-01-01T03:00:00Z");
    let local = json!({"snapshot_id": later, "fence": first.lease.fence});
    std::fs::write(a.root().join("tables/filings/_pointer.json"), serde_json::to_vec(&local).unwrap()).unwrap();
    let report = a.syncer.push(at("2030-01-01T03:00:00Z")).unwrap();
    assert!(report.pointers.is_empty());
    assert!(report.refused.iter().any(|w| w.contains("research/tables/filings/_pointer.json") && w.contains("fence")), "{:?}", report.refused);
    assert_eq!(bucket_pointer(b.as_ref()).unwrap(), json!({"snapshot_id": snapshot, "fence": first.lease.fence + 1}));
}

fn generation(b: &dyn ObjectStore, n: u64) -> BucketManifest {
    serde_json::from_slice(&b.get(&format!("team/{}", generation_key(n))).unwrap().unwrap().0).unwrap()
}

fn generation_pointer(b: &dyn ObjectStore, n: u64) -> Option<String> {
    generation(b, n).pointers.get("research/tables/filings/_pointer.json").and_then(|p| p.snapshot_id.clone())
}

/// Fold twice between pushes, then let retention collect every superseded snapshot, leaving `a` only the newest.
fn fold_twice_past_retention(a: &crate::support::Node) -> String {
    a.land("run-2", json!([{"id": 2}]), "2030-01-01T02:00:00Z");
    folded(a, "2030-01-01T03:00:00Z");
    a.land("run-3", json!([{"id": 3}]), "2030-01-01T04:00:00Z");
    let newest = folded(a, "2030-01-01T05:00:00Z");
    let decl = contextful_core::store::declare::TableDecl::named("filings");
    contextful_context::fold::collect(&a.syncer.store, &decl, at(LATER)).unwrap();
    let snapshots: Vec<String> = std::fs::read_dir(a.root().join("tables/filings/data/snapshots")).unwrap().map(|e| e.unwrap().file_name().to_string_lossy().into_owned()).collect();
    assert_eq!(snapshots, std::slice::from_ref(&newest), "retention leaves only the newest snapshot");
    newest
}

const LATER: &str = "2030-01-20T00:00:00Z";

/// A snapshot's `_manifest.json` carries `ancestors`, so a push decides descent with every superseded snapshot
/// collected.
#[test]
fn a_pointer_carries_across_folds_whose_snapshots_retention_collected() {
    let dir = tempfile::tempdir().unwrap();
    let b = bucket(dir.path());
    let a = node("ingest-a", b.clone(), "");
    a.land("run-1", json!([{"id": 1}]), "2030-01-01T00:00:00Z");
    folded(&a, NOW);
    assert_eq!(a.syncer.push(at(NOW)).unwrap().pointers.len(), 1);
    let newest = fold_twice_past_retention(&a);
    let report = a.syncer.push(at(LATER)).unwrap();
    assert_eq!(report.pointers, ["research/tables/filings/_pointer.json"], "{:?}", report.refused);
    assert_eq!(bucket_pointer(b.as_ref()).unwrap()["snapshot_id"], json!(newest));
    // A cold node reads every folded row.
    let c = node("ingest-e", b.clone(), "");
    c.syncer.pull(&contextful_sync::PullScope::default()).unwrap();
    assert_eq!(files(&c), [format!("tables/filings/data/snapshots/{newest}/part-00000.parquet")]);
}

/// A local pointer whose snapshot's ancestry ends at a collected manifest before reaching the bucket pointer's snapshot
/// or a root stays local, and the push reports it as a warning beside its commit.
// spec: store.push.pointer-unrooted@a3860765
#[test]
fn a_pointer_whose_ancestry_retention_cut_stays_local_with_a_warning() {
    let dir = tempfile::tempdir().unwrap();
    let b = bucket(dir.path());
    let a = node("ingest-a", b.clone(), "");
    a.land("run-1", json!([{"id": 1}]), "2030-01-01T00:00:00Z");
    let first = folded(&a, NOW);
    a.syncer.push(at(NOW)).unwrap();
    let newest = fold_twice_past_retention(&a);
    // A manifest written without `ancestors` reaches back by `parent` alone, into a collected snapshot.
    let path = a.root().join("tables/filings/data/snapshots").join(&newest).join("_manifest.json");
    let mut m: serde_json::Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    m.as_object_mut().unwrap().remove("ancestors");
    std::fs::write(&path, serde_json::to_vec(&m).unwrap()).unwrap();
    let report = a.syncer.push(at(LATER)).unwrap();
    assert!(report.pointers.is_empty());
    assert!(report.refused.iter().any(|w| w.contains("research/tables/filings/_pointer.json") && w.contains(&first)), "{:?}", report.refused);
    assert_eq!(bucket_pointer(b.as_ref()).unwrap()["snapshot_id"], json!(first));
}

/// A push whose pointer publish the bucket took commits the manifest again, so a generation names a carried pointer
/// only once the bucket's pointer holds it.
// spec: store.push.pointer-recommit@d2222ae2
#[test]
fn a_generation_names_no_pointer_the_bucket_declined() {
    let dir = tempfile::tempdir().unwrap();
    let inner = bucket(dir.path());
    let raced = inner.clone();
    let once = std::sync::Mutex::new(true);
    // Another node publishes an unrelated snapshot between the manifest commit and the pointer's publish.
    let b: Arc<dyn ObjectStore> = Arc::new(Scripted {
        inner: inner.clone(),
        script: Script {
            on_put: Some(Box::new(move |key, _| {
                if key.ends_with("/manifest.json") && std::mem::take(&mut *once.lock().unwrap()) {
                    let foreign = json!({"snapshot_id": "snapshot-09999999999999999999", "fence": 0});
                    raced.put(POINTER, &serde_json::to_vec(&foreign).unwrap(), Condition::None).unwrap();
                }
                None
            })),
            ..Script::default()
        },
    });
    let a = node("ingest-a", b.clone(), "");
    a.land("run-1", json!([{"id": 1}]), "2030-01-01T00:00:00Z");
    let snapshot = folded(&a, NOW);
    let report = a.syncer.push(at(NOW)).unwrap();
    assert!(report.pointers.is_empty());
    assert_eq!(report.generation, 1);
    assert_ne!(generation_pointer(inner.as_ref(), 1), Some(snapshot.clone()), "gen-1 names no pointer the bucket declined");
    let live = manifest(inner.as_ref()).pointers.get("research/tables/filings/_pointer.json").and_then(|p| p.snapshot_id.clone());
    assert_ne!(live, Some(snapshot), "the live manifest names no pointer the bucket declined");
}
