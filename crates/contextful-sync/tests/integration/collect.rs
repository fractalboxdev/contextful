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
// spec: store.collect.unlisted@8020c8cf
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
// spec: store.collect.command@2a7db1dd
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
// spec: store.collect.drop-first@840f04c5
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
// spec: store.collect.drop-first@840f04c5
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
// spec: store.collect.unlisted@8020c8cf
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
// spec: store.collect.shared-prefix@474d250d
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
// spec: store.collect.shared-prefix@474d250d
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

fn keyed(name: &str) -> TableDecl {
    TableDecl::parse_pipeline(&format!("[[pipeline.tables]]\nname = \"{name}\"\nprimary_key = [\"id\"]\n")).unwrap().remove(0)
}

fn fold_at(n: &Node, d: &TableDecl, now: &str) -> String {
    use contextful_core::store::fold::FoldOutcome;
    let FoldOutcome::Folded { snapshot_id, .. } = contextful_context::fold::fold(&n.syncer.store, d, at(now)).unwrap() else { panic!("nothing folded") };
    snapshot_id
}

/// A bucket whose `filings` chain is F3 → F2 → F1: B lands every run, A folds and publishes. F1 is superseded at
/// 03:00 on day 0 and F2 on day 9; F1 folds run-1 and F2 run-2. C pulled once before F1 and went away holding run-1.
/// Read unkeyed, the table holds keys 1, 11, 12 and 13 once each; a run read twice adds its keys again.
struct History {
    _dir: tempfile::TempDir,
    b: Arc<dyn ObjectStore>,
    a: Node,
    writer: Node,
    away: Node,
    decl: TableDecl,
    snapshots: Vec<String>,
}

fn history(retain: u64) -> History {
    let dir = tempfile::tempdir().unwrap();
    let b = bucket(dir.path());
    let mut a = node("ingest-a", b.clone(), "");
    a.syncer.config.retain_generations = NonZeroU64::new(retain);
    let writer = node("ingest-b", b.clone(), "");
    let away = node("ingest-c", b.clone(), "");
    let decl = keyed("filings");
    let mut snapshots = Vec::new();
    let steps = [("2030-01-01T00:00:00Z", "2030-01-01T01:00:00Z"), ("2030-01-01T02:00:00Z", "2030-01-01T03:00:00Z"), ("2030-01-10T00:00:00Z", "2030-01-10T01:00:00Z")];
    for (i, (landed, folded)) in steps.into_iter().enumerate() {
        // Each run replaces key 1, so each fold rewrites its parent's part, and adds a key of its own.
        writer.land(&format!("run-{}", i + 1), json!([{"id": 1, "v": i + 1}, {"id": 11 + i}]), landed);
        writer.syncer.push(at(landed)).unwrap();
        if i == 0 {
            away.syncer.pull(&PullScope::default()).unwrap();
        }
        a.syncer.pull(&PullScope::default()).unwrap();
        snapshots.push(fold_at(&a, &decl, folded));
        a.syncer.push(at(folded)).unwrap();
    }
    History { _dir: dir, b, a, writer, away, decl, snapshots }
}

fn listed(m: &BucketManifest, prefix: &str) -> Vec<String> {
    m.entries.keys().filter(|k| k.starts_with(prefix)).cloned().collect()
}

