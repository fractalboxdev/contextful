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
// spec: store.collect.command@7a6ebce7
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
// spec: store.push.collected@e5cb8ab7
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

/// Collect deletes a run directory, `_manifest.json` included, once the bucket chain snapshot folding the run
/// supersedes a snapshot collect reclaims; a run the root snapshot folds stays.
// spec: store.collect.folded-runs@96c39058
#[test]
fn a_folded_run_leaves_whole_once_its_folding_snapshot_supersedes_a_reclaimed_one() {
    let h = history(1);
    let report = h.a.syncer.collect_declared(at("2030-01-11T00:00:00Z"), false, std::slice::from_ref(&h.decl)).unwrap();
    assert!(report.folded_runs.dropped > 0 && report.folded_runs.deleted > 0, "{report:?}");
    let m = manifest(h.b.as_ref());
    assert!(!listed(&m, "research/tables/filings/data/runs/run-1/").is_empty(), "the root snapshot's run stays");
    assert!(listed(&m, "research/tables/filings/data/runs/run-2/").is_empty());
    assert!(!listed(&m, "research/tables/filings/data/runs/run-3/").is_empty(), "a run folded inside the window stays");
    assert!(h.b.list("team/research/tables/filings/data/runs/run-2/").unwrap().is_empty());
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
    assert!(!cold.root().join("tables/filings/data/runs/run-2").exists());
    assert!(m.collected.keys().all(|k| !cold.root().join(k.strip_prefix("research/").unwrap()).exists()), "no collected key arrives");
    assert_eq!(ids(table_rows(&cold.syncer.store, &TableDecl::named("filings"), &["id"]).unwrap()), [1, 11, 12, 13]);
}

/// A collect records a listed key first and deletes its object only once no retained generation lists it, and a
/// record stands, stamped again, while its object stays.
// spec: store.collect.two-phase@d57fe42e
// spec: store.collect.record-stands@235241cd
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
    a.land("run-1", json!([{"id": 1, "v": 1}]), T0);
    fold_at(&a, &decl, "2030-01-01T01:00:00Z");
    a.land("run-2", json!([{"id": 1, "v": 2}]), "2030-01-01T02:00:00Z");
    fold_at(&a, &decl, "2030-01-01T03:00:00Z");
    a.syncer.push(at("2030-01-01T03:00:00Z")).unwrap();
    // X folds its own runs off the bucket's chain, twice; its push uploads the snapshots and keeps the pointer local.
    away.land("run-x", json!([{"id": 7}]), T0);
    fold_at(&away, &decl, "2030-01-01T04:00:00Z");
    away.land("run-y", json!([{"id": 8}]), T0);
    fold_at(&away, &decl, "2030-01-01T05:00:00Z");
    assert!(away.syncer.push(at("2030-01-01T05:00:00Z")).unwrap().pointers.is_empty());
    let run_y = "research/tables/filings/data/runs/run-y/";
    assert!(!listed(&manifest(b.as_ref()), run_y).is_empty());
    let report = a.syncer.collect_declared(at("2030-01-11T00:00:00Z"), false, std::slice::from_ref(&decl)).unwrap();
    assert!(report.folded_runs.dropped > 0, "{report:?}");
    let m = manifest(b.as_ref());
    assert!(listed(&m, "research/tables/filings/data/runs/run-2/").is_empty(), "the chain's folded run leaves");
    assert!(!listed(&m, run_y).is_empty(), "the unpublished snapshot's run stays");
}

/// Local retention on a synced store writes no `_collected.json` and its push lists none, so another node pulling
/// the snapshot does not count it collected, and a pull bringing the parts home stops the retaining node counting
/// it collected; a bound needing the runs the owner's retention removed still refuses on both. Once a bucket
/// collect lists the marker, a pull brings it down.
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
    // The bound still refuses there: the run the next snapshot folds left the bucket by its owner's retention.
    let err = contextful_context::scan::scan(&reader.syncer.store, &decl, bound).unwrap_err().to_string();
    assert!(err.starts_with("StoreAsOfUnretained"), "{err}");

    // The retaining node's pull brings the parts home again.
    a.syncer.pull(&PullScope::default()).unwrap();
    assert!(!a.syncer.store.snapshot_collected("filings", &first).unwrap());
    let err = contextful_context::scan::scan(&a.syncer.store, &decl, bound).unwrap_err().to_string();
    assert!(err.starts_with("StoreAsOfUnretained"), "{err}");

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

