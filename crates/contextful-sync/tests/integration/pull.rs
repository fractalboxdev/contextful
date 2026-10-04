//! `store.pull`, `store.lease` and `store.replicate` through the syncer.

use crate::support::{at, bucket, node, Script, Scripted};
use contextful_context::fold::fold;
use contextful_core::store::declare::TableDecl;
use contextful_core::store::fold::FoldOutcome;
use contextful_core::store::object::{Condition, ObjectStore};
use contextful_core::store::sync::{BucketManifest, Entry};
use contextful_core::run::journal::sha256_hex;
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
            injection: contextful_core::store::reserve::Injection { run_id: "run-x".into(), site_id: "s".into(), batch_seq: None, authored_by: None, taint: None },
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
    let scope = PullScope { tables: vec!["filings".into()], replicate_off: vec!["filings".into()], ..PullScope::default() };
    match r.syncer.pull(&scope) {
        Err(SyncError::Store(StoreError::ReplicaSensitiveTable(m))) => assert!(m.contains("filings") && m.contains("proxying face"), "{m}"),
        other => panic!("{other:?}"),
    }
    // An unscoped refresh takes every other table and leaves the replicate-off one behind.
    r.syncer.pull(&PullScope { tables: vec![], replicate_off: vec!["filings".into()], ..PullScope::default() }).unwrap();
    assert!(!r.root().join("tables/filings").exists());
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
