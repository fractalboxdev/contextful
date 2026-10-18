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
        assert!(!keys.is_empty(), "the exclusion below ranges over no element");
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

/// A push of one store waits for the push guard another push on the machine holds, up to `[sync] push_wait_secs`,
/// defaulting to 300 s, then raises `SyncPushInFlight`, naming the holder. A push whose wait outlasts the holder
/// commits after it.
// spec: store.push.in-flight@60525ef6
#[test]
fn a_second_push_of_one_store_waits_for_the_first_then_refuses_past_its_bound() {
    assert_eq!(contextful_core::store::sync::PUSH_WAIT_SECS, 300);
    let dir = tempfile::tempdir().unwrap();
    let (gate_tx, gate_rx) = std::sync::mpsc::channel::<()>();
    let (entered_tx, entered_rx) = std::sync::mpsc::channel::<()>();
    let gate = std::sync::Mutex::new(Some(gate_rx));
    let entered = std::sync::Mutex::new(Some(entered_tx));
    let inner = bucket(dir.path());
    let slow: Arc<dyn ObjectStore> = Arc::new(Scripted {
        inner: inner.clone(),
        script: Script {
            on_get: Some(Box::new(move |key: &str| {
                if key.ends_with("manifest.json") {
                    if let Some(tx) = entered.lock().unwrap().take() {
                        tx.send(()).unwrap();
                        gate.lock().unwrap().take().unwrap().recv_timeout(std::time::Duration::from_secs(30)).expect("the holder is released");
                    }
                }
                None
            })),
            ..Script::default()
        },
    });
    let mut a = node("ingest-a", slow, "");
    a.syncer.config.push_wait_secs = Some(1);
    a.land("run-1", json!([{"id": 1}]), "2030-01-01T00:00:00Z");
    let mut patient = node("ingest-a", inner.clone(), "");
    patient.syncer.store = a.syncer.store.clone();
    patient.syncer.config.push_wait_secs = Some(30);
    std::thread::scope(|s| {
        let first = s.spawn(|| a.syncer.push(at(NOW)));
        entered_rx.recv_timeout(std::time::Duration::from_secs(30)).expect("the first push reads the manifest");
        let waited = std::time::Instant::now();
        match a.syncer.push(at(NOW)) {
            Err(SyncError::Store(StoreError::SyncPushInFlight(m))) => assert!(m.contains(&std::process::id().to_string()), "{m}"),
            other => panic!("{other:?}"),
        }
        assert!(waited.elapsed() >= std::time::Duration::from_secs(1), "the push waited its bound before refusing");
        let after = s.spawn(|| patient.syncer.push(at(NOW)));
        std::thread::sleep(std::time::Duration::from_millis(200));
        gate_tx.send(()).unwrap();
        let held = first.join().unwrap().unwrap();
        let next = after.join().unwrap().expect("a push waiting within its bound commits after the holder");
        assert!(next.generation > held.generation, "{held:?} then {next:?}");
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

/// A bucket on which another writer commits the next generation ahead of each of this writer's first `ahead`
/// manifest puts, writing its generation copy, so each of those puts loses to a newer generation. Returns the bucket
/// and the count of manifest puts this writer made.
fn overtaken(dir: &std::path::Path, ahead: usize) -> (Arc<dyn ObjectStore>, Arc<std::sync::atomic::AtomicUsize>) {
    interfered(dir, Box::new(move |put| (put < ahead).then_some(true)))
}

/// A bucket answering this writer's manifest put number `put` by `plan(put)`: `Some(true)` commits another writer's
/// next generation ahead of it, `Some(false)` fails it with no newer generation, and `None` lets it through. Returns
/// the bucket and the count of manifest puts this writer made.
fn interfered(dir: &std::path::Path, plan: Box<dyn Fn(usize) -> Option<bool> + Send + Sync>) -> (Arc<dyn ObjectStore>, Arc<std::sync::atomic::AtomicUsize>) {
    let inner = bucket(dir);
    let other = inner.clone();
    let puts = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let counted = puts.clone();
    let b: Arc<dyn ObjectStore> = Arc::new(Scripted {
        inner,
        script: Script {
            on_put: Some(Box::new(move |key, _| {
                if key != "team/manifest.json" {
                    return None;
                }
                match plan(counted.fetch_add(1, std::sync::atomic::Ordering::SeqCst)) {
                    None => return None,
                    Some(false) => return Some(Ok(Put::ConditionFailed)),
                    Some(true) => {}
                }
                let (mut m, condition) = match other.get(key).unwrap() {
                    Some((bytes, etag)) => (serde_json::from_slice::<BucketManifest>(&bytes).unwrap(), Condition::IfMatch(etag)),
                    None => (BucketManifest::default(), Condition::IfNoneMatch),
                };
                m.generation += 1;
                let bytes = serde_json::to_vec_pretty(&m).unwrap();
                assert!(matches!(other.put(key, &bytes, condition).unwrap(), Put::Applied(_)));
                other.put(&format!("team/{}", generation_key(m.generation)), &bytes, Condition::IfNoneMatch).unwrap();
                None
            })),
            ..Script::default()
        },
    });
    (b, puts)
}

/// `[sync] push_retries` bounds consecutive re-commit rounds whose re-read manifest carries no newer generation,
/// defaulting to 5 attempts.
// spec: store.merge.retries@15d8a962
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
    // Rounds lost to a newer generation count against no retry: seven of them, then a commit, under a bound of 2.
    let dir = tempfile::tempdir().unwrap();
    let (b, puts) = overtaken(dir.path(), 7);
    let mut a = node("ingest-a", b, "");
    a.syncer.config.push_retries = Some(2);
    a.land("run-1", json!([{"id": 1}]), "2030-01-01T00:00:00Z");
    let report = a.syncer.push(at(NOW)).unwrap();
    assert_eq!((report.rounds, report.generation), (8, 8), "{report:?}");
    assert_eq!(puts.load(std::sync::atomic::Ordering::SeqCst), 8);
    // Only consecutive rounds meeting no newer generation count: four such rounds, each after one that met a newer
    // generation, then a commit, under a bound of 2.
    let dir = tempfile::tempdir().unwrap();
    let (b, puts) = interfered(dir.path(), Box::new(|put| (put < 8).then_some(put % 2 == 0)));
    let mut a = node("ingest-a", b, "");
    a.syncer.config.push_retries = Some(2);
    a.land("run-1", json!([{"id": 1}]), "2030-01-01T00:00:00Z");
    let report = a.syncer.push(at(NOW)).expect("no two consecutive rounds meet no newer generation");
    assert_eq!((report.rounds, report.generation), (9, 5), "{report:?}");
    assert_eq!(puts.load(std::sync::atomic::Ordering::SeqCst), 9);
}

/// A push stops its manifest commit loop after 64 rounds, whatever generations those rounds meet.
// spec: store.merge.round-ceiling@bb637cc3
#[test]
fn a_push_overtaken_every_round_stops_at_sixty_four_rounds() {
    assert_eq!(contextful_core::store::sync::PUSH_ROUND_CEILING, 64);
    let dir = tempfile::tempdir().unwrap();
    let (b, puts) = overtaken(dir.path(), usize::MAX);
    let a = node("ingest-a", b, "");
    a.land("run-1", json!([{"id": 1}]), "2030-01-01T00:00:00Z");
    match a.syncer.push(at(NOW)) {
        Err(SyncError::Store(StoreError::SyncManifestRebaseExhausted(m))) => assert!(m.contains("lost 64 races"), "{m}"),
        other => panic!("{other:?}"),
    }
    assert_eq!(puts.load(std::sync::atomic::Ordering::SeqCst), 64);
}

/// Exhausting the retries or the round ceiling raises `SyncManifestRebaseExhausted`, reports every uploaded object as
/// already in the bucket, and asks for a re-run.
// spec: store.merge.exhausted@c7d702ef
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

/// Each commit stamps its instant and pushing node, and the generation listing summarizes every
/// generation with that stamp and the project's entry and tombstone counts.
#[test]
fn each_commit_stamps_its_instant_and_node_and_the_listing_counts_tombstones() {
    let dir = tempfile::tempdir().unwrap();
    let b = bucket(dir.path());
    let a = node("ingest-a", b.clone(), "");
    a.land("run-1", json!([{"id": 1}]), "2030-01-01T00:00:00Z");
    a.syncer.push(at("2030-01-01T01:00:00Z")).unwrap();
    std::fs::remove_dir_all(a.root().join("tables/filings/data/runs/run-1")).unwrap();
    a.syncer.push(at("2030-01-02T01:00:00Z")).unwrap();
    assert_eq!(generation(b.as_ref(), 2).committed_by.as_deref(), Some("ingest-a"));
    let listed = a.syncer.generation_list().unwrap();
    assert_eq!(listed.iter().map(|g| (g.generation, g.committed_at)).collect::<Vec<_>>(),
        [(1, Some(at("2030-01-01T01:00:00Z"))), (2, Some(at("2030-01-02T01:00:00Z")))]);
    assert_eq!(listed[0].tombstones, 0);
    assert!(listed[1].tombstones >= 2 && listed[1].entries < listed[0].entries, "{listed:?}");
}

/// The bucket key of unit `i`'s run part when a node lands table `t<i>` under run `run-<i>`.
fn unit_part(node: &str, i: usize) -> String {
    format!("research/tables/t{i}/data/runs/run-{i}/{node}/part-00000.parquet")
}

/// Units sharing one store root push through its one push guard: while one push holds it, each overlapping push of
/// the root waits within its bound and commits after it. For N = 2, 4 and 8 units landing disjoint tables, the first
/// push holds at its manifest read while the other N-1 push; the record is the `SyncPushInFlight` refusals summed
/// over the three sets, and every unit's run is published.
#[test]
fn parallel_units_on_one_store_root_count_push_in_flight_refusals() {
    let (mut refusals, mut pushes) = (0usize, 0u64);
    for units in [2usize, 4, 8] {
        let dir = tempfile::tempdir().unwrap();
        let inner = bucket(dir.path());
        let (entered_tx, entered_rx) = std::sync::mpsc::channel::<()>();
        let (gate_tx, gate_rx) = std::sync::mpsc::channel::<()>();
        let entered = std::sync::Mutex::new(Some(entered_tx));
        let gate = std::sync::Mutex::new(Some(gate_rx));
        let held: Arc<dyn ObjectStore> = Arc::new(Scripted {
            inner: inner.clone(),
            script: Script {
                on_get: Some(Box::new(move |key: &str| {
                    if key == "team/manifest.json" {
                        if let Some(tx) = entered.lock().unwrap().take() {
                            tx.send(()).unwrap();
                            let rx = gate.lock().unwrap().take().unwrap();
                            rx.recv_timeout(std::time::Duration::from_secs(30)).expect("the holding push is released");
                        }
                    }
                    None
                })),
                ..Script::default()
            },
        });
        let a = node("ingest-a", held, "");
        for i in 0..units {
            a.land_into(&format!("t{i}"), &format!("run-{i}"), json!([{"id": i}]), "2030-01-01T00:00:00Z");
        }
        let (holder, others) = std::thread::scope(|s| {
            let holder = s.spawn(|| a.syncer.push(at(NOW)));
            entered_rx.recv_timeout(std::time::Duration::from_secs(30)).expect("the first push reads the manifest");
            let (done_tx, done_rx) = std::sync::mpsc::channel();
            for _ in 1..units {
                let done = done_tx.clone();
                let a = &a;
                s.spawn(move || done.send(a.syncer.push(at(NOW))).unwrap());
            }
            drop(done_tx);
            // Every overlapping push answers before the holder moves on, or the holder is released after 2 s.
            let mut others = Vec::new();
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
            while others.len() < units - 1 {
                match done_rx.recv_timeout(deadline.saturating_duration_since(std::time::Instant::now())) {
                    Ok(outcome) => others.push(outcome),
                    Err(_) => break,
                }
            }
            gate_tx.send(()).unwrap();
            others.extend(done_rx.iter());
            (holder.join().unwrap(), others)
        });
        holder.expect("the holding push commits");
        assert_eq!(others.len(), units - 1);
        let refused = others.iter().filter(|o| matches!(o, Err(SyncError::Store(StoreError::SyncPushInFlight(_))))).count();
        assert!(others.iter().all(|o| o.is_ok() || matches!(o, Err(SyncError::Store(StoreError::SyncPushInFlight(_))))), "{others:?}");
        let m = manifest(inner.as_ref());
        for i in 0..units {
            assert!(m.entries.contains_key(&unit_part("ingest-a", i)), "unit {i} of {units} is unpublished");
        }
        eprintln!("{units} overlapping pushes of one root: {refused} refused");
        refusals += refused;
        pushes += units as u64;
    }
    contextful_eval::record::emit("push-parallel-same-root-refusals", refusals as f64, pushes, 0x5eed_0090);
    assert_eq!(refusals, 0, "an overlapping push of one root waits for the guard and commits");
}

/// A round barrier for manifest commits: each writer's manifest put waits until every live writer has reached its
/// own, so the bucket's compare-and-set picks one winner per round.
#[derive(Default)]
struct Lockstep {
    /// Writers arrived this round, writers still pushing, and the round number.
    state: std::sync::Mutex<(usize, usize, u64)>,
    turn: std::sync::Condvar,
}

impl Lockstep {
    fn arrive(&self) {
        let mut s = self.state.lock().unwrap();
        s.0 += 1;
        if s.0 >= s.1 {
            *s = (0, s.1, s.2 + 1);
            self.turn.notify_all();
            return;
        }
        let round = s.2;
        let (_s, wait) = self.turn.wait_timeout_while(s, std::time::Duration::from_secs(30), |s| s.2 == round).unwrap();
        assert!(!wait.timed_out(), "a manifest commit round never filled");
    }

    fn leave(&self) {
        let mut s = self.state.lock().unwrap();
        s.1 -= 1;
        if s.0 > 0 && s.0 >= s.1 {
            *s = (0, s.1, s.2 + 1);
            self.turn.notify_all();
        }
    }
}

/// Leaves the barrier when its writer's push returns, however it returns.
struct Leaving<'a>(&'a Lockstep);

impl Drop for Leaving<'_> {
    fn drop(&mut self) {
        self.0.leave();
    }
}

