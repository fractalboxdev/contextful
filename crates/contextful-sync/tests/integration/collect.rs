//! `store.collect`: deleting the bucket objects the manifest no longer lists and no retained
//! generation names, each recorded as collected by a committed manifest before its object goes,
//! and the generation manifests past `[sync] retain_generations`.

use crate::support::{at, bucket, node, Node, Script, Scripted};
use contextful_context::rows::table_rows;
use contextful_core::store::declare::TableDecl;
use contextful_core::store::object::ObjectStore;
use contextful_core::store::sync::{generation_key, BucketManifest};
use contextful_core::store::StoreError;
use contextful_sync::{PullScope, SyncError};
use serde_json::json;
use std::num::NonZeroU64;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

const T0: &str = "2030-01-01T00:00:00Z";
const T1: &str = "2030-01-02T00:00:00Z";
const T2: &str = "2030-01-03T00:00:00Z";
const RUN: &str = "research/tables/filings/data/runs/run-1/";

fn manifest(b: &dyn ObjectStore) -> BucketManifest {
    serde_json::from_slice(&b.get("team/manifest.json").unwrap().unwrap().0).unwrap()
}

/// The objects under run `run-1` the bucket holds.
fn run_objects(b: &dyn ObjectStore) -> Vec<String> {
    b.list(&format!("team/{RUN}")).unwrap()
}

/// A bucket holding run `run-1`, which its owner deleted and a later push tombstoned; the owner's
/// copy of the run's directory is kept beside its store for a later restore.
fn tombstoned(retain: Option<u64>) -> (tempfile::TempDir, Arc<dyn ObjectStore>, Node, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let b = bucket(dir.path());
    let mut a = node("ingest-a", b.clone(), "");
    a.syncer.config.retain_generations = retain.and_then(NonZeroU64::new);
    a.land("run-1", json!([{"id": 1}]), T0);
    a.land("run-2", json!([{"id": 2}]), T0);
    a.syncer.push(at(T0)).unwrap();
    assert!(!run_objects(b.as_ref()).is_empty(), "the bucket holds the run");
    let saved = tempfile::tempdir().unwrap();
    copy_dir(&a.root().join("tables/filings/data/runs/run-1"), saved.path());
    std::fs::remove_dir_all(a.root().join("tables/filings/data/runs/run-1")).unwrap();
    a.syncer.push(at(T1)).unwrap();
    let m = manifest(b.as_ref());
    assert!(m.tombstones.keys().any(|k| k.starts_with(RUN)), "the push tombstones the deleted run");
    (dir, b, a, saved)
}

/// Copy the tree under `from` into `to`.
fn copy_dir(from: &std::path::Path, to: &std::path::Path) {
    std::fs::create_dir_all(to).unwrap();
    for entry in std::fs::read_dir(from).unwrap() {
        let path = entry.unwrap().path();
        let dest = to.join(path.file_name().unwrap());
        if path.is_dir() { copy_dir(&path, &dest) } else { std::fs::copy(&path, &dest).map(|_| ()).unwrap() }
    }
}

/// Restore the owner's saved copy of `run-1` into its store.
fn restore(a: &Node, saved: &tempfile::TempDir) {
    copy_dir(saved.path(), &a.root().join("tables/filings/data/runs/run-1"));
}

fn ids(rows: Vec<serde_json::Map<String, serde_json::Value>>) -> Vec<i64> {
    let mut ids: Vec<i64> = rows.iter().map(|r| r["id"].as_i64().unwrap()).collect();
    ids.sort_unstable();
    ids
}

/// A tombstoned object no retained generation lists is recorded as collected and deleted, its size read off the
/// generation that listed it, and a store pulled afterwards reads every remaining row.
// spec: store.collect.unlisted@87dfc65c
#[test]
fn a_tombstoned_object_no_retained_generation_lists_is_deleted() {
    let (_dir, b, a, _saved) = tombstoned(Some(1));
    let run = run_objects(b.as_ref());
    let report = a.syncer.collect(at(T2), false).unwrap();
    assert_eq!(report.unlisted.deleted, run.len(), "{report:?}");
    assert!(report.unlisted.bytes > 0 && report.unlisted.unknown_size == 0, "{report:?}");
    assert!(run_objects(b.as_ref()).is_empty(), "the tombstoned run's objects leave the bucket");
    let m = manifest(b.as_ref());
    assert!(run.iter().all(|k| m.collected.contains_key(k.strip_prefix("team/").unwrap())), "{:?}", m.collected.keys().collect::<Vec<_>>());
    let cold = node("reader", b.clone(), "");
    cold.syncer.pull(&PullScope::default()).unwrap();
    assert_eq!(ids(table_rows(&cold.syncer.store, &TableDecl::named("filings"), &["id"]).unwrap()), [2]);
}

