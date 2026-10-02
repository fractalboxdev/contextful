//! `store.emit`, the manifest format, and the generation manifests a push writes and a pull restores.

use crate::support::{at, bucket, node, Node, Script, Scripted};
use contextful_context::fold::fold;
use contextful_core::store::declare::TableDecl;
use contextful_core::store::fold::FoldOutcome;
use contextful_core::store::object::{Condition, ObjectStore};
use contextful_core::store::sync::{generation_key, BucketManifest, MANIFEST_FORMAT};
use contextful_core::store::StoreError;
use contextful_sync::{PullScope, SyncError};
use serde_json::json;
use std::sync::Arc;

const NOW: &str = "2030-01-01T01:00:00Z";

fn manifest(b: &dyn ObjectStore) -> BucketManifest {
    serde_json::from_slice(&b.get("team/manifest.json").unwrap().unwrap().0).unwrap()
}

fn files(n: &Node) -> Vec<String> {
    contextful_context::scan::scan(&n.syncer.store, &TableDecl::named("filings"), Default::default()).unwrap().files
}

fn at_generation(n: u64) -> PullScope {
    PullScope { generation: Some(n), ..PullScope::default() }
}

/// `contextful sync manifest --emit` prints, as a bucket manifest in JSON, each key a push of the store commits
/// with its sha256, size and owner, and reads and writes no bucket object.
// spec: store.emit.plan@5b7ae685
#[test]
fn an_emitted_plan_lists_what_the_push_commits_and_touches_no_bucket() {
    let dir = tempfile::tempdir().unwrap();
    let b = bucket(dir.path());
    let counting = Arc::new(Scripted { inner: b.clone(), script: Script::default() });
    let a = node("ingest-a", counting.clone(), "");
    a.land("run-1", json!([{"id": 1, "title": "a"}]), "2030-01-01T00:00:00Z");
    let plan = a.syncer.plan_manifest().unwrap();
    let again = contextful_sync::sync::plan_manifest(&a.syncer.store, "research", "ingest-a").unwrap();
    assert!(counting.script.gets.lock().unwrap().is_empty(), "the plan reads no bucket object");
    assert!(b.list("").unwrap().is_empty(), "the plan writes no bucket object");
    let emitted = serde_json::to_vec_pretty(&plan.manifest()).unwrap();
    assert_eq!(emitted, serde_json::to_vec_pretty(&again.manifest()).unwrap(), "an unchanged store emits identical bytes");
    let listed: BucketManifest = serde_json::from_slice(&emitted).unwrap();
    assert_eq!(listed.format, MANIFEST_FORMAT);
    a.syncer.push(at(NOW)).unwrap();
    assert_eq!(listed.entries, manifest(b.as_ref()).entries, "the committed manifest lists the plan's entries");
}

/// The plan lists every key {{store.push.wire-format}} uploads and each `schema.json` at its local digest; a key
/// another node owns stays out.
// spec: store.emit.plan-scope@57923e2a
#[test]
fn a_plan_leaves_out_keys_another_node_owns() {
    let dir = tempfile::tempdir().unwrap();
    let b = bucket(dir.path());
    let (a, c) = (node("ingest-a", b.clone(), ""), node("ingest-b", b.clone(), ""));
    c.land("run-1", json!([{"id": 2}]), "2030-01-01T00:00:00Z");
    c.syncer.push(at(NOW)).unwrap();
    a.syncer.pull(&PullScope::default()).unwrap();
    a.land("run-2", json!([{"id": 1, "pages": 3}]), "2030-01-01T00:00:00Z");
    let entries = a.syncer.plan_manifest().unwrap().manifest().entries;
    assert!(entries.keys().all(|k| !k.contains("/ingest-b/")), "{:?}", entries.keys().collect::<Vec<_>>());
    assert!(entries.contains_key("research/tables/filings/data/runs/run-2/ingest-a/part-00000.parquet"));
    let schema = std::fs::read(a.root().join("tables/filings/schema.json")).unwrap();
    assert_eq!(entries["research/tables/filings/schema.json"].sha256, contextful_core::run::journal::sha256_hex(&schema));
}