/// Collect deletes every object of a bucket chain snapshot's directory save `_manifest.json` once a child created
/// `retain_runs` ago supersedes it, writing `_collected.json` beside it; a node away since that snapshot reads each
/// folded run once.
// spec: store.collect.superseded@ea7391d1
// spec: store.collect.manifest-kept@bf02fbfc
#[test]
fn a_superseded_snapshot_keeps_its_manifest_and_a_returning_node_reads_each_run_once() {
    let h = history(1);
    let f1 = format!("research/tables/filings/data/snapshots/{}/", h.snapshots[0]);
    let before = listed(&manifest(h.b.as_ref()), &f1);
    assert!(before.iter().any(|k| k.ends_with(".parquet")), "{before:?}");
    let report = h.a.syncer.collect_declared(at("2030-01-11T00:00:00Z"), false, std::slice::from_ref(&h.decl)).unwrap();
    assert!(report.superseded.dropped > 0 && report.superseded.deleted > 0, "{report:?}");
    let m = manifest(h.b.as_ref());
    assert_eq!(listed(&m, &f1), [format!("{f1}_collected.json"), format!("{f1}_manifest.json")]);
    let left = h.b.list(&format!("team/{f1}")).unwrap();
    assert!(!left.is_empty(), "the kept manifest stays in the bucket");
    assert!(left.iter().all(|k| !k.ends_with(".parquet")), "the parts leave the bucket");
    let f2 = format!("research/tables/filings/data/snapshots/{}/", h.snapshots[1]);
    assert!(listed(&m, &f2).iter().any(|k| k.ends_with(".parquet")), "a snapshot superseded inside the window keeps its parts");

    // C still holds run-1 and no snapshot; its pull reaches F1's kept manifest, which folds run-1.
    assert!(h.away.root().join("tables/filings/data/runs/run-1/ingest-b/_manifest.json").exists());
    h.away.syncer.pull(&PullScope::default()).unwrap();
    assert_eq!(ids(table_rows(&h.away.syncer.store, &TableDecl::named("filings"), &["id"]).unwrap()), [1, 11, 12, 13]);
    let cold = node("reader", h.b.clone(), "");
    cold.syncer.pull(&PullScope::default()).unwrap();
    assert_eq!(cold.syncer.store.chain("filings").unwrap().0.len(), 3, "the chain reaches its root");
    assert_eq!(ids(table_rows(&cold.syncer.store, &TableDecl::named("filings"), &["id"]).unwrap()), [1, 11, 12, 13]);
}

/// Collect deletes a run directory, `_manifest.json` included, once a chain snapshot created `retain_runs` ago names
/// it; a run its owner lists again reads once.
// spec: store.collect.folded-runs@697af6bf
#[test]
fn a_folded_run_leaves_whole_and_a_relisted_copy_reads_once() {
    let h = history(1);
    let saved = tempfile::tempdir().unwrap();
    copy_dir(&h.writer.root().join("tables/filings/data/runs/run-1"), saved.path());
    let report = h.a.syncer.collect_declared(at("2030-01-11T00:00:00Z"), false, std::slice::from_ref(&h.decl)).unwrap();
    assert!(report.folded_runs.dropped > 0 && report.folded_runs.deleted > 0, "{report:?}");
    let m = manifest(h.b.as_ref());
    assert!(listed(&m, "research/tables/filings/data/runs/run-1/").is_empty());
    assert!(listed(&m, "research/tables/filings/data/runs/run-2/").is_empty());
    assert!(!listed(&m, "research/tables/filings/data/runs/run-3/").is_empty(), "a run folded inside the window stays");
    assert!(h.b.list("team/research/tables/filings/data/runs/run-1/").unwrap().is_empty());

    // Past the record's age the owner's restored copy is listed again, and every reader folds it.
    copy_dir(saved.path(), &h.writer.root().join("tables/filings/data/runs/run-1"));
    h.writer.syncer.push(at("2030-03-01T00:00:00Z")).unwrap();
    assert!(!listed(&manifest(h.b.as_ref()), "research/tables/filings/data/runs/run-1/").is_empty(), "the owner lists run-1 again");
    let cold = node("reader", h.b.clone(), "");
    cold.syncer.pull(&PullScope::default()).unwrap();
    assert_eq!(ids(table_rows(&cold.syncer.store, &TableDecl::named("filings"), &["id"]).unwrap()), [1, 11, 12, 13]);
}