/// Writers on distinct nodes and disks merge disjoint tables through the manifest's compare-and-set, re-reading and
/// re-merging after each lost round. In lockstep each round has one winner, and each loser's next round meets a
/// newer generation, which counts against no retry. For N = 2, 4 and 8 writers the record is the writers summed
/// over the three sets that exhaust their rounds with `SyncManifestRebaseExhausted`.
#[test]
fn lockstep_writers_on_distinct_nodes_count_exhausted_rebases() {
    let (mut exhausted, mut pushes) = (0usize, 0u64);
    for writers in [2usize, 4, 8] {
        let dir = tempfile::tempdir().unwrap();
        let inner = bucket(dir.path());
        let lockstep = Arc::new(Lockstep::default());
        lockstep.state.lock().unwrap().1 = writers;
        let barrier = lockstep.clone();
        let stepped: Arc<dyn ObjectStore> = Arc::new(Scripted {
            inner: inner.clone(),
            script: Script {
                on_put: Some(Box::new(move |key, _| {
                    if key.ends_with("/manifest.json") {
                        barrier.arrive();
                    }
                    None
                })),
                ..Script::default()
            },
        });
        let nodes: Vec<_> = (0..writers).map(|i| node(&format!("ingest-{i}"), stepped.clone(), "")).collect();
        for (i, n) in nodes.iter().enumerate() {
            // A plain landing writes no local pointer, so each push commits once.
            n.land_into(&format!("t{i}"), &format!("run-{i}"), json!([{"id": i}]), "2030-01-01T00:00:00Z");
        }
        let outcomes: Vec<_> = std::thread::scope(|s| {
            let handles: Vec<_> = nodes
                .iter()
                .map(|n| {
                    let lockstep = &lockstep;
                    s.spawn(move || {
                        let _leaving = Leaving(lockstep);
                        n.syncer.push(at(NOW))
                    })
                })
                .collect();
            handles.into_iter().map(|h| h.join().unwrap()).collect()
        });
        let m = manifest(inner.as_ref());
        let mut lost = 0;
        for (i, outcome) in outcomes.iter().enumerate() {
            match outcome {
                Ok(report) => {
                    assert!(report.pointers.is_empty(), "one commit per push: {report:?}");
                    assert!(m.entries.contains_key(&unit_part(&format!("ingest-{i}"), i)), "writer {i} committed and is unlisted");
                }
                Err(SyncError::Store(StoreError::SyncManifestRebaseExhausted(_))) => lost += 1,
                Err(e) => panic!("writer {i}: {e:?}"),
            }
        }
        eprintln!("{writers} lockstep writers: {lost} exhausted their rounds");
        exhausted += lost;
        pushes += writers as u64;
    }
    contextful_eval::record::emit("push-lockstep-rebase-exhausted", exhausted as f64, pushes, 0x5eed_0091);
    assert_eq!(exhausted, 0, "every lockstep writer commits");
}