/// The bucket manifest and every generation manifest carry `format`, `1` for this layout; a manifest without
/// `format` reads as `1`.
// spec: store.push.manifest-format@85bad2a5
#[test]
fn a_manifest_without_a_format_reads_as_format_one() {
    let dir = tempfile::tempdir().unwrap();
    let b = bucket(dir.path());
    b.put("team/manifest.json", br#"{"entries":{},"tombstones":{}}"#, Condition::IfNoneMatch).unwrap();
    let a = node("ingest-a", b.clone(), "");
    a.land("run-1", json!([{"id": 1}]), "2030-01-01T00:00:00Z");
    a.syncer.push(at(NOW)).unwrap();
    let raw: serde_json::Value = serde_json::from_slice(&b.get("team/manifest.json").unwrap().unwrap().0).unwrap();
    assert_eq!(raw["format"], json!(1));
    let generation: serde_json::Value = serde_json::from_slice(&b.get(&format!("team/{}", generation_key(1))).unwrap().unwrap().0).unwrap();
    assert_eq!(generation["format"], json!(1));
}

/// A bucket or generation manifest whose `format` exceeds 1 raises `SyncManifestFormatUnsupported`, naming the key
/// and its format, before the push commits or the pull writes a file.
// spec: store.push.format-unsupported@fdaa8fb0
#[test]
fn a_manifest_of_a_newer_format_refuses_push_and_pull() {
    let dir = tempfile::tempdir().unwrap();
    let b = bucket(dir.path());
    let newer = br#"{"format":2,"entries":{"research/tables/filings/schema.json":{"sha256":"x","size":1}},"shape":"unknown"}"#;
    b.put("team/manifest.json", newer, Condition::IfNoneMatch).unwrap();
    let a = node("ingest-a", b.clone(), "");
    a.land("run-1", json!([{"id": 1}]), "2030-01-01T00:00:00Z");
    match a.syncer.push(at(NOW)) {
        Err(SyncError::Store(StoreError::SyncManifestFormatUnsupported(m))) => assert!(m.contains("manifest.json") && m.contains('2'), "{m}"),
        other => panic!("{other:?}"),
    }
    assert_eq!(b.get("team/manifest.json").unwrap().unwrap().0, newer.to_vec(), "the newer manifest stays as written");
    let c = node("ingest-b", b.clone(), "");
    match c.syncer.pull(&PullScope::default()) {
        Err(SyncError::Store(StoreError::SyncManifestFormatUnsupported(m))) => assert!(m.contains("manifest.json"), "{m}"),
        other => panic!("{other:?}"),
    }
    assert!(!c.root().join("tables/filings").exists(), "the pull writes no file");
}

/// Each manifest commit carries `generation` per {{store.push.generation-floor}} and the project's table pointers as
/// read before it and advanced by {{store.push.pointer-carry}}; the push then creates `<prefix>/manifests/gen-<N>.json` holding the committed bytes under
/// `If-None-Match`.
// spec: store.push.generation@70b37842
#[test]
fn each_push_commits_the_next_generation_and_writes_it_immutably() {
    let dir = tempfile::tempdir().unwrap();
    let b = bucket(dir.path());
    let a = node("ingest-a", b.clone(), "");
    a.land("run-1", json!([{"id": 1}]), "2030-01-01T00:00:00Z");
    assert_eq!(a.syncer.push(at(NOW)).unwrap().generation, 1);
    let held = a.syncer.acquire("filings", at(NOW)).unwrap();
    let FoldOutcome::Folded { snapshot_id, .. } = fold(&a.syncer.store, &TableDecl::named("filings"), at(NOW)).unwrap() else { panic!() };
    assert_eq!(a.syncer.push(at(NOW)).unwrap().generation, 2);
    a.syncer.publish("filings", &snapshot_id, &held).unwrap();
    assert_eq!(a.syncer.push(at(NOW)).unwrap().generation, 3);
    let committed = b.get("team/manifest.json").unwrap().unwrap().0;
    assert_eq!(b.get(&format!("team/{}", generation_key(3))).unwrap().unwrap().0, committed, "gen-3 holds the committed bytes");
    let m = manifest(b.as_ref());
    assert_eq!(m.generation, 3);
    let pointer = &m.pointers["research/tables/filings/_pointer.json"];
    assert_eq!(pointer.snapshot_id.as_deref(), Some(snapshot_id.as_str()));
    assert_eq!(pointer.fence, held.lease.fence);
    let gen2: BucketManifest = serde_json::from_slice(&b.get(&format!("team/{}", generation_key(2))).unwrap().unwrap().0).unwrap();
    let before = gen2.pointers.get("research/tables/filings/_pointer.json").and_then(|p| p.snapshot_id.clone());
    assert_eq!(before, None, "gen-2 committed before the snapshot was published");
    // The earlier generation stays as committed.
    assert_eq!(gen2.generation, 2);
}

/// A commit's `generation` is one past the greater of the read manifest's `generation`, absent reading as 0, and the
/// newest `gen-<N>.json` under the prefix.
// spec: store.push.generation-floor@cb58002c
#[test]
fn a_manifest_rewritten_without_its_generation_numbers_past_the_newest_generation_file() {
    let dir = tempfile::tempdir().unwrap();
    let b = bucket(dir.path());
    let a = node("ingest-a", b.clone(), "");
    a.land("run-1", json!([{"id": 1}]), "2030-01-01T00:00:00Z");
    assert_eq!(a.syncer.push(at(NOW)).unwrap().generation, 1);
    // A format-1 writer predating the field rewrites the manifest and drops `generation`.
    let (bytes, etag) = b.get("team/manifest.json").unwrap().unwrap();
    let mut raw: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    raw.as_object_mut().unwrap().remove("generation");
    b.put("team/manifest.json", &serde_json::to_vec(&raw).unwrap(), Condition::IfMatch(etag)).unwrap();
    a.land("run-2", json!([{"id": 2}]), "2030-01-01T00:00:00Z");
    assert_eq!(a.syncer.push(at(NOW)).unwrap().generation, 2);
    assert_eq!(a.syncer.push(at(NOW)).unwrap().generation, 3);
    let gen2: BucketManifest = serde_json::from_slice(&b.get(&format!("team/{}", generation_key(2))).unwrap().unwrap().0).unwrap();
    assert!(gen2.entries.contains_key("research/tables/filings/data/runs/run-2/ingest-a/part-00000.parquet"));
    assert_eq!(b.get(&format!("team/{}", generation_key(3))).unwrap().unwrap().0, b.get("team/manifest.json").unwrap().unwrap().0);
}

/// A push whose create of `gen-<N>.json` meets another commit's bytes raises `SyncGenerationConflict`, naming N, after
/// its manifest commit applied.
// spec: store.push.generation-conflict@4aba4e8b
#[test]
fn a_generation_file_holding_another_commit_refuses_and_the_next_push_numbers_past_it() {
    let dir = tempfile::tempdir().unwrap();
    let inner = bucket(dir.path());
    let raced = inner.clone();
    let once = std::sync::Mutex::new(true);
    let b: Arc<dyn ObjectStore> = Arc::new(Scripted {
        inner: inner.clone(),
        script: Script {
            on_put: Some(Box::new(move |key, _| {
                // Another commit takes `gen-2.json` between this push's manifest commit and its generation write.
                if key == "team/manifests/gen-2.json" && std::mem::take(&mut *once.lock().unwrap()) {
                    raced.put(key, br#"{"format":1,"generation":2,"entries":{}}"#, Condition::IfNoneMatch).unwrap();
                }
                None
            })),
            ..Script::default()
        },
    });
    let a = node("ingest-a", b, "");
    a.land("run-1", json!([{"id": 1}]), "2030-01-01T00:00:00Z");
    a.syncer.push(at(NOW)).unwrap();
    a.land("run-2", json!([{"id": 2}]), "2030-01-01T00:00:00Z");
    match a.syncer.push(at(NOW)) {
        Err(SyncError::Store(StoreError::SyncGenerationConflict(m))) => assert!(m.contains("gen-2.json"), "{m}"),
        other => panic!("{other:?}"),
    }
    assert_eq!(manifest(inner.as_ref()).generation, 2, "the manifest commit applied");
    assert_eq!(a.syncer.push(at(NOW)).unwrap().generation, 3, "the next push numbers past the taken generation");
    assert_eq!(a.syncer.push(at(NOW)).unwrap().generation, 4);
}

/// Before each manifest commit, a push finding no `gen-<N>.json` for the generation of the manifest it read creates it
/// from the bytes it read.
// spec: store.push.generation-heal@e4a13c43
#[test]
fn a_generation_committed_under_a_concurrent_push_is_written_before_the_next_commit() {
    let dir = tempfile::tempdir().unwrap();
    let inner = bucket(dir.path());
    let a = node("ingest-a", inner.clone(), "");
    a.land("run-1", json!([{"id": 1}]), "2030-01-01T00:00:00Z");
    a.syncer.push(at(NOW)).unwrap();
    // Another push commits generation 2 while this one uploads, and stops before writing `gen-2.json`.
    let raced = inner.clone();
    let gen2 = Arc::new(std::sync::Mutex::new(None::<Vec<u8>>));
    let taken = gen2.clone();
    let b: Arc<dyn ObjectStore> = Arc::new(Scripted {
        inner: inner.clone(),
        script: Script {
            on_put: Some(Box::new(move |key, _| {
                let mut taken = taken.lock().unwrap();
                if key == "team/manifest.json" && taken.is_none() {
                    let (bytes, etag) = raced.get(key).unwrap().unwrap();
                    let mut m: BucketManifest = serde_json::from_slice(&bytes).unwrap();
                    m.generation = 2;
                    let committed = serde_json::to_vec_pretty(&m).unwrap();
                    raced.put(key, &committed, Condition::IfMatch(etag)).unwrap();
                    *taken = Some(committed);
                }
                None
            })),
            ..Script::default()
        },
    });
    let c = node("ingest-b", b, "");
    c.land("run-2", json!([{"id": 2}]), "2030-01-01T00:00:00Z");
    let report = c.syncer.push(at(NOW)).unwrap();
    assert_eq!((report.generation, report.rounds), (3, 2));
    let written = inner.get(&format!("team/{}", generation_key(2))).unwrap().map(|(b, _)| b);
    assert_eq!(written, gen2.lock().unwrap().clone(), "gen-2.json holds the concurrent commit");
}

#[test]
fn a_missing_generation_is_written_by_the_next_push() {
    let dir = tempfile::tempdir().unwrap();
    let b = bucket(dir.path());
    let a = node("ingest-a", b.clone(), "");
    a.land("run-1", json!([{"id": 1}]), "2030-01-01T00:00:00Z");
    a.syncer.push(at(NOW)).unwrap();
    let committed = b.get("team/manifest.json").unwrap().unwrap().0;
    b.delete(&format!("team/{}", generation_key(1))).unwrap();
    a.land("run-2", json!([{"id": 2}]), "2030-01-01T00:00:00Z");
    a.syncer.push(at(NOW)).unwrap();
    assert_eq!(b.get(&format!("team/{}", generation_key(1))).unwrap().unwrap().0, committed);
    assert!(b.get(&format!("team/{}", generation_key(2))).unwrap().is_some());
}

/// `contextful sync pull --generation <N>` reads `gen-<N>.json` in place of the bucket manifest, downloads each
/// listed key in scope at its listed digest, and writes exactly that generation's pointers, whatever their fence.
// spec: store.pull.generation@22b2d58b
#[test]
fn a_generation_pull_restores_exactly_that_generation() {
    let dir = tempfile::tempdir().unwrap();
    let b = bucket(dir.path());
    let a = node("ingest-a", b.clone(), "");
    a.land("run-1", json!([{"id": 1, "title": "a"}]), "2030-01-01T00:00:00Z");
    a.syncer.push(at(NOW)).unwrap();
    // Generation 2 holds a published snapshot of run-1.
    let held = a.syncer.acquire("filings", at(NOW)).unwrap();
    let FoldOutcome::Folded { snapshot_id: first, .. } = fold(&a.syncer.store, &TableDecl::named("filings"), at(NOW)).unwrap() else { panic!() };
    a.syncer.push(at(NOW)).unwrap();
    a.syncer.publish("filings", &first, &held).unwrap();
    assert_eq!(a.syncer.push(at(NOW)).unwrap().generation, 3);
    // A later run and a later snapshot under a later push.
    a.land("run-2", json!([{"id": 2, "title": "b"}]), "2030-01-01T01:30:00Z");
    let FoldOutcome::Folded { snapshot_id: second, .. } = fold(&a.syncer.store, &TableDecl::named("filings"), at("2030-01-01T02:00:00Z")).unwrap() else { panic!() };
    a.syncer.push(at(NOW)).unwrap();
    a.syncer.publish("filings", &second, &held).unwrap();
    a.syncer.push(at(NOW)).unwrap();

    // Generation 1: run-1 alone, no pointer.
    let c = node("ingest-c", b.clone(), "");
    c.syncer.pull(&at_generation(1)).unwrap();
    assert_eq!(files(&c), ["tables/filings/data/runs/run-1/ingest-a/part-00000.parquet"]);
    assert!(!c.root().join("tables/filings/_pointer.json").exists());

    // Generation 3: the first snapshot, although the bucket's pointer names the second.
    let d = node("ingest-d", b.clone(), "");
    let report = d.syncer.pull(&at_generation(3)).unwrap();
    assert_eq!(report.pointers, ["research/tables/filings/_pointer.json"]);
    assert_eq!(files(&d), [format!("tables/filings/data/snapshots/{first}/part-00000.parquet")]);
    assert!(!d.root().join("tables/filings/data/runs/run-2").exists(), "run-2 is outside generation 3");

    // An ordinary pull moves the restored store on to the bucket's current state.
    d.syncer.pull(&PullScope::default()).unwrap();
    assert_eq!(files(&d), [format!("tables/filings/data/snapshots/{second}/part-00000.parquet")]);
}

/// A generation the bucket holds no `gen-<N>.json` for raises `SyncGenerationAbsent`, naming N and the newest
/// generation, and writes nothing.
// spec: store.pull.generation-absent@7866e592
#[test]
fn a_generation_the_bucket_lacks_refuses() {
    let dir = tempfile::tempdir().unwrap();
    let b = bucket(dir.path());
    let a = node("ingest-a", b.clone(), "");
    a.land("run-1", json!([{"id": 1}]), "2030-01-01T00:00:00Z");
    a.syncer.push(at(NOW)).unwrap();
    let c = node("ingest-b", b, "");
    match c.syncer.pull(&at_generation(9)) {
        Err(SyncError::Store(StoreError::SyncGenerationAbsent(m))) => assert!(m.contains("gen-9") && m.contains("newest generation is 1"), "{m}"),
        other => panic!("{other:?}"),
    }
    assert!(!c.root().join("tables").exists());
}

/// A generation pull into a store holding a syncable file in scope that the generation does not list raises
/// `SyncGenerationDiverged`, naming the file, and writes nothing.
// spec: store.pull.generation-diverged@a5ed5f76
#[test]
fn a_generation_pull_into_a_store_holding_unlisted_files_refuses() {
    let dir = tempfile::tempdir().unwrap();
    let b = bucket(dir.path());
    let a = node("ingest-a", b.clone(), "");
    a.land("run-1", json!([{"id": 1}]), "2030-01-01T00:00:00Z");
    a.syncer.push(at(NOW)).unwrap();
    let c = node("ingest-b", b, "");
    c.land("run-7", json!([{"id": 7}]), "2030-01-01T00:00:00Z");
    match c.syncer.pull(&at_generation(1)) {
        Err(SyncError::Store(StoreError::SyncGenerationDiverged(m))) => assert!(m.contains("run-7/ingest-b/"), "{m}"),
        other => panic!("{other:?}"),
    }
    assert!(!c.root().join("tables/filings/data/runs/run-1").exists(), "the refused pull downloads nothing");
}

/// A generation pull takes each `schema.json` as the bucket holds it, merged per {{store.pull.schema-merge}},
/// never refusing on its generation digest.
// spec: store.pull.generation-schema@9647c1d8
#[test]
fn a_generation_pull_takes_the_current_schema() {
    let dir = tempfile::tempdir().unwrap();
    let b = bucket(dir.path());
    let a = node("ingest-a", b.clone(), "");
    a.land("run-1", json!([{"id": 1}]), "2030-01-01T00:00:00Z");
    a.syncer.push(at(NOW)).unwrap();
    a.land("run-2", json!([{"id": 2, "pages": 4}]), "2030-01-01T00:00:00Z");
    a.syncer.push(at(NOW)).unwrap();
    let c = node("ingest-b", b, "");
    c.syncer.pull(&at_generation(1)).unwrap();
    let schema = std::fs::read_to_string(c.root().join("tables/filings/schema.json")).unwrap();
    assert!(schema.contains("\"pages\""), "{schema}");
    assert_eq!(files(&c), ["tables/filings/data/runs/run-1/ingest-a/part-00000.parquet"]);
}