/// After a collect reclaims folded runs, a table-scoped pull still takes every node's run state, the cursor source a
/// collected run's manifest no longer carries, and pulls none of the collected keys.
#[test]
fn a_table_scoped_pull_after_a_collect_takes_run_states_and_no_collected_key() {
    let h = history(1);
    let state = json!({
        "format": 1,
        "node_id": "ingest-b",
        "runs": {"feed": {"run_id": "run-1", "table": "filings", "status": "succeeded", "started_at": "2030-01-01T00:00:00Z"}},
        "cursors": [{"pipeline_id": "feed", "table": "filings", "position": 41, "run_id": "run-1", "committed_at": "2030-01-01T00:00:00Z"}]
    });
    std::fs::create_dir_all(h.writer.root().join("nodes/ingest-b")).unwrap();
    std::fs::write(h.writer.root().join("nodes/ingest-b/run-state.json"), state.to_string()).unwrap();
    h.writer.syncer.push(at("2030-01-10T02:00:00Z")).unwrap();
    h.a.syncer.collect_declared(at("2030-01-11T00:00:00Z"), false, std::slice::from_ref(&h.decl)).unwrap();
    let m = manifest(h.b.as_ref());
    assert!(m.entries.contains_key("research/nodes/ingest-b/run-state.json"));
    assert!(!m.collected.is_empty());
    let cold = node("reader", h.b.clone(), "");
    cold.syncer.pull(&PullScope { tables: vec![contextful_core::grant::TablePattern::parse("filings").unwrap()], ..PullScope::default() }).unwrap();
    assert!(cold.root().join("nodes/ingest-b/run-state.json").exists());
    assert!(!cold.root().join("tables/filings/data/runs/run-1").exists());
    assert!(m.collected.keys().all(|k| !cold.root().join(k.strip_prefix("research/").unwrap()).exists()), "no collected key arrives");
    assert_eq!(ids(table_rows(&cold.syncer.store, &TableDecl::named("filings"), &["id"]).unwrap()), [1, 11, 12, 13]);
}

/// A collect records a listed key first and deletes its object only once no retained generation lists it, and a
/// record stands, stamped again, while its object stays.
// spec: store.collect.two-phase@d57fe42e
// spec: store.collect.record-stands@0bd76699
#[test]
fn a_collect_unlists_first_and_deletes_once_no_retained_generation_lists_the_key() {
    let h = history(3);
    let f1 = format!("research/tables/filings/data/snapshots/{}/", h.snapshots[0]);
    let parts: Vec<String> = listed(&manifest(h.b.as_ref()), &f1).into_iter().filter(|k| k.ends_with(".parquet")).collect();
    assert!(!parts.is_empty());
    let decls = std::slice::from_ref(&h.decl);
    let first = h.a.syncer.collect_declared(at("2030-01-11T00:00:00Z"), false, decls).unwrap();
    assert!(first.superseded.dropped > 0 && first.superseded.deleted == 0, "{first:?}");
    let m = manifest(h.b.as_ref());
    assert!(parts.iter().all(|k| !m.entries.contains_key(k) && m.collected.contains_key(k)));
    assert!(parts.iter().all(|k| h.b.get(&format!("team/{k}")).unwrap().is_some()), "a retained generation lists the parts");

    // Twenty days on the pending records are stamped again, so they outlive the age a record keeps.
    h.a.syncer.collect_declared(at("2030-01-31T00:00:00Z"), false, decls).unwrap();
    let m = manifest(h.b.as_ref());
    assert!(parts.iter().all(|k| m.collected[k].collected_at == at("2030-01-31T00:00:00Z")), "{:?}", m.collected);

    // Three more commits age every generation listing the parts out of the retained count.
    for i in 4..7 {
        h.writer.land(&format!("run-{i}"), json!([{"id": i}]), "2030-02-01T00:00:00Z");
        h.writer.syncer.push(at("2030-02-01T00:00:00Z")).unwrap();
    }
    let last = h.a.syncer.collect_declared(at("2030-02-02T00:00:00Z"), false, decls).unwrap();
    assert!(last.superseded.deleted >= parts.len(), "{last:?}");
    assert!(parts.iter().all(|k| h.b.get(&format!("team/{k}")).unwrap().is_none()));
}