/// Two disks pushing under one node id each list the bucket's entries owned by that id, and a push tombstones only
/// those its own disk held at its last sync. Disk A pushes three runs, then disk B, which pulled before A pushed,
/// pushes one run of another table; the record is the number of A's runs B's commit tombstones, and a third node
/// pulls every one of them.
#[test]
fn sibling_disks_sharing_a_node_id_count_tombstoned_runs() {
    const RUNS: usize = 3;
    let dir = tempfile::tempdir().unwrap();
    let b = bucket(dir.path());
    let first = node("ingest-s", b.clone(), "");
    let (a, sibling) = (node("conformance", b.clone(), ""), node("conformance", b.clone(), ""));
    crate::support::trust(&[&first, &a, &sibling], "2030-01-01T00:00:00Z");
    first.land("run-s", json!([{"id": 0}]), "2030-01-01T00:00:00Z");
    first.syncer.push(at(NOW)).unwrap();
    for n in [&a, &sibling] {
        n.syncer.pull(&contextful_sync::PullScope::default()).unwrap();
    }
    for r in 0..RUNS {
        a.land_into("t0", &format!("run-a{r}"), json!([{"id": r}]), "2030-01-01T00:00:00Z");
    }
    record_run_state(&a);
    a.syncer.push(at(NOW)).unwrap();
    let pushed = manifest(b.as_ref());
    let runs: Vec<Vec<String>> = (0..RUNS)
        .map(|r| pushed.entries.keys().filter(|k| k.starts_with(&format!("research/tables/t0/data/runs/run-a{r}/conformance/"))).cloned().collect())
        .collect();
    assert_eq!(runs.len(), RUNS);
    assert!(runs.iter().all(|keys| !keys.is_empty()), "A's push lists each of its runs: {runs:?}");
    sibling.land_into("t1", "run-b", json!([{"id": 9}]), "2030-01-01T00:00:00Z");
    record_run_state(&sibling);
    let outcome = sibling.syncer.push(at(NOW));
    let after = manifest(b.as_ref());
    let tombstoned = runs.iter().filter(|keys| keys.iter().any(|k| after.tombstones.contains_key(k) && !after.entries.contains_key(k))).count();
    eprintln!("the sibling push answered {:?}; {tombstoned} of A's {RUNS} runs tombstoned", outcome.as_ref().map(|r| r.generation));
    contextful_eval::record::emit("push-shared-node-sibling-tombstones", tombstoned as f64, RUNS as u64, 0x5eed_0092);
    outcome.expect("the sibling push commits");
    assert_eq!(tombstoned, 0, "the sibling tombstones no run its disk never held");
    assert!(after.entries.keys().any(|k| k.starts_with("research/tables/t1/data/runs/run-b/conformance/")), "the sibling's run is listed");
    let reader = node("ingest-r", b.clone(), "");
    crate::support::trust(&[&first, &a, &sibling, &reader], "2030-01-01T00:00:00Z");
    reader.syncer.pull(&contextful_sync::PullScope::default()).unwrap();
    for key in runs.iter().flatten() {
        let rel = key.strip_prefix("research/").unwrap();
        assert!(reader.root().join(rel).exists(), "a third node pulls `{key}`");
    }
}

