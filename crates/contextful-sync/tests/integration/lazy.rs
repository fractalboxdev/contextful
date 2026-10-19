//! `store.pull.lazy-parts`, `store.pull.part-cache` and `store.pull.part-collected`: a store
//! booting on metadata and fetching each part when a read first needs it.

use crate::support::{at, bucket, node, Node};
use contextful_context::rows::table_rows;
use contextful_core::store::declare::TableDecl;
use contextful_core::store::object::ObjectStore;
use contextful_sync::PullScope;
use serde_json::json;
use std::sync::Arc;

const NOW: &str = "2030-01-01T01:00:00Z";

/// A writer that landed and pushed `runs` runs of one row each into `filings`.
fn pushed(b: Arc<dyn ObjectStore>, runs: usize) -> Node {
    let a = node("ingest-a", b, "");
    for i in 0..runs {
        a.land(&format!("run-{i}"), json!([{ "id": i, "title": format!("t{i}") }]), "2030-01-01T00:00:00Z");
    }
    a.syncer.push(at(NOW)).unwrap();
    a
}

/// Every local Parquet part under `node`'s `filings` table.
fn local_parts(node: &Node) -> Vec<std::path::PathBuf> {
    let mut out = Vec::new();
    let mut stack = vec![node.root().join("tables/filings")];
    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(&dir).into_iter().flatten().flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else if path.extension().is_some_and(|e| e == "parquet") {
                out.push(path);
            }
        }
    }
    out.sort();
    out
}

/// A cold node of the same bucket declaring `lazy_parts`, its cache bound `cache` where given.
fn lazy(b: Arc<dyn ObjectStore>, cache: Option<u64>) -> Node {
    let mut n = node("ingest-b", b, "");
    n.syncer.config.lazy_parts = Some(true);
    n.syncer.config.part_cache_bytes = cache;
    n
}

fn ids(rows: Vec<serde_json::Map<String, serde_json::Value>>) -> Vec<i64> {
    let mut ids: Vec<i64> = rows.iter().map(|r| r["id"].as_i64().unwrap()).collect();
    ids.sort_unstable();
    ids
}

/// A lazy pull downloads no Parquet part, and a read fetches each part it needs, verified,
/// answering the rows the writer landed.
// spec: store.pull.lazy-parts@e8405d48
#[test]
fn a_lazy_pull_downloads_no_part_and_a_read_fetches_each_it_needs() {
    let dir = tempfile::tempdir().unwrap();
    let b = bucket(dir.path());
    pushed(b.clone(), 3);
    let cold = lazy(b, None);
    let report = cold.syncer.pull(&PullScope::default()).unwrap();
    assert!(!report.downloaded.is_empty(), "the pull brings metadata home");
    assert!(report.downloaded.iter().all(|k| !k.ends_with(".parquet")), "{:?}", report.downloaded);
    assert!(local_parts(&cold).is_empty());

    let store = cold.syncer.store.clone().with_part_source(cold.syncer.part_source());
    assert_eq!(ids(table_rows(&store, &TableDecl::named("filings"), &["id"]).unwrap()), [0, 1, 2]);
    assert_eq!(local_parts(&cold).len(), 3, "each needed part is home");
}

/// Past `[sync] part_cache_bytes`, the least recently read fetched part that no read holds
/// leaves the disk.
// spec: store.pull.part-cache@03d3e3f0
#[test]
fn past_the_part_cache_bound_the_least_recently_read_part_leaves() {
    let dir = tempfile::tempdir().unwrap();
    let b = bucket(dir.path());
    let a = pushed(b.clone(), 1);
    let part = local_parts(&a)[0].clone();
    let size = std::fs::metadata(&part).unwrap().len();
    a.land("run-1", json!([{ "id": 1, "title": "t1" }]), "2030-01-01T00:30:00Z");
    a.syncer.push(at("2030-01-01T02:00:00Z")).unwrap();

    let cold = lazy(b, Some(size + size / 2));
    cold.syncer.pull(&PullScope::default()).unwrap();
    let source = cold.syncer.part_source();
    let store = cold.syncer.store.clone().with_part_source(source.clone());
    let decl = TableDecl::named("filings");
    // One read needs both parts and holds them both while it runs, whatever the bound.
    assert_eq!(ids(table_rows(&store, &decl, &["id"]).unwrap()), [0, 1]);
    assert_eq!(local_parts(&cold).len(), 1, "once the read returns, the bound holds one part");
}

