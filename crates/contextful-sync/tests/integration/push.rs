//! `store.push`, `store.probe` and `store.merge` through the syncer.

use crate::support::{at, bucket, node, Script, Scripted};
use contextful_core::store::object::{ObjectError, ObjectStore, Put};
use contextful_core::store::sync::{BucketManifest, Coordination};
use contextful_core::store::StoreError;
use contextful_sync::{FsBucket, SyncError, VolumeClass};
use serde_json::json;
use std::sync::Arc;

const NOW: &str = "2030-01-01T01:00:00Z";

fn manifest(b: &dyn ObjectStore) -> BucketManifest {
    serde_json::from_slice(&b.get("team/manifest.json").unwrap().unwrap().0).unwrap()
}

/// A push uploads each file it owns, or no node owns, whose digest the bucket lacks under
/// `<prefix>/<project>/<path>` by a conditional put; machine catalogs, `config.toml`, locks, staging directories
/// and table pointers stay local.
// spec: store.push.wire-format@eee2b392
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