/// The keys of `node` the store's `.sync-held.json` records.
fn held_record(n: &crate::support::Node) -> Vec<String> {
    let record: serde_json::Value = serde_json::from_slice(&std::fs::read(n.root().join(contextful_sync::HELD_FILE)).unwrap()).unwrap();
    record["nodes"][n.syncer.node.as_str()].as_array().expect("a record for the node").iter().map(|k| k.as_str().unwrap().to_string()).collect()
}

/// Write the node's run state into its store root, as the CLI does before every push.
fn record_run_state(n: &crate::support::Node) {
    let state = contextful_sync::RunState { format: contextful_sync::run_state::RUN_STATE_FORMAT, node_id: n.syncer.node.clone(), runs: Default::default(), cursors: Vec::new(), control_version: None };
    contextful_sync::run_state::record(&n.syncer.store, &state).unwrap();
}

const RUN_1: &str = "research/tables/filings/data/runs/run-1/ingest-a/part-00000.parquet";

/// A push records in `.sync-held.json`, under its node id, each owned key the store holds, adding them before its
/// commit and replacing the node's keys after; a pull holding the push guard adds each such key the bucket manifest
/// lists.
// spec: store.merge.held-record@1de637d6
#[test]
fn a_push_records_the_keys_it_holds_and_a_pull_adds_those_it_brings_home() {
    let dir = tempfile::tempdir().unwrap();
    let b = bucket(dir.path());
    let a = node("ingest-a", b.clone(), "");
    a.land("run-1", json!([{"id": 1}]), "2030-01-01T00:00:00Z");
    a.syncer.push(at(NOW)).unwrap();
    let recorded = held_record(&a);
    assert!(recorded.iter().any(|k| k == RUN_1), "{recorded:?}");
    assert!(recorded.iter().all(|k| k.contains("/ingest-a/")), "only keys ingest-a owns: {recorded:?}");
    // A replacement disk under the node id pulls the node's keys home and records them.
    let replacement = node("ingest-a", b.clone(), "");
    replacement.syncer.pull(&contextful_sync::PullScope::default()).unwrap();
    assert!(held_record(&replacement).iter().any(|k| k == RUN_1));
    // So its deletion of a pulled run propagates.
    std::fs::remove_dir_all(replacement.root().join("tables/filings/data/runs/run-1")).unwrap();
    replacement.syncer.push(at(NOW)).unwrap();
    let m = manifest(b.as_ref());
    assert!(m.tombstones.contains_key(RUN_1) && !m.entries.contains_key(RUN_1), "the held run leaves with a tombstone");
    assert!(!held_record(&replacement).iter().any(|k| k == RUN_1), "the record drops what the store no longer holds");
    // A push under another node id on the store keeps this node's keys.
    let mut other = node("ingest-b", b.clone(), "");
    other.syncer.store = a.syncer.store.clone();
    other.syncer.push(at(NOW)).unwrap();
    assert!(held_record(&a).iter().any(|k| k == RUN_1), "ingest-a's keys survive a push under ingest-b");
}