/// A failed acquisition keeps every completed download subject to cache eviction.
// spec: store.pull.part-accounting@6f2d3781
#[test]
fn a_refused_lazy_fetch_keeps_successful_parts_accounted() {
    use contextful_core::store::object::PartSource;
    use contextful_sync::lazy::LazyParts;
    let dir = tempfile::tempdir().unwrap();
    let b = bucket(dir.path());
    let writer = pushed(b.clone(), 2);
    let cold = lazy(b.clone(), Some(1));
    cold.syncer.pull(&PullScope::default()).unwrap();
    let files: Vec<String> = local_parts(&writer).iter().map(|p| p.strip_prefix(writer.root()).unwrap().to_string_lossy().into_owned()).collect();
    assert_eq!(files.len(), 2);
    b.delete(&format!("team/research/{}", files[1])).unwrap();
    let source = LazyParts::new(b, "team", "research", &cold.root(), 1, 1);
    let error = source.hold(&files).unwrap_err().to_string();
    assert!(error.starts_with("SyncPartCollected"), "{error}");
    assert_eq!(local_parts(&cold).len(), 0);
}

/// An unindexed fetched part counts toward the cache bound after a reopen.
#[test]
fn a_reopened_lazy_cache_accounts_for_an_unindexed_part() {
    use contextful_core::store::object::PartSource;
    use contextful_sync::lazy::LazyParts;
    let dir = tempfile::tempdir().unwrap();
    let b = bucket(dir.path());
    let writer = pushed(b.clone(), 1);
    let own = LazyParts::new(b.clone(), "team", "research", &writer.root(), 1, 1);
    drop(own.hold(&[]).unwrap());
    assert_eq!(local_parts(&writer).len(), 1);
    let cold = lazy(b.clone(), None);
    cold.syncer.pull(&PullScope::default()).unwrap();
    let file = local_parts(&writer)[0].strip_prefix(writer.root()).unwrap().to_string_lossy().into_owned();
    let source = LazyParts::new(b.clone(), "team", "research", &cold.root(), u64::MAX, 1);
    drop(source.hold(&[file]).unwrap());
    assert_eq!(local_parts(&cold).len(), 1);
    drop(source);
    let index = cold._dir.path().join(".contextful/sync/research/part-cache.json");
    std::fs::remove_file(index).unwrap();
    let receipts = cold._dir.path().join(".contextful/sync/research/fetched-parts");
    if receipts.exists() {
        std::fs::remove_dir_all(receipts).unwrap();
    }
    cold.land("local-run", json!([{ "id": 2, "title": "local" }]), NOW);
    assert_eq!(local_parts(&cold).len(), 2);

    let reopened = LazyParts::new(b, "team", "research", &cold.root(), 1, 1);
    drop(reopened.hold(&[]).unwrap());
    let remaining = local_parts(&cold);
    assert_eq!(remaining.len(), 1);
    assert!(remaining[0].to_string_lossy().contains("local-run"));
}

/// A download receipt recovers an unindexed part without the bucket manifest.
#[test]
fn a_reopened_lazy_cache_uses_download_receipts_without_the_bucket_manifest() {
    use contextful_core::store::object::PartSource;
    use contextful_sync::lazy::LazyParts;
    let dir = tempfile::tempdir().unwrap();
    let b = bucket(dir.path());
    let writer = pushed(b.clone(), 1);
    let cold = lazy(b.clone(), None);
    cold.syncer.pull(&PullScope::default()).unwrap();
    let file = local_parts(&writer)[0].strip_prefix(writer.root()).unwrap().to_string_lossy().into_owned();
    let source = LazyParts::new(b.clone(), "team", "research", &cold.root(), u64::MAX, 1);
    drop(source.hold(&[file]).unwrap());
    assert_eq!(local_parts(&cold).len(), 1);
    drop(source);
    std::fs::remove_file(cold._dir.path().join(".contextful/sync/research/part-cache.json")).unwrap();
    b.delete("team/manifest.json").unwrap();

    let reopened = LazyParts::new(b, "team", "research", &cold.root(), 1, 1);
    drop(reopened.hold(&[]).unwrap());
    assert_eq!(local_parts(&cold).len(), 0);
}