/// `--dry-run` reports per class the keys collect records and the objects and bytes it deletes, and writes nothing.
// spec: store.collect.command@4c00f075
#[test]
fn a_dry_run_reports_each_class_and_writes_nothing() {
    let (_dir, b, a, _saved) = tombstoned(Some(1));
    let snapshot = |b: &dyn ObjectStore| -> Vec<(String, Vec<u8>)> { b.list("team/").unwrap().into_iter().map(|k| { let v = b.get(&k).unwrap().unwrap().0; (k, v) }).collect() };
    let before = snapshot(b.as_ref());
    assert!(!before.is_empty());
    let report = a.syncer.collect(at(T2), true).unwrap();
    assert!(report.dry_run);
    assert!(report.unlisted.dropped > 0 && report.unlisted.deleted > 0 && report.unlisted.bytes > 0, "{report:?}");
    assert!(report.generations.deleted > 0 && report.generations.bytes > 0, "{report:?}");
    assert_eq!(before, snapshot(b.as_ref()), "a dry run writes, replaces and deletes nothing");
    let real = a.syncer.collect(at(T2), false).unwrap();
    assert_eq!((real.unlisted.deleted, real.unlisted.bytes, real.generations.deleted), (report.unlisted.deleted, report.unlisted.bytes, report.generations.deleted));
}

/// Collect deletes no object whose key a retained generation lists, and leaves it for a later collect.
// spec: store.collect.generation-listed@811e21af
#[test]
fn an_object_a_retained_generation_lists_stays() {
    let (_dir, b, a, _saved) = tombstoned(None);
    let run = run_objects(b.as_ref());
    let report = a.syncer.collect(at(T2), false).unwrap();
    assert_eq!(report.unlisted.deleted, 0, "{report:?}");
    assert_eq!(run_objects(b.as_ref()), run, "the objects stay while a retained generation lists them");
    let restore = node("restorer", b.clone(), "");
    restore.syncer.pull(&PullScope { generation: Some(1), ..PullScope::default() }).unwrap();
    assert_eq!(ids(table_rows(&restore.syncer.store, &TableDecl::named("filings"), &["id"]).unwrap()), [1, 2]);
}

/// `[sync] retain_generations` keeps the newest generation manifests, the current one included, and collect deletes
/// every older one; a restore of a deleted generation then names the newest.
// spec: store.collect.generations@e12dca12
#[test]
fn generations_past_the_retained_count_are_deleted() {
    let dir = tempfile::tempdir().unwrap();
    let b = bucket(dir.path());
    let mut a = node("ingest-a", b.clone(), "");
    for i in 0..4 {
        a.land(&format!("run-{i}"), json!([{"id": i}]), T0);
        a.syncer.push(at(T0)).unwrap();
    }
    assert_eq!(manifest(b.as_ref()).generation, 4);
    a.syncer.config.retain_generations = NonZeroU64::new(2);
    let report = a.syncer.collect(at(T1), false).unwrap();
    assert_eq!(report.generations.deleted, 2, "{report:?}");
    let kept: Vec<String> = b.list("team/manifests/").unwrap();
    assert_eq!(kept, [format!("team/{}", generation_key(3)), format!("team/{}", generation_key(4))]);
    match a.syncer.pull(&PullScope { generation: Some(1), ..PullScope::default() }) {
        Err(SyncError::Store(StoreError::SyncGenerationAbsent(m))) => assert!(m.contains('4'), "{m}"),
        other => panic!("{other:?}"),
    }
}

/// An owner restoring a collected run neither lists nor uploads it while the collected record stands.
// spec: store.push.collected@b72ca3a9
#[test]
fn a_push_restoring_a_collected_key_neither_lists_nor_uploads_it() {
    let (_dir, b, a, saved) = tombstoned(Some(1));
    a.syncer.collect(at(T2), false).unwrap();
    assert!(run_objects(b.as_ref()).is_empty());
    restore(&a, &saved);
    a.syncer.push(at(T2)).unwrap();
    let m = manifest(b.as_ref());
    assert!(!m.entries.is_empty(), "the manifest lists the remaining run");
    assert!(!m.entries.keys().any(|k| k.starts_with(RUN)), "the push lists no collected key");
    assert!(run_objects(b.as_ref()).is_empty(), "the push uploads no collected key");
}