/// A pull extends the record only while it holds the push guard: during a push of the store it waits within
/// `[sync] push_wait_secs`, and the record it writes keeps every key that push committed.
#[test]
fn a_pull_extends_the_record_only_while_it_holds_the_push_guard() {
    let dir = tempfile::tempdir().unwrap();
    let inner = bucket(dir.path());
    let armed = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let (gate_tx, gate_rx) = std::sync::mpsc::channel::<()>();
    let (entered_tx, entered_rx) = std::sync::mpsc::channel::<()>();
    let gate = std::sync::Mutex::new(Some(gate_rx));
    let entered = std::sync::Mutex::new(Some(entered_tx));
    let arm = armed.clone();
    let slow: Arc<dyn ObjectStore> = Arc::new(Scripted {
        inner: inner.clone(),
        script: Script {
            on_get: Some(Box::new(move |key: &str| {
                if arm.load(std::sync::atomic::Ordering::SeqCst) && key.ends_with("manifest.json") {
                    if let Some(tx) = entered.lock().unwrap().take() {
                        tx.send(()).unwrap();
                        gate.lock().unwrap().take().unwrap().recv_timeout(std::time::Duration::from_secs(30)).expect("the holder is released");
                    }
                }
                None
            })),
            ..Script::default()
        },
    });
    let a = node("ingest-a", slow, "");
    a.land("run-1", json!([{"id": 1}]), "2030-01-01T00:00:00Z");
    a.syncer.push(at(NOW)).unwrap();
    a.land("run-2", json!([{"id": 2}]), "2030-01-01T00:00:00Z");
    armed.store(true, std::sync::atomic::Ordering::SeqCst);
    let mut hasty = node("ingest-a", inner.clone(), "");
    hasty.syncer.store = a.syncer.store.clone();
    hasty.syncer.config.push_wait_secs = Some(1);
    let mut patient = node("ingest-a", inner.clone(), "");
    patient.syncer.store = a.syncer.store.clone();
    patient.syncer.config.push_wait_secs = Some(30);
    std::thread::scope(|s| {
        let push = s.spawn(|| a.syncer.push(at(NOW)));
        entered_rx.recv_timeout(std::time::Duration::from_secs(30)).expect("the push reads the manifest");
        match hasty.syncer.pull(&contextful_sync::PullScope::default()) {
            Err(SyncError::Store(StoreError::SyncPushInFlight(m))) => assert!(m.contains(&std::process::id().to_string()), "{m}"),
            other => panic!("a pull during a push waits for its guard: {other:?}"),
        }
        let pull = s.spawn(|| patient.syncer.pull(&contextful_sync::PullScope::default()));
        std::thread::sleep(std::time::Duration::from_millis(200));
        gate_tx.send(()).unwrap();
        push.join().unwrap().unwrap();
        pull.join().unwrap().expect("a pull waiting within its bound runs after the push");
    });
    let recorded = held_record(&a);
    for run in ["run-1", "run-2"] {
        assert!(recorded.iter().any(|k| *k == RUN_1.replace("run-1", run)), "{run} stays recorded: {recorded:?}");
    }
}