/// Concurrent downloads of one part occupy its bytes once.
#[test]
fn two_lazy_reads_fetching_one_part_count_it_once() {
    use crate::support::{Script, Scripted};
    use contextful_core::store::object::PartSource;
    use contextful_sync::lazy::LazyParts;
    let dir = tempfile::tempdir().unwrap();
    let b = bucket(dir.path());
    let writer = pushed(b.clone(), 2);
    let parts = local_parts(&writer);
    assert_eq!(parts.len(), 2);
    let bound = parts.iter().map(|p| std::fs::metadata(p).unwrap().len()).sum();
    let files: Vec<String> = parts.iter().map(|p| p.strip_prefix(writer.root()).unwrap().to_string_lossy().into_owned()).collect();
    let cold = lazy(b.clone(), Some(bound));
    cold.syncer.pull(&PullScope::default()).unwrap();
    let together = std::sync::Barrier::new(2);
    let key = format!("team/research/{}", files[0]);
    let scripted = Arc::new(Scripted {
        inner: b,
        script: Script {
            on_get: Some(Box::new(move |k| {
                if k == key {
                    together.wait();
                }
                None
            })),
            ..Script::default()
        },
    });
    let source = LazyParts::new(scripted, "team", "research", &cold.root(), bound, 1);
    drop(source.hold(&files[1..]).unwrap());
    let holds = std::thread::scope(|scope| {
        let first = scope.spawn(|| source.hold(&files[..1]).unwrap());
        let second = scope.spawn(|| source.hold(&files[..1]).unwrap());
        (first.join().unwrap(), second.join().unwrap())
    });
    drop(holds);
    assert_eq!(local_parts(&cold).len(), 2);
}

/// A part the bucket no longer holds refuses the read, naming the part.
// spec: store.pull.part-collected@0de4a1ac
#[test]
fn a_part_the_bucket_no_longer_holds_refuses_the_read() {
    let dir = tempfile::tempdir().unwrap();
    let b = bucket(dir.path());
    let a = pushed(b.clone(), 1);
    let cold = lazy(b.clone(), None);
    cold.syncer.pull(&PullScope::default()).unwrap();
    let part = local_parts(&a)[0].strip_prefix(a.root()).unwrap().to_string_lossy().into_owned();
    b.delete(&format!("team/research/{part}")).unwrap();

    let store = cold.syncer.store.clone().with_part_source(cold.syncer.part_source());
    let err = table_rows(&store, &TableDecl::named("filings"), &["id"]).unwrap_err().to_string();
    assert!(err.starts_with("SyncPartCollected") && err.contains(&part), "{err}");
}

/// A lazy store pushing after its pull tombstones no part it never fetched, its own node's
/// included.
#[test]
fn a_lazy_store_push_tombstones_no_part_it_never_fetched() {
    let dir = tempfile::tempdir().unwrap();
    let b = bucket(dir.path());
    pushed(b.clone(), 2);
    for id in ["ingest-b", "ingest-a"] {
        let mut cold = node(id, b.clone(), "");
        cold.syncer.config.lazy_parts = Some(true);
        cold.syncer.pull(&PullScope::default()).unwrap();
        cold.syncer.push(at("2030-01-01T03:00:00Z")).unwrap();
        let (bytes, _) = b.get("team/manifest.json").unwrap().unwrap();
        let listed: contextful_core::store::sync::BucketManifest = serde_json::from_slice(&bytes).unwrap();
        let parts = listed.entries.keys().filter(|k| k.ends_with(".parquet") && k.contains("/data/")).count();
        assert!(parts >= 2, "{id}: the bucket still lists every part: {:?}", listed.entries.keys().collect::<Vec<_>>());
        let tombstoned: Vec<&String> = listed.tombstones.keys().filter(|k| k.ends_with(".parquet")).collect();
        assert_eq!(tombstoned.len(), 0, "{id}: {tombstoned:?}");
    }
}

/// A fold on a lazy store fetches its input parts through the part cache, and inputs larger
/// than the cache refuse the pass, which publishes nothing.
// spec: store.fold.cached-inputs@0f705ed9
#[test]
fn a_lazy_fold_reads_its_inputs_through_the_cache_and_refuses_past_its_bound() {
    use contextful_context::fold::fold;
    use contextful_core::store::fold::FoldOutcome;
    let dir = tempfile::tempdir().unwrap();
    let b = bucket(dir.path());
    pushed(b.clone(), 3);
    let decl = TableDecl::named("filings");

    let tight = lazy(b.clone(), Some(1));
    tight.syncer.pull(&PullScope::default()).unwrap();
    let store = tight.syncer.store.clone().with_part_source(tight.syncer.part_source());
    let before = store.pointer("filings").unwrap();
    let err = fold(&store, &decl, at("2030-01-02T00:00:00Z")).unwrap_err().to_string();
    assert!(err.starts_with("FoldInputsExceedCache"), "{err}");
    assert_eq!(store.pointer("filings").unwrap(), before, "a refused pass publishes nothing");

    let roomy = lazy(b, None);
    roomy.syncer.pull(&PullScope::default()).unwrap();
    let store = roomy.syncer.store.clone().with_part_source(roomy.syncer.part_source());
    assert!(matches!(fold(&store, &decl, at("2030-01-02T00:00:00Z")).unwrap(), FoldOutcome::Folded { .. }));
    assert_eq!(ids(table_rows(&store, &decl, &["id"]).unwrap()), [0, 1, 2]);
}