/// An owner restoring a run, whose push read the manifest before a collect committed and commits after it deleted
/// the objects, loses its condition and re-merges past the collected record, so no manifest lists a deleted object.
// spec: store.collect.drop-first@062faab9
// spec: store.merge.collected@ae4c5844
#[test]
fn a_push_racing_a_collect_lists_no_object_it_deleted() {
    let (_dir, b, a, saved) = tombstoned(Some(1));
    let mut collector = node("collector", b.clone(), "");
    collector.syncer.config.retain_generations = NonZeroU64::new(1);
    let collector = Arc::new(collector);
    let fired = Arc::new(AtomicBool::new(false));
    let (c, once) = (collector.clone(), fired.clone());
    let script = Script {
        on_put: Some(Box::new(move |key, _| {
            if key == "team/manifest.json" && !once.swap(true, Ordering::SeqCst) {
                c.syncer.collect(at(T2), false).unwrap();
            }
            None
        })),
        ..Script::default()
    };
    let owner = rebucketed(&a, Arc::new(Scripted { inner: b.clone(), script }));
    restore(&a, &saved);
    let pushed = owner.push(at(T2)).unwrap();
    assert!(fired.load(Ordering::SeqCst), "the collect ran between the push's read and its commit");
    assert!(pushed.rounds >= 2, "the push lost its first condition: {pushed:?}");
    let m = manifest(b.as_ref());
    assert!(!m.collected.is_empty() && !m.entries.is_empty(), "the manifest holds records and entries");
    for key in m.entries.keys() {
        assert!(b.get(&format!("team/{key}")).unwrap().is_some(), "`{key}` is listed and its object is gone");
    }
}

/// A lazy store whose pull listed a run the collect then deleted meets `SyncPartCollected`, never a torn read.
// spec: store.collect.drop-first@062faab9
#[test]
fn a_lazy_reader_of_a_collected_run_meets_part_collected() {
    let dir = tempfile::tempdir().unwrap();
    let b = bucket(dir.path());
    let mut a = node("ingest-a", b.clone(), "");
    a.syncer.config.retain_generations = NonZeroU64::new(1);
    a.land("run-1", json!([{"id": 1}]), T0);
    a.syncer.push(at(T0)).unwrap();
    let mut reader = node("reader", b.clone(), "");
    reader.syncer.config.lazy_parts = Some(true);
    reader.syncer.pull(&PullScope::default()).unwrap();
    std::fs::remove_dir_all(a.root().join("tables/filings/data/runs/run-1")).unwrap();
    a.syncer.push(at(T1)).unwrap();
    let report = a.syncer.collect(at(T2), false).unwrap();
    assert!(report.unlisted.deleted > 0, "{report:?}");
    let store = reader.syncer.store.clone().with_part_source(reader.syncer.part_source());
    let err = table_rows(&store, &TableDecl::named("filings"), &["id"]).unwrap_err().to_string();
    assert!(err.starts_with("SyncPartCollected"), "{err}");
}