/// A store whose `.sync-held.json` names no keys for its node reads as holding every remote key that node owns when an
/// earlier push left its `.push.lock`, and none otherwise.
// spec: store.merge.held-absent@6df5abdf
#[test]
fn a_store_without_a_record_holds_every_owned_key_once_it_pushed_and_none_before() {
    let dir = tempfile::tempdir().unwrap();
    let b = bucket(dir.path());
    let a = node("ingest-a", b.clone(), "");
    a.land("run-1", json!([{"id": 1}]), "2030-01-01T00:00:00Z");
    a.land("run-2", json!([{"id": 2}]), "2030-01-01T00:00:00Z");
    a.syncer.push(at(NOW)).unwrap();
    // A store that pushed before it kept a record propagates its deletions.
    std::fs::remove_file(a.root().join(contextful_sync::HELD_FILE)).unwrap();
    assert!(a.root().join(".push.lock").exists());
    std::fs::remove_dir_all(a.root().join("tables/filings/data/runs/run-1")).unwrap();
    a.syncer.push(at(NOW)).unwrap();
    let m = manifest(b.as_ref());
    assert!(m.tombstones.contains_key(RUN_1) && !m.entries.contains_key(RUN_1), "{:?}", m.tombstones.keys().collect::<Vec<_>>());
    // A fresh disk under the node id that never pulled or pushed deletes nothing it never held, though it writes its
    // run state before the push as the CLI does.
    let fresh = node("ingest-a", b.clone(), "");
    fresh.land("run-3", json!([{"id": 3}]), "2030-01-01T00:00:00Z");
    record_run_state(&fresh);
    fresh.syncer.push(at(NOW)).unwrap();
    let run_2 = RUN_1.replace("run-1", "run-2");
    let m = manifest(b.as_ref());
    assert!(m.entries.contains_key(&run_2) && !m.tombstones.contains_key(&run_2), "run-2 stays listed");
    assert!(m.entries.contains_key(&RUN_1.replace("run-1", "run-3")));
}