/// Collect reclaims only snapshots the bucket pointer reaches: a run that only an unpublished snapshot folds stays.
// spec: store.collect.chain-only@b36eacf2
#[test]
fn a_run_only_an_unpublished_snapshot_folds_stays() {
    let dir = tempfile::tempdir().unwrap();
    let b = bucket(dir.path());
    let mut a = node("ingest-a", b.clone(), "");
    a.syncer.config.retain_generations = NonZeroU64::new(1);
    let away = node("ingest-x", b.clone(), "");
    let decl = keyed("filings");
    a.land("run-1", json!([{"id": 1}]), T0);
    fold_at(&a, &decl, "2030-01-01T01:00:00Z");
    a.syncer.push(at("2030-01-01T01:00:00Z")).unwrap();
    // X folds its own run off the bucket's chain; its push uploads the snapshot and keeps the pointer local.
    away.land("run-x", json!([{"id": 7}]), T0);
    fold_at(&away, &decl, "2030-01-01T02:00:00Z");
    assert!(away.syncer.push(at("2030-01-01T02:00:00Z")).unwrap().pointers.is_empty());
    let run_x = "research/tables/filings/data/runs/run-x/";
    assert!(!listed(&manifest(b.as_ref()), run_x).is_empty());
    let report = a.syncer.collect_declared(at("2030-01-11T00:00:00Z"), false, std::slice::from_ref(&decl)).unwrap();
    assert!(report.folded_runs.dropped > 0, "{report:?}");
    let m = manifest(b.as_ref());
    assert!(listed(&m, "research/tables/filings/data/runs/run-1/").is_empty(), "the chain's folded run leaves");
    assert!(!listed(&m, run_x).is_empty(), "the unpublished snapshot's run stays");
}

/// Local retention on a synced store writes no `_collected.json` and its push lists none, so another node pulling
/// the snapshot reads it `as_of`; once a bucket collect lists the marker, that node's pull brings it down and the
/// read refuses. A pull bringing the parts home again lets the retaining node read the snapshot.
// spec: store.fold.collected-snapshot@bc6b164e
#[test]
fn only_a_bucket_collect_marks_a_snapshot_collected() {
    use contextful_core::store::bound_time::{Bound, Bounds};
    let dir = tempfile::tempdir().unwrap();
    let b = bucket(dir.path());
    let mut a = node("ingest-a", b.clone(), "[sync]\nendpoint = \"file://bucket\"\nbucket = \"context-team\"\n");
    a.syncer.config.retain_generations = NonZeroU64::new(1);
    assert!(a.syncer.store.synced());
    let decl = keyed("filings");
    let mut snapshots = Vec::new();
    let steps = [("2030-01-01T00:00:00Z", "2030-01-01T01:00:00Z"), ("2030-01-01T02:00:00Z", "2030-01-01T03:00:00Z"), ("2030-01-10T00:00:00Z", "2030-01-10T01:00:00Z")];
    for (i, (landed, folded)) in steps.into_iter().enumerate() {
        a.land(&format!("run-{}", i + 1), json!([{"id": 1, "v": i + 1}, {"id": 11 + i}]), landed);
        snapshots.push(fold_at(&a, &decl, folded));
        a.syncer.push(at(folded)).unwrap();
    }
    let (chain, _) = a.syncer.store.chain("filings").unwrap();
    let first = chain.last().unwrap().clone();
    assert_eq!(first.snapshot_id.to_string(), snapshots[0]);
    assert!(a.syncer.store.snapshot_collected("filings", &first).unwrap(), "local retention removed its parts");
    let f1 = format!("research/tables/filings/data/snapshots/{}/", snapshots[0]);
    let m = manifest(b.as_ref());
    assert!(listed(&m, &f1).iter().any(|k| k.ends_with(".parquet")), "the bucket keeps every part");
    assert!(!m.entries.contains_key(&format!("{f1}_collected.json")), "the push lists no local marker");

    let bound = Bounds { as_of: Some(Bound::parse("2030-01-01T01:30:00Z").unwrap()), ..Bounds::default() };
    let reader = node("reader", b.clone(), "");
    reader.syncer.pull(&PullScope::default()).unwrap();
    assert!(!reader.syncer.store.snapshot_collected("filings", &first).unwrap());
    assert!(contextful_context::scan::scan(&reader.syncer.store, &decl, bound).is_ok(), "another node reads the snapshot as of its time");

    // The retaining node's pull brings the parts home again.
    a.syncer.pull(&PullScope::default()).unwrap();
    assert!(!a.syncer.store.snapshot_collected("filings", &first).unwrap());
    assert!(contextful_context::scan::scan(&a.syncer.store, &decl, bound).is_ok());

    a.syncer.collect_declared(at("2030-01-11T00:00:00Z"), false, std::slice::from_ref(&decl)).unwrap();
    assert!(manifest(b.as_ref()).entries.contains_key(&format!("{f1}_collected.json")));
    reader.syncer.pull(&PullScope::default()).unwrap();
    assert!(reader.syncer.store.snapshot_collected("filings", &first).unwrap());
    let err = contextful_context::scan::scan(&reader.syncer.store, &decl, bound).unwrap_err().to_string();
    assert!(err.starts_with("StoreAsOfUnretained"), "{err}");
}