/// A tombstoned group marker stays in the bucket, since a member run reads as committed only beside it.
// spec: store.collect.unlisted@87dfc65c
#[test]
fn a_tombstoned_group_marker_stays() {
    let dir = tempfile::tempdir().unwrap();
    let b = bucket(dir.path());
    let mut a = node("ingest-a", b.clone(), "");
    a.syncer.config.retain_generations = NonZeroU64::new(1);
    a.land("run-1", json!([{"id": 1}]), T0);
    let marker = "tables/filings/data/runs/run-1/ingest-a/_group.json";
    std::fs::write(a.root().join(marker), br#"["filings","pages"]"#).unwrap();
    a.syncer.push(at(T0)).unwrap();
    std::fs::remove_file(a.root().join(marker)).unwrap();
    a.syncer.push(at(T1)).unwrap();
    assert!(manifest(b.as_ref()).tombstones.contains_key(&format!("research/{marker}")));
    a.syncer.collect(at(T2), false).unwrap();
    assert!(b.get(&format!("team/research/{marker}")).unwrap().is_some(), "the marker stays in the bucket");
    assert!(!manifest(b.as_ref()).collected.contains_key(&format!("research/{marker}")));
}

/// A push of a bucket holding no collected record writes format 1; a collect that records one writes format 2.
// spec: store.push.manifest-format@bd112bbd
#[test]
fn a_collect_writes_format_two_and_a_plain_push_format_one() {
    let (_dir, b, a, _saved) = tombstoned(Some(1));
    let raw = |b: &dyn ObjectStore| serde_json::from_slice::<serde_json::Value>(&b.get("team/manifest.json").unwrap().unwrap().0).unwrap()["format"].clone();
    assert_eq!(raw(b.as_ref()), json!(1), "pushes before any collect keep format 1");
    a.syncer.collect(at(T2), false).unwrap();
    assert_eq!(raw(b.as_ref()), json!(2), "a manifest holding collected records carries format 2");
}

/// A hold renewed after its first push reaches the bucket.
// spec: store.push.hold-renewal@db274eb4
#[test]
fn a_renewed_hold_reaches_the_bucket() {
    let dir = tempfile::tempdir().unwrap();
    let b = bucket(dir.path());
    let a = node("ingest-a", b.clone(), "");
    a.land("run-1", json!([{"id": 1}]), T0);
    let path = a.root().join("tables/filings/holds/build-1.json");
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    let hold = |expires: &str| json!({ "build_id": "build-1", "principal": "user://dana@example.test", "placed_at": T0, "expires_at": expires }).to_string();
    std::fs::write(&path, hold("2030-01-05T00:00:00Z")).unwrap();
    a.syncer.push(at(T0)).unwrap();
    std::fs::write(&path, hold("2030-02-01T00:00:00Z")).unwrap();
    a.syncer.push(at(T1)).unwrap();
    let held = String::from_utf8(b.get("team/research/tables/filings/holds/build-1.json").unwrap().unwrap().0).unwrap();
    assert!(held.contains("2030-02-01"), "the renewal reaches the bucket: {held}");
}

/// Collect heals an absent copy of the read generation before it lists the generations.
// spec: store.collect.generations@e12dca12
#[test]
fn a_collect_heals_the_read_generation() {
    let dir = tempfile::tempdir().unwrap();
    let b = bucket(dir.path());
    let a = node("ingest-a", b.clone(), "");
    a.land("run-1", json!([{"id": 1}]), T0);
    a.syncer.push(at(T0)).unwrap();
    let floor = manifest(b.as_ref()).generation;
    b.delete(&format!("team/{}", generation_key(floor))).unwrap();
    a.syncer.collect(at(T1), false).unwrap();
    assert!(b.get(&format!("team/{}", generation_key(floor))).unwrap().is_some(), "the collect heals the read generation");
}

/// A prefix holding another project's keys keeps every generation manifest, and the report says so.
// spec: store.collect.shared-prefix@b9c9a29b
#[test]
fn generations_stay_while_the_prefix_holds_another_project() {
    let dir = tempfile::tempdir().unwrap();
    let b = bucket(dir.path());
    let mut a = node("ingest-a", b.clone(), "");
    let other = crate::support::node_in("archive", "ingest-o", b.clone(), "");
    for i in 0..3 {
        a.land(&format!("run-{i}"), json!([{"id": i}]), T0);
        a.syncer.push(at(T0)).unwrap();
    }
    other.land("run-o", json!([{"id": 9}]), T0);
    other.syncer.push(at(T0)).unwrap();
    a.syncer.config.retain_generations = NonZeroU64::new(1);
    let before = b.list("team/manifests/").unwrap();
    assert!(before.len() > 1);
    let report = a.syncer.collect(at(T1), true).unwrap();
    assert!(report.shared_prefix && report.generations.deleted == 0, "{report:?}");
    a.syncer.collect(at(T1), false).unwrap();
    assert_eq!(b.list("team/manifests/").unwrap(), before);
}

/// On a shared prefix every generation is retained, so a tombstoned object a kept generation lists stays.
// spec: store.collect.shared-prefix@b9c9a29b
#[test]
fn a_shared_prefix_keeps_what_a_kept_generation_lists() {
    let dir = tempfile::tempdir().unwrap();
    let b = bucket(dir.path());
    let other = crate::support::node_in("archive", "ingest-o", b.clone(), "");
    other.land("run-o", json!([{"id": 9}]), T0);
    other.syncer.push(at(T0)).unwrap();
    let mut a = node("ingest-a", b.clone(), "");
    a.land("run-1", json!([{"id": 1}]), T0);
    a.syncer.push(at(T0)).unwrap();
    let run = run_objects(b.as_ref());
    assert!(!run.is_empty());
    std::fs::remove_dir_all(a.root().join("tables/filings/data/runs/run-1")).unwrap();
    a.syncer.push(at(T1)).unwrap();
    a.syncer.config.retain_generations = NonZeroU64::new(1);
    let report = a.syncer.collect(at(T1), false).unwrap();
    assert!(report.shared_prefix && report.unlisted.deleted == 0, "{report:?}");
    assert_eq!(run_objects(b.as_ref()), run, "a kept generation lists the run");
}

/// A syncer of `n`'s store over `bucket`.
fn rebucketed(n: &Node, bucket: Arc<dyn ObjectStore>) -> contextful_sync::Syncer {
    let s = &n.syncer;
    contextful_sync::Syncer {
        store: s.store.clone(),
        bucket,
        config: s.config.clone(),
        prefix: s.prefix.clone(),
        project: s.project.clone(),
        node: s.node.clone(),
        residency: s.residency.clone(),
        control_dir: s.control_dir.clone(),
        signer: s.signer.clone(),
        key_set: s.key_set.clone(),
        redistribution: s.redistribution.clone(),
    }
}