/// A run row for unit `unit` at tick `tick`, its id derived from the fixture seed.
fn tick_row(seed: u64, tick: usize, unit: usize, phase: &str, started_at: contextful_core::time::Instant) -> contextful_core::run::record::RunRow {
    serde_json::from_value(json!({
        "run_id": format!("u{unit}-{:016x}", seed ^ ((tick as u64) << 8 | unit as u64)), "pipeline_id": format!("unit-{unit}"),
        "table": "filings", "site_id": "site", "status": if phase == "plan" { "running" } else { "success" },
        "started_at": started_at.to_rfc3339_nanos(), "rows": 0, "bytes": 0, "batches": 0,
        "connector_id": "vendor", "connector_version": "1", "connector_hash": "h",
        "phase": phase, "execution_id": format!("x-{unit}-{tick}"),
    }))
    .unwrap()
}

/// Each unit run appends a plan row and a commit row to the run record, each one store run of a part and a run
/// manifest, and nothing folds the run record in a push cycle. Five units a tick, one push a tick, hourly from
/// 2030-01-01: the records are the `_runs` entries the bucket manifest gains per tick over the last 50 ticks, its
/// live `_runs` entries and its `_runs` entries plus tombstones per tick, and the bucket keys gained per tick. Under
/// `contextful-ci measure` the schedule is 240 ticks; elsewhere 8 ticks with a window of 3 exercise the path.
#[test]
fn the_run_record_grows_the_bucket_by_a_fixed_object_count_per_tick() {
    const SEED: u64 = 0x5eed_0615;
    const UNITS: usize = 5;
    let measuring = std::env::var_os(contextful_eval::record::MEASURE_DIR_VAR).is_some();
    let (ticks, window) = if measuring { (240, 50) } else { (8, 3) };
    let dir = tempfile::tempdir().unwrap();
    let b = bucket(dir.path());
    let a = node("ingest-a", b.clone(), "");
    let id = contextful_core::store::lay_out::NodeId::parse("ingest-a").unwrap();
    let start = at("2030-01-01T00:00:00Z");
    let runs = |m: &BucketManifest| -> (usize, usize) {
        let under = |k: &&String| k.starts_with("research/tables/_runs/");
        (m.entries.keys().filter(under).count(), m.tombstones.keys().filter(under).count())
    };
    let mut marks = Vec::new();
    for tick in 0..ticks {
        let t = start.plus_secs(tick as u64 * 3600);
        for unit in 0..UNITS {
            let plan = t.plus_secs(unit as u64);
            contextful_context::run_record::append(&a.syncer.store, &id, &tick_row(SEED, tick, unit, "plan", plan), plan).unwrap();
            let commit = plan.plus_secs(1);
            contextful_context::run_record::append(&a.syncer.store, &id, &tick_row(SEED, tick, unit, "commit", plan), commit).unwrap();
        }
        a.syncer.push(t.plus_secs(UNITS as u64 + 1)).unwrap();
        if tick + 1 == ticks - window || tick + 1 == ticks {
            let m = manifest(b.as_ref());
            marks.push((runs(&m), b.list("team/").unwrap().len()));
        }
    }
    let [((live_from, dead_from), keys_from), ((live, dead), keys)] = marks[..] else { panic!("{marks:?}") };
    assert!(live > 0, "the run record reaches the bucket");
    let per_tick = |to: usize, from: usize| (to as f64 - from as f64) / window as f64;
    let (objects, listed, bucket_keys) = (per_tick(live, live_from), per_tick(live + dead, live_from + dead_from), per_tick(keys, keys_from));
    eprintln!("after {ticks} ticks: {live} live `_runs` entries, {dead} tombstoned; per tick {objects} live, {listed} listed, {bucket_keys} bucket keys");
    contextful_eval::record::emit("runs-record-objects-per-tick", objects, window as u64, SEED);
    contextful_eval::record::emit("runs-record-live-objects", live as f64, ticks as u64, SEED);
    contextful_eval::record::emit("runs-record-manifest-keys-per-tick", listed, window as u64, SEED);
    contextful_eval::record::emit("bucket-keys-per-tick", bucket_keys, window as u64, SEED);
}