/// A table the collecting store declares no window for keeps its snapshots and runs, and the report names it.
// spec: store.collect.declared-window@1ed7ca51
#[test]
fn an_undeclared_table_keeps_its_snapshots_and_runs() {
    let h = history(1);
    let before = manifest(h.b.as_ref());
    let report = h.a.syncer.collect(at("2030-01-11T00:00:00Z"), false).unwrap();
    assert_eq!(report.undeclared, ["filings"]);
    assert_eq!((report.superseded.dropped, report.folded_runs.dropped), (0, 0), "{report:?}");
    let after = manifest(h.b.as_ref());
    assert_eq!(listed(&after, "research/tables/filings/data/"), listed(&before, "research/tables/filings/data/"));
}

/// On a shared prefix a collect unlists nothing.
// spec: store.collect.shared-prefix@474d250d
#[test]
fn a_shared_prefix_unlists_nothing() {
    let h = history(1);
    let other = crate::support::node_in("archive", "ingest-o", h.b.clone(), "");
    other.land("run-o", json!([{"id": 9}]), T0);
    other.syncer.push(at(T0)).unwrap();
    let before = manifest(h.b.as_ref());
    let report = h.a.syncer.collect_declared(at("2030-01-11T00:00:00Z"), false, std::slice::from_ref(&h.decl)).unwrap();
    assert!(report.shared_prefix && report.superseded.dropped == 0 && report.folded_runs.dropped == 0, "{report:?}");
    assert_eq!(manifest(h.b.as_ref()).entries, before.entries);
}

/// A folded run's `_group.json` stays while a member table holding a copy of the run folds it in no snapshot older
/// than its window, and leaves once every member does.
// spec: store.collect.group-window@aed6aa20
#[test]
fn a_group_marker_waits_for_every_member_fold_to_age_past_the_window() {
    let dir = tempfile::tempdir().unwrap();
    let b = bucket(dir.path());
    let mut a = node("ingest-a", b.clone(), "");
    a.syncer.config.retain_generations = NonZeroU64::new(1);
    let writer = node("ingest-b", b.clone(), "");
    let (filings, pages) = (keyed("filings"), keyed("pages"));
    writer.land_into("filings", "run-1", json!([{"id": 1}]), T0);
    writer.land_into("pages", "run-1", json!([{"id": 1}]), T0);
    std::fs::write(writer.root().join("tables/filings/data/runs/run-1/ingest-b/_group.json"), br#"["filings","pages"]"#).unwrap();
    writer.syncer.push(at(T0)).unwrap();
    a.syncer.pull(&PullScope::default()).unwrap();
    fold_at(&a, &filings, "2030-01-01T01:00:00Z");
    a.syncer.push(at("2030-01-01T01:00:00Z")).unwrap();
    // Pages folds its copy nine days later.
    fold_at(&a, &pages, "2030-01-10T00:00:00Z");
    a.syncer.push(at("2030-01-10T00:00:00Z")).unwrap();
    let marker = "research/tables/filings/data/runs/run-1/ingest-b/_group.json";
    let decls = [filings.clone(), pages.clone()];
    a.syncer.collect_declared(at("2030-01-11T00:00:00Z"), false, &decls).unwrap();
    let m = manifest(b.as_ref());
    assert!(m.entries.contains_key(marker), "the marker stays while the member's fold is younger than the window");
    assert_eq!(listed(&m, "research/tables/filings/data/runs/run-1/"), [marker], "the rest of the root run leaves");
    a.syncer.collect_declared(at("2030-01-18T00:00:00Z"), false, &decls).unwrap();
    let m = manifest(b.as_ref());
    assert!(!m.entries.contains_key(marker), "the marker leaves once every member's fold is a window old");
    assert!(listed(&m, "research/tables/pages/data/runs/run-1/").is_empty());
}