/// A folded run's `_group.json` stays while a member table holding a copy of the run has not folded it out as the
/// root table did, and leaves once every member has.
// spec: store.collect.group-window@32b5cbcc
#[test]
fn a_group_marker_waits_for_every_member_fold_to_age_past_the_window() {
    let dir = tempfile::tempdir().unwrap();
    let b = bucket(dir.path());
    let mut a = node("ingest-a", b.clone(), "");
    a.syncer.config.retain_generations = NonZeroU64::new(1);
    let writer = node("ingest-b", b.clone(), "");
    let (filings, pages) = (keyed("filings"), keyed("pages"));
    // Each table's first fold, so the folds of run-1 supersede a snapshot collect can reclaim.
    writer.land_into("filings", "run-0", json!([{"id": 0}]), T0);
    writer.land_into("pages", "run-0", json!([{"id": 0}]), T0);
    writer.syncer.push(at(T0)).unwrap();
    a.syncer.pull(&PullScope::default()).unwrap();
    fold_at(&a, &filings, "2030-01-01T00:30:00Z");
    fold_at(&a, &pages, "2030-01-01T00:30:00Z");
    a.syncer.push(at("2030-01-01T00:30:00Z")).unwrap();
    writer.land_into("filings", "run-1", json!([{"id": 1}]), T0);
    writer.land_into("pages", "run-1", json!([{"id": 1}]), T0);
    std::fs::write(writer.root().join("tables/filings/data/runs/run-1/ingest-b/_group.json"), br#"["filings","pages"]"#).unwrap();
    writer.syncer.push(at("2030-01-01T00:40:00Z")).unwrap();
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
    assert!(!m.entries.contains_key(marker), "the marker leaves once every member folded the run out");
    assert!(listed(&m, "research/tables/pages/data/runs/run-1/").is_empty());
}

/// A collect whose manifest commit loses to a concurrent collect overwrites no `_collected.json` that collect
/// listed, so every pull verifies the marker against its listed digest.
// spec: store.collect.marker-first-writer@d1732d16
#[test]
fn a_losing_collect_overwrites_no_listed_marker() {
    use std::sync::atomic::AtomicUsize;
    let h = history(3);
    let stale = h.b.get("team/manifest.json").unwrap().unwrap();
    h.a.syncer.collect_declared(at("2030-01-11T00:00:00Z"), false, std::slice::from_ref(&h.decl)).unwrap();
    let marker = format!("research/tables/filings/data/snapshots/{}/_collected.json", h.snapshots[0]);
    let listed_digest = manifest(h.b.as_ref()).entries[&marker].sha256.clone();
    // A second collect read the manifest before the first committed: its first two reads see that manifest.
    let reads = Arc::new(AtomicUsize::new(0));
    let seen = reads.clone();
    let script = Script {
        on_get: Some(Box::new(move |key| (key == "team/manifest.json" && seen.fetch_add(1, Ordering::SeqCst) < 2).then(|| Ok(Some(stale.clone()))))),
        ..Script::default()
    };
    let racing = rebucketed(&h.writer, Arc::new(Scripted { inner: h.b.clone(), script }));
    racing.collect_declared(at("2030-01-11T06:00:00Z"), false, std::slice::from_ref(&h.decl)).unwrap();
    assert!(reads.load(Ordering::SeqCst) >= 2, "the second collect read the stale manifest");
    let (bytes, _) = h.b.get(&format!("team/{marker}")).unwrap().unwrap();
    assert_eq!(contextful_core::run::journal::sha256_hex(&bytes), listed_digest, "the listed marker keeps its bytes");
    node("reader", h.b.clone(), "").syncer.pull(&PullScope::default()).unwrap();
}

/// A push whose plan skipped a key a standing collected record named uploads it before a later round commits
/// the key, once that round's manifest no longer records it, so no listed key lacks its object.
// spec: store.push.collected@e5cb8ab7
#[test]
fn a_push_uploads_a_key_its_commit_lists_after_the_record_leaves() {
    use std::sync::atomic::AtomicUsize;
    let dir = tempfile::tempdir().unwrap();
    let b = bucket(dir.path());
    let a = node("ingest-a", b.clone(), "");
    a.land("run-1", json!([{"id": 1}]), T0);
    a.syncer.push(at(T0)).unwrap();
    // A finished collect: run-1 recorded as collected and its objects gone.
    let mut m = manifest(b.as_ref());
    let keys: Vec<String> = m.entries.keys().filter(|k| k.starts_with(RUN)).cloned().collect();
    assert!(!keys.is_empty());
    for k in &keys {
        m.entries.remove(k);
        m.collected.insert(k.clone(), contextful_core::store::sync::Collected { collected_at: at(T1), collected_by: "ingest-z".into() });
        b.delete(&format!("team/{k}")).unwrap();
    }
    m.format = 2;
    b.put("team/manifest.json", &serde_json::to_vec(&m).unwrap(), contextful_core::store::object::Condition::None).unwrap();
    // The push plans against that manifest; before its commit round reads, another commit prunes the records.
    let reads = Arc::new(AtomicUsize::new(0));
    let (seen, inner) = (reads.clone(), b.clone());
    let script = Script {
        on_get: Some(Box::new(move |key| {
            if key == "team/manifest.json" && seen.fetch_add(1, Ordering::SeqCst) == 1 {
                let mut m: BucketManifest = serde_json::from_slice(&inner.get(key).unwrap().unwrap().0).unwrap();
                m.collected.clear();
                m.format = 1;
                inner.put(key, &serde_json::to_vec(&m).unwrap(), contextful_core::store::object::Condition::None).unwrap();
            }
            None
        })),
        ..Script::default()
    };
    let pushing = rebucketed(&a, Arc::new(Scripted { inner: b.clone(), script }));
    pushing.push(at(T1)).unwrap();
    let m = manifest(b.as_ref());
    let relisted: Vec<&String> = m.entries.keys().filter(|k| k.starts_with(RUN)).collect();
    assert!(!relisted.is_empty(), "the commit lists run-1 again");
    for k in relisted {
        assert!(b.get(&format!("team/{k}")).unwrap().is_some(), "`{k}` is listed without its object");
    }
}

/// A record lapses once its object is gone, and a copy pushed later stays out structurally: a superseded part under
/// a listed `_collected.json`, and a run the bucket chain folds out.
// spec: store.merge.collected-directory@d7221403
#[test]
fn a_copy_pushed_after_its_record_lapses_stays_out() {
    let h = history(1);
    let holder = node("holder", h.b.clone(), "");
    holder.syncer.pull(&PullScope::default()).unwrap();
    let f1 = format!("tables/filings/data/snapshots/{}/", h.snapshots[0]);
    let parts: Vec<String> = std::fs::read_dir(holder.root().join(&f1)).unwrap().map(|e| e.unwrap().file_name().to_string_lossy().into_owned()).filter(|n| n.ends_with(".parquet")).collect();
    assert!(!parts.is_empty(), "the holder keeps copies of F1's parts");
    h.a.syncer.collect_declared(at("2030-01-11T00:00:00Z"), false, std::slice::from_ref(&h.decl)).unwrap();
    let m = manifest(h.b.as_ref());
    assert!(listed(&m, "research/tables/filings/data/runs/run-2/").is_empty());
    // Forty days on, the records have lapsed; the owner's copy of run-2 and the holder's parts push again.
    h.a.syncer.collect_declared(at("2030-02-20T00:00:00Z"), false, std::slice::from_ref(&h.decl)).unwrap();
    assert_eq!(manifest(h.b.as_ref()).collected.keys().filter(|k| k.contains("/runs/run-2/")).count(), 0, "the run's records lapsed");
    assert!(h.writer.root().join("tables/filings/data/runs/run-2/ingest-b/_manifest.json").exists());
    h.writer.syncer.push(at("2030-02-21T00:00:00Z")).unwrap();
    holder.syncer.push(at("2030-02-21T00:00:00Z")).unwrap();
    let m = manifest(h.b.as_ref());
    assert!(listed(&m, "research/tables/filings/data/runs/run-2/").is_empty(), "the folded-out run stays unlisted");
    for part in &parts {
        assert!(!m.entries.contains_key(&format!("research/{f1}{part}")), "`{part}` stays unlisted");
        assert!(h.b.get(&format!("team/research/{f1}{part}")).unwrap().is_none(), "`{part}` is not uploaded again");
    }
}

/// A recorded key a retained snapshot names keeps its object at the delete step, reported, and stays unlisted: a
/// refusal, not a loss.
// spec: store.collect.retained-kept@c46c0379
#[test]
fn a_recorded_key_a_retained_snapshot_names_keeps_its_object() {
    let h = history(3);
    let decls = std::slice::from_ref(&h.decl);
    let f1 = format!("research/tables/filings/data/snapshots/{}/", h.snapshots[0]);
    let parts: Vec<String> = listed(&manifest(h.b.as_ref()), &f1).into_iter().filter(|k| k.ends_with(".parquet")).collect();
    assert!(!parts.is_empty());
    let first = h.a.syncer.collect_declared(at("2030-01-11T00:00:00Z"), false, decls).unwrap();
    assert!(first.superseded.dropped > 0 && first.superseded.deleted == 0, "{first:?}");
    // A hold reaches the bucket around the bucket-first rule, after the record.
    let key = format!("research/tables/filings/holds/{}.json", h.snapshots[0]);
    let hold = json!({"build_id": h.snapshots[0], "principal": "ops", "placed_at": "2030-01-11T01:00:00Z", "expires_at": "2030-02-20T00:00:00Z"}).to_string();
    h.b.put(&format!("team/{key}"), hold.as_bytes(), contextful_core::store::object::Condition::None).unwrap();
    let mut m = manifest(h.b.as_ref());
    m.entries.insert(key, contextful_core::store::sync::Entry { sha256: contextful_core::run::journal::sha256_hex(hold.as_bytes()), size: hold.len() as u64, owner: String::new() });
    h.b.put("team/manifest.json", &serde_json::to_vec(&m).unwrap(), contextful_core::store::object::Condition::None).unwrap();
    for i in 4..7 {
        h.writer.land(&format!("run-{i}"), json!([{"id": 20 + i}]), "2030-01-12T00:00:00Z");
        h.writer.syncer.push(at("2030-01-12T00:00:00Z")).unwrap();
    }
    let last = h.a.syncer.collect_declared(at("2030-01-12T01:00:00Z"), false, decls).unwrap();
    assert!(parts.iter().all(|k| last.referenced.contains(k)), "{last:?}");
    let m = manifest(h.b.as_ref());
    for k in &parts {
        assert!(h.b.get(&format!("team/{k}")).unwrap().is_some(), "`{k}` was deleted under a hold");
        assert!(!m.entries.contains_key(k), "`{k}` stays unlisted");
    }
}

/// Collect serializes on the bucket's collect lease: a second collect refuses while one holds it.
// spec: store.collect.serialized@039d7b65
#[test]
fn a_second_collect_refuses_while_one_holds_the_lease() {
    let h = history(1);
    let held = h.a.syncer.acquire_collect(at("2030-01-11T00:00:00Z")).unwrap();
    assert!(held.is_some(), "the bucket's conditional writes span clients");
    let err = h.writer.syncer.collect_declared(at("2030-01-11T00:01:00Z"), false, std::slice::from_ref(&h.decl)).unwrap_err();
    assert!(err.to_string().starts_with("LeaseHeld"), "{err}");
    h.a.syncer.release_collect(held.as_ref().unwrap()).unwrap();
    let report = h.writer.syncer.collect_declared(at("2030-01-11T00:02:00Z"), false, std::slice::from_ref(&h.decl)).unwrap();
    assert!(report.superseded.dropped > 0, "{report:?}");
}

/// Collect keeps a folded-runs segment a retained snapshot carries from a snapshot it reclaims, so a reader still stops
/// at the head's index.
// spec: store.fold.reference-retention@44e6f5f5
#[test]
fn a_collect_keeps_a_carried_folded_runs_segment() {
    let dir = tempfile::tempdir().unwrap();
    let b = bucket(dir.path());
    let mut a = node("ingest-a", b.clone(), "");
    a.syncer.config.retain_generations = NonZeroU64::new(1);
    let decl = keyed("filings");
    // F1 folds enough runs to flush its index into a segment its successors carry.
    for i in 0..70 {
        a.land(&format!("run-{i:03}"), json!([{"id": i}]), T0);
    }
    let f1 = fold_at(&a, &decl, "2030-01-01T01:00:00Z");
    a.syncer.push(at("2030-01-01T01:00:00Z")).unwrap();
    for (run, landed, folded) in [("run-a", "2030-01-01T02:00:00Z", "2030-01-01T03:00:00Z"), ("run-b", "2030-01-10T00:00:00Z", "2030-01-10T01:00:00Z")] {
        a.land(run, json!([{"id": 100}]), landed);
        fold_at(&a, &decl, folded);
        a.syncer.push(at(folded)).unwrap();
    }
    let segment = format!("research/tables/filings/data/snapshots/{f1}/folded/segment-00000.runs");
    assert!(manifest(b.as_ref()).entries.contains_key(&segment), "F1 flushed a segment");
    let report = a.syncer.collect_declared(at("2030-01-11T00:00:00Z"), false, std::slice::from_ref(&decl)).unwrap();
    assert!(manifest(b.as_ref()).entries.contains_key(&format!("research/tables/filings/data/snapshots/{f1}/_collected.json")), "F1 is reclaimed: {report:?}");
    assert!(manifest(b.as_ref()).entries.contains_key(&segment) && b.get(&format!("team/{segment}")).unwrap().is_some(), "the carried segment stays");
    let cold = node("reader", b.clone(), "");
    cold.syncer.pull(&PullScope::default()).unwrap();
    let state = cold.syncer.store.state(&decl).unwrap();
    assert_eq!(state.index.as_ref().map(|ix| ix.at), Some(0), "the head's index reads valid");
}

/// Collect records no metadata key, a tombstoned one included: a hold, a schema and a snapshot manifest stay.
// spec: store.collect.metadata-kept@39422932
#[test]
fn a_collect_records_no_metadata_key() {
    let h = history(1);
    let hold = "research/tables/filings/holds/snapshot-00000000000000000009.json";
    h.b.put(&format!("team/{hold}"), b"{}", contextful_core::store::object::Condition::None).unwrap();
    let f1 = format!("research/tables/filings/data/snapshots/{}/_manifest.json", h.snapshots[0]);
    let mut m = manifest(h.b.as_ref());
    for k in [hold.to_string(), "research/tables/filings/schema.json".to_string(), f1.clone()] {
        m.entries.remove(&k);
        m.tombstones.insert(k, contextful_core::store::sync::Tombstone::unsigned("ingest-z", at("2030-01-10T00:00:00Z")));
    }
    h.b.put("team/manifest.json", &serde_json::to_vec(&m).unwrap(), contextful_core::store::object::Condition::None).unwrap();
    h.a.syncer.collect_declared(at("2030-01-11T00:00:00Z"), false, std::slice::from_ref(&h.decl)).unwrap();
    let m = manifest(h.b.as_ref());
    for k in [hold, "research/tables/filings/schema.json", f1.as_str()] {
        assert!(!m.collected.contains_key(k), "`{k}` was recorded");
        assert!(h.b.get(&format!("team/{k}")).unwrap().is_some(), "`{k}` was deleted");
    }
}

/// A push's chain walk allows a clock skew between a run's commit and the snapshot folding it, so a run committed
/// just after its fold's instant stays out once a collect reclaimed it.
#[test]
fn a_push_walk_allows_clock_skew_between_a_run_and_its_fold() {
    let dir = tempfile::tempdir().unwrap();
    let b = bucket(dir.path());
    let mut a = node("ingest-a", b.clone(), "");
    a.syncer.config.retain_generations = NonZeroU64::new(1);
    let writer = node("ingest-b", b.clone(), "");
    let decl = keyed("filings");
    // run-2 commits 20 s after the instant of the snapshot folding it: the writer's clock runs ahead.
    for (run, landed, folded) in [("run-1", "2030-01-01T01:00:00Z", "2030-01-01T01:30:00Z"), ("run-2", "2030-01-01T02:00:20Z", "2030-01-01T02:00:00Z"), ("run-3", "2030-01-10T00:00:00Z", "2030-01-10T01:00:00Z")] {
        let own: i64 = run.trim_start_matches("run-").parse().unwrap();
        writer.land(run, json!([{"id": 1, "v": own}, {"id": 100 + own}]), landed);
        writer.syncer.push(at(landed)).unwrap();
        a.syncer.pull(&PullScope::default()).unwrap();
        fold_at(&a, &decl, folded);
        a.syncer.push(at(folded)).unwrap();
    }
    a.syncer.collect_declared(at("2030-01-11T00:00:00Z"), false, std::slice::from_ref(&decl)).unwrap();
    assert!(listed(&manifest(b.as_ref()), "research/tables/filings/data/runs/run-2/").is_empty());
    // The records lapse and a push prunes them; the writer's copy of run-2 stays out.
    a.syncer.push(at("2030-02-20T00:00:00Z")).unwrap();
    assert_eq!(manifest(b.as_ref()).collected.keys().filter(|k| k.contains("/runs/run-2/")).count(), 0);
    writer.syncer.push(at("2030-02-21T00:00:00Z")).unwrap();
    assert!(listed(&manifest(b.as_ref()), "research/tables/filings/data/runs/run-2/").is_empty(), "run-2 stays out");
}

/// A hold whose bucket write meets a collect marking its build refuses and lists the object it wrote at its digest,
/// so a fresh pull verifies every listed key.
#[test]
fn a_refused_hold_leaves_no_stale_digest() {
    use std::sync::atomic::AtomicUsize;
    let h = history(1);
    let build = h.snapshots[0].clone();
    let holds = h.writer.root().join("tables/filings/holds");
    std::fs::create_dir_all(&holds).unwrap();
    let expired = json!({"build_id": build, "principal": "ops", "placed_at": "2030-01-01T04:00:00Z", "expires_at": "2030-01-05T00:00:00Z"}).to_string();
    std::fs::write(holds.join(format!("{build}.json")), &expired).unwrap();
    h.writer.syncer.push(at("2030-01-10T03:00:00Z")).unwrap();
    // The writer holds the build again; a collect marks it between the hold's check and its object write.
    let renewed = json!({"build_id": build, "principal": "ops", "placed_at": "2030-01-11T00:00:00Z", "expires_at": "2030-02-20T00:00:00Z"}).to_string();
    std::fs::write(holds.join(format!("{build}.json")), &renewed).unwrap();
    let hold_key = format!("team/research/tables/filings/holds/{build}.json");
    let gets = Arc::new(AtomicUsize::new(0));
    let (seen, collector, decl) = (gets.clone(), rebucketed(&h.a, h.b.clone()), h.decl.clone());
    let script = Script {
        on_get: Some(Box::new(move |key| {
            if key == hold_key && seen.fetch_add(1, Ordering::SeqCst) == 0 {
                collector.collect_declared(at("2030-01-11T00:00:00Z"), false, std::slice::from_ref(&decl)).unwrap();
            }
            None
        })),
        ..Script::default()
    };
    let holding = rebucketed(&h.writer, Arc::new(Scripted { inner: h.b.clone(), script }));
    assert!(!holding.push_hold("filings", &build, at("2030-01-11T00:00:00Z")).unwrap(), "the hold refuses");
    assert!(gets.load(Ordering::SeqCst) >= 1);
    node("fresh", h.b.clone(), "").syncer.pull(&PullScope::default()).unwrap();
}

/// A hold whose commit round meets a collect's commit of its build refuses, and lists nothing.
// spec: run.model.hold-bucket-first@a6eb40f1
#[test]
fn a_hold_racing_a_collect_commit_refuses() {
    use std::sync::atomic::AtomicUsize;
    let h = history(1);
    let build = h.snapshots[0].clone();
    let hold = json!({"build_id": build, "principal": "ops", "placed_at": "2030-01-11T00:00:00Z", "expires_at": "2030-02-20T00:00:00Z"}).to_string();
    std::fs::create_dir_all(h.writer.root().join("tables/filings/holds")).unwrap();
    std::fs::write(h.writer.root().join(format!("tables/filings/holds/{build}.json")), &hold).unwrap();
    // The collect commits after the hold's bucket check and before the hold's commit round reads.
    let reads = Arc::new(AtomicUsize::new(0));
    let (seen, collector, decl) = (reads.clone(), rebucketed(&h.a, h.b.clone()), h.decl.clone());
    let script = Script {
        on_get: Some(Box::new(move |key| {
            if key == "team/manifest.json" && seen.fetch_add(1, Ordering::SeqCst) == 0 {
                collector.collect_declared(at("2030-01-11T00:00:00Z"), false, std::slice::from_ref(&decl)).unwrap();
            }
            None
        })),
        ..Script::default()
    };
    let holding = rebucketed(&h.writer, Arc::new(Scripted { inner: h.b.clone(), script }));
    assert!(!holding.push_hold("filings", &build, at("2030-01-11T00:00:00Z")).unwrap(), "the hold refuses");
    let m = manifest(h.b.as_ref());
    assert!(m.entries.contains_key(&format!("research/tables/filings/data/snapshots/{build}/_collected.json")));
    assert!(!m.entries.contains_key(&format!("research/tables/filings/holds/{build}.json")), "the hold is not listed");
}

/// A collect whose commit round meets a hold committed after its first read deletes nothing the hold retains: its
/// delete step answers to the reclaim of the round that committed.
// spec: store.collect.retained-kept@c46c0379
#[test]
fn a_hold_committed_inside_a_collects_commit_keeps_the_parts() {
    use std::sync::atomic::AtomicUsize;
    let mut h = history(3);
    let decls = std::slice::from_ref(&h.decl);
    let f1 = format!("research/tables/filings/data/snapshots/{}/", h.snapshots[0]);
    let parts: Vec<String> = listed(&manifest(h.b.as_ref()), &f1).into_iter().filter(|k| k.ends_with(".parquet")).collect();
    assert!(!parts.is_empty());
    h.a.syncer.collect_declared(at("2030-01-11T00:00:00Z"), false, decls).unwrap();
    h.a.syncer.config.retain_generations = NonZeroU64::new(1);
    // A hold lands between the second collect's first read and its commit round.
    let reads = Arc::new(AtomicUsize::new(0));
    let (seen, inner, build) = (reads.clone(), h.b.clone(), h.snapshots[0].clone());
    let script = Script {
        on_get: Some(Box::new(move |key| {
            if key == "team/manifest.json" && seen.fetch_add(1, Ordering::SeqCst) == 1 {
                let hkey = format!("research/tables/filings/holds/{build}.json");
                let hold = json!({"build_id": build, "principal": "ops", "placed_at": "2030-01-12T00:00:00Z", "expires_at": "2030-02-20T00:00:00Z"}).to_string();
                inner.put(&format!("team/{hkey}"), hold.as_bytes(), contextful_core::store::object::Condition::None).unwrap();
                let mut m: BucketManifest = serde_json::from_slice(&inner.get(key).unwrap().unwrap().0).unwrap();
                m.entries.insert(hkey, contextful_core::store::sync::Entry { sha256: contextful_core::run::journal::sha256_hex(hold.as_bytes()), size: hold.len() as u64, owner: String::new() });
                m.generation += 1;
                inner.put(key, &serde_json::to_vec(&m).unwrap(), contextful_core::store::object::Condition::None).unwrap();
            }
            None
        })),
        ..Script::default()
    };
    let racing = rebucketed(&h.a, Arc::new(Scripted { inner: h.b.clone(), script }));
    let report = racing.collect_declared(at("2030-01-12T00:00:00Z"), false, decls).unwrap();
    assert!(reads.load(Ordering::SeqCst) >= 2);
    for k in &parts {
        assert!(h.b.get(&format!("team/{k}")).unwrap().is_some(), "`{k}` was deleted under a hold: {report:?}");
    }
}

/// A collect renews its lease with a fresh clock before deleting; a lease another collect took past its expiry stops
/// it without deleting anything.
// spec: store.collect.lease-renewed@4e0904f0
#[test]
fn a_collect_whose_lease_another_took_deletes_nothing() {
    use std::sync::atomic::AtomicUsize;
    let h = history(1);
    let f1 = format!("research/tables/filings/data/snapshots/{}/", h.snapshots[0]);
    let parts: Vec<String> = listed(&manifest(h.b.as_ref()), &f1).into_iter().filter(|k| k.ends_with(".parquet")).collect();
    assert!(!parts.is_empty());
    // Eleven minutes into the collect's commit, another node takes the expired lease.
    let reads = Arc::new(AtomicUsize::new(0));
    let (seen, other) = (reads.clone(), rebucketed(&h.writer, h.b.clone()));
    let script = Script {
        on_get: Some(Box::new(move |key| {
            if key == "team/manifest.json" && seen.fetch_add(1, Ordering::SeqCst) == 1 {
                assert!(other.acquire_collect(at("2030-01-11T00:11:00Z")).unwrap().is_some());
            }
            None
        })),
        ..Script::default()
    };
    let collecting = rebucketed(&h.a, Arc::new(Scripted { inner: h.b.clone(), script }));
    let err = collecting.collect_declared(at("2030-01-11T00:00:00Z"), false, std::slice::from_ref(&h.decl)).unwrap_err();
    assert!(err.to_string().starts_with("LeaseFenced"), "{err}");
    assert!(parts.iter().all(|k| h.b.get(&format!("team/{k}")).unwrap().is_some()), "nothing was deleted");
}

/// A push holding a run a collect folded out reads its table's chain only back to the first snapshot created before
/// that run committed.
// spec: store.merge.collected-directory@d7221403
#[test]
fn a_push_walks_the_chain_only_back_to_its_oldest_candidate_run() {
    let dir = tempfile::tempdir().unwrap();
    let b = bucket(dir.path());
    let mut a = node("ingest-a", b.clone(), "");
    a.syncer.config.retain_generations = NonZeroU64::new(1);
    let writer = node("ingest-b", b.clone(), "");
    let decl = keyed("filings");
    let mut snapshots = Vec::new();
    for i in 1..=4 {
        writer.land(&format!("run-{i}"), json!([{"id": 1, "v": i}, {"id": 10 + i}]), &format!("2030-01-01T0{i}:30:00Z"));
        writer.syncer.push(at(&format!("2030-01-01T0{i}:30:00Z"))).unwrap();
        a.syncer.pull(&PullScope::default()).unwrap();
        snapshots.push(fold_at(&a, &decl, &format!("2030-01-01T0{}:00:00Z", i + 1)));
        a.syncer.push(at(&format!("2030-01-01T0{}:00:00Z", i + 1))).unwrap();
    }
    a.syncer.collect_declared(at("2030-01-09T00:00:00Z"), false, std::slice::from_ref(&decl)).unwrap();
    // Forty days on the records lapsed; the writer pushes again, holding run-4 alone.
    a.syncer.collect_declared(at("2030-02-20T00:00:00Z"), false, std::slice::from_ref(&decl)).unwrap();
    for i in 1..=3 {
        std::fs::remove_dir_all(writer.root().join(format!("tables/filings/data/runs/run-{i}"))).unwrap();
    }
    let script = Script::default();
    let watched = Arc::new(Scripted { inner: b.clone(), script });
    let pushing = rebucketed(&writer, watched.clone());
    pushing.push(at("2030-02-21T00:00:00Z")).unwrap();
    let fetched = watched.script.gets.lock().unwrap().clone();
    let manifest_of = |id: &str| format!("team/research/tables/filings/data/snapshots/{id}/_manifest.json");
    assert!(fetched.contains(&manifest_of(&snapshots[3])) && fetched.contains(&manifest_of(&snapshots[2])), "{fetched:?}");
    assert!(!fetched.contains(&manifest_of(&snapshots[1])) && !fetched.contains(&manifest_of(&snapshots[0])), "{fetched:?}");
    assert!(listed(&manifest(b.as_ref()), "research/tables/filings/data/runs/run-4/").is_empty(), "run-4 stays out");
}

/// A project named by several segments collects its tables as a one-segment project does.
#[test]
fn a_multi_segment_project_reclaims_its_superseded_snapshots() {
    let dir = tempfile::tempdir().unwrap();
    let b = bucket(dir.path());
    let mut a = crate::support::node_in("lab/research", "ingest-a", b.clone(), "");
    a.syncer.config.retain_generations = NonZeroU64::new(1);
    let decl = keyed("filings");
    let steps = [("2030-01-01T00:00:00Z", "2030-01-01T01:00:00Z"), ("2030-01-01T02:00:00Z", "2030-01-01T03:00:00Z"), ("2030-01-10T00:00:00Z", "2030-01-10T01:00:00Z")];
    for (i, (landed, folded)) in steps.into_iter().enumerate() {
        a.land(&format!("run-{}", i + 1), json!([{"id": 1, "v": i + 1}, {"id": 11 + i}]), landed);
        fold_at(&a, &decl, folded);
        a.syncer.push(at(folded)).unwrap();
    }
    let report = a.syncer.collect_declared(at("2030-01-11T00:00:00Z"), false, std::slice::from_ref(&decl)).unwrap();
    assert!(report.undeclared.is_empty() && report.superseded.dropped > 0, "{report:?}");
}
