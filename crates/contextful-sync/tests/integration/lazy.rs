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

mod deadline {
    use super::*;
    use contextful_core::run::journal::sha256_hex;
    use contextful_core::store::object::{Condition, ObjectError, PartSource, Put};
    use contextful_core::store::sync::{BucketManifest, Entry, MANIFEST_KEY};
    use contextful_core::read::ReadError;
    use contextful_core::store::StoreError;
    use contextful_sync::lazy::{Clock, PART_WAIT};
    use contextful_sync::LazyParts;
    use std::sync::Mutex;
    use std::time::{Duration, Instant};

    const PART: &str = "tables/filings/data/part.parquet";
    const SECOND: &str = "tables/filings/data/second.parquet";

    struct Time(Mutex<Instant>);

    impl Clock for Time {
        fn now(&self) -> Instant {
            *self.0.lock().unwrap()
        }
        fn sleep(&self, duration: Duration) {
            *self.0.lock().unwrap() += duration;
        }
    }

    struct Slow {
        time: Arc<Time>,
        manifest_delay: Duration,
        part_delay: Duration,
        unavailable: bool,
        deadlines: Mutex<Vec<Instant>>,
    }

    impl ObjectStore for Slow {
        fn get(&self, key: &str) -> Result<Option<(Vec<u8>, String)>, ObjectError> {
            let manifest = key == MANIFEST_KEY;
            self.time.sleep(if manifest { self.manifest_delay } else { self.part_delay });
            if manifest {
                let mut manifest = BucketManifest::default();
                manifest.entries.insert(format!("project/{PART}"), Entry { sha256: sha256_hex(b"rows"), size: 4, owner: String::new() });
                manifest.entries.insert(format!("project/{SECOND}"), Entry { sha256: sha256_hex(b"rows"), size: 4, owner: String::new() });
                Ok(Some((serde_json::to_vec(&manifest).unwrap(), "manifest".into())))
            } else if self.unavailable {
                Err(ObjectError::Unavailable("offline".into()))
            } else {
                Ok(Some((b"rows".to_vec(), "part".into())))
            }
        }

        fn get_within(&self, key: &str, deadline: Instant) -> Result<Option<(Vec<u8>, String)>, ObjectError> {
            self.deadlines.lock().unwrap().push(deadline);
            let delay = if key == MANIFEST_KEY { self.manifest_delay } else { self.part_delay };
            let remaining = deadline.saturating_duration_since(self.time.now());
            if delay > remaining {
                self.time.sleep(remaining);
                return Err(ObjectError::Unavailable("deadline".into()));
            }
            self.get(key)
        }

        fn put(&self, _: &str, _: &[u8], _: Condition) -> Result<Put, ObjectError> {
            unreachable!()
        }
        fn delete(&self, _: &str) -> Result<(), ObjectError> {
            unreachable!()
        }
        fn list(&self, _: &str) -> Result<Vec<String>, ObjectError> {
            unreachable!()
        }
    }

    #[test]
    fn fold_input_lookup_and_download_share_the_acquisition_deadline() {
        let started = Instant::now();
        let time = Arc::new(Time(Mutex::new(started)));
        let bucket = Arc::new(Slow {
            time: time.clone(),
            manifest_delay: Duration::from_secs(29),
            part_delay: PART_WAIT * 2,
            unavailable: false,
            deadlines: Mutex::new(Vec::new()),
        });
        let dir = tempfile::tempdir().unwrap();
        let parts = LazyParts::with_clock(
            bucket.clone(),
            "",
            "project",
            dir.path(),
            100,
            1,
            time.clone(),
        );
        let result = parts.hold_inputs(&[PART.into()]);
        assert!(
            matches!(result, Err(StoreError::Read(ReadError::ReadPartUnavailable(_)))),
            "{result:?}"
        );
        assert_eq!(time.now() - started, PART_WAIT);
        assert_eq!(*bucket.deadlines.lock().unwrap(), [started + PART_WAIT; 2]);
    }

    #[test]
    fn a_part_answer_at_the_deadline_is_kept() {
        let started = Instant::now();
        let time = Arc::new(Time(Mutex::new(started)));
        let bucket = Arc::new(Slow {
            time: time.clone(),
            manifest_delay: Duration::ZERO,
            part_delay: PART_WAIT,
            unavailable: false,
            deadlines: Mutex::new(Vec::new()),
        });
        let dir = tempfile::tempdir().unwrap();
        let parts = LazyParts::with_clock(bucket, "", "project", dir.path(), 100, 1, time.clone());
        let hold = parts.hold(&[PART.into()]).unwrap();
        assert_eq!(time.now() - started, PART_WAIT);
        assert_eq!(std::fs::read(dir.path().join(PART)).unwrap(), b"rows");
        drop(hold);
    }

    #[test]
    fn queued_parts_share_the_acquisition_deadline() {
        let started = Instant::now();
        let time = Arc::new(Time(Mutex::new(started)));
        let bucket = Arc::new(Slow {
            time: time.clone(),
            manifest_delay: Duration::ZERO,
            part_delay: Duration::from_secs(20),
            unavailable: false,
            deadlines: Mutex::new(Vec::new()),
        });
        let dir = tempfile::tempdir().unwrap();
        let parts = LazyParts::with_clock(
            bucket.clone(),
            "",
            "project",
            dir.path(),
            100,
            1,
            time.clone(),
        );
        let result = parts.hold(&[PART.into(), SECOND.into()]);
        assert!(
            matches!(result, Err(StoreError::Read(ReadError::ReadPartUnavailable(_)))),
            "{result:?}"
        );
        assert_eq!(time.now() - started, PART_WAIT);
        assert_eq!(*bucket.deadlines.lock().unwrap(), [started + PART_WAIT; 3]);
        assert_eq!(std::fs::read(dir.path().join(PART)).unwrap(), b"rows");
        assert!(!dir.path().join(SECOND).exists());
    }

    // spec: read.cache.miss-wait@15b66524
    #[test]
    fn one_deadline_bounds_manifest_gets_and_backoff() {
        for (manifest_delay, part_delay, unavailable) in [
            (PART_WAIT * 2, Duration::ZERO, false),
            (Duration::from_secs(7), PART_WAIT * 2, false),
            (Duration::from_millis(7250), Duration::from_millis(250), true),
            (Duration::from_millis(7250), Duration::ZERO, true),
        ] {
            let started = Instant::now();
            let time = Arc::new(Time(Mutex::new(started)));
            let bucket = Arc::new(Slow { time: time.clone(), manifest_delay, part_delay, unavailable, deadlines: Mutex::new(Vec::new()) });
            let dir = tempfile::tempdir().unwrap();
            let parts = LazyParts::with_clock(bucket.clone(), "", "project", dir.path(), 100, 1, time.clone());
            let err = parts.hold(&[PART.into()]).unwrap_err();
            assert!(matches!(err, StoreError::Read(ReadError::ReadPartUnavailable(ref message)) if message.contains(PART)), "{err}");
            assert_eq!(time.now() - started, PART_WAIT);
            let deadlines = bucket.deadlines.lock().unwrap();
            assert!(!deadlines.is_empty());
            assert!(deadlines.iter().all(|deadline| *deadline == started + PART_WAIT));
            assert!(!dir.path().join(PART).exists());
        }
    }

    #[test]
    fn the_serving_edge_preserves_the_manifest_deadline() {
        let started = Instant::now();
        let time = Arc::new(Time(Mutex::new(started)));
        let bucket = Arc::new(Slow { time: time.clone(), manifest_delay: PART_WAIT * 2, part_delay: Duration::ZERO, unavailable: false, deadlines: Mutex::new(Vec::new()) });
        let edge = Arc::new(contextful_sync::ServingEdge::new(bucket.clone(), "", "project", &[]));
        let dir = tempfile::tempdir().unwrap();
        let parts = LazyParts::with_clock(edge, "", "project", dir.path(), 100, 1, time.clone());
        assert!(matches!(parts.hold(&[PART.into()]), Err(StoreError::Read(ReadError::ReadPartUnavailable(_)))));
        assert_eq!(time.now() - started, PART_WAIT);
        assert_eq!(*bucket.deadlines.lock().unwrap(), [started + PART_WAIT]);
    }
}

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
// spec: store.pull.lazy-parts@d6ebc42d
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
// spec: store.pull.part-cache@556f49e0
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
// spec: store.pull.part-accounting@b7253123
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

/// Construction preserves receipt-free files without reading the bucket.
#[test]
fn a_reopened_lazy_cache_preserves_receipt_free_parts_without_bucket_reads() {
    use crate::support::{Script, Scripted};
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

    let scripted = Arc::new(Scripted { inner: b, script: Script::default() });
    let reopened = LazyParts::new(scripted.clone(), "team", "research", &cold.root(), 1, 1);
    assert!(scripted.script.gets.lock().unwrap().is_empty(), "construction reads local receipts alone");
    drop(reopened.hold(&[]).unwrap());
    let remaining = local_parts(&cold);
    assert_eq!(remaining.len(), 2, "receipt-free parts retain local ownership");
}

/// A hold persists its completed downloads together, including a refused batch.
#[test]
fn a_lazy_hold_saves_its_index_after_the_download_batch() {
    use crate::support::{Script, Scripted};
    use contextful_core::store::object::PartSource;
    use contextful_sync::lazy::LazyParts;
    use std::sync::atomic::{AtomicBool, Ordering};
    for refuse in [false, true] {
        let dir = tempfile::tempdir().unwrap();
        let b = bucket(dir.path());
        let writer = pushed(b.clone(), 3);
        let cold = lazy(b.clone(), None);
        cold.syncer.pull(&PullScope::default()).unwrap();
        let files: Vec<String> = local_parts(&writer).iter().map(|p| p.strip_prefix(writer.root()).unwrap().to_string_lossy().into_owned()).collect();
        if refuse {
            b.delete(&format!("team/research/{}", files[2])).unwrap();
        }
        let index = cold._dir.path().join(".contextful/sync/research/part-cache.json");
        let observed_index = Arc::new(AtomicBool::new(false));
        let observed = observed_index.clone();
        let during = index.clone();
        let scripted = Arc::new(Scripted {
            inner: b,
            script: Script {
                on_get: Some(Box::new(move |key| {
                    if key.ends_with(".parquet") && during.exists() {
                        observed.store(true, Ordering::SeqCst);
                    }
                    None
                })),
                ..Script::default()
            },
        });
        let source = LazyParts::new(scripted, "team", "research", &cold.root(), u64::MAX, 1);
        let held = source.hold(&files);
        assert_eq!(held.is_err(), refuse);
        assert!(!observed_index.load(Ordering::SeqCst), "the index remains unchanged during the batch");
        let saved: std::collections::BTreeMap<String, u64> = serde_json::from_slice(&std::fs::read(index).unwrap()).unwrap();
        assert_eq!(saved.len(), if refuse { 2 } else { 3 });
        assert!(saved.values().all(|size| *size > 0));
    }
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
// spec: store.pull.part-collected@fe1d9673
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
// spec: store.fold.cached-inputs@03aad29b
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

/// The bucket manifest `b` holds now.
fn listed(b: &Arc<dyn ObjectStore>) -> contextful_core::store::sync::BucketManifest {
    let (bytes, _) = b.get("team/manifest.json").unwrap().unwrap();
    serde_json::from_slice(&bytes).unwrap()
}

/// A writer that folded runs 0 to 2 of `filings` into one snapshot, landed run 9 after it,
/// and pushed.
fn folded_then_landed(b: Arc<dyn ObjectStore>) -> Node {
    let a = pushed(b, 3);
    contextful_context::fold::fold(&a.syncer.store, &TableDecl::named("filings"), at("2030-01-02T00:00:00Z")).unwrap();
    a.land("run-9", json!([{ "id": 9, "title": "t9" }]), "2030-01-02T00:00:00Z");
    a.syncer.push(at("2030-01-02T01:00:00Z")).unwrap();
    a
}

/// A lazy pull of a folded table with a later run writes the pointer while no part of its
/// snapshot is local, and brings home every listed object other than a data part and the
/// manifests of the runs that snapshot folds, which it records as skipped.
// spec: store.pull.lazy-pointer@3f707c97
// spec: store.pull.lazy-ready@c9de184d
// spec: store.pull.lazy-folded-runs@2552f902
#[test]
fn a_lazy_pull_writes_the_pointer_on_the_snapshot_manifest_and_brings_home_metadata_alone() {
    use contextful_core::store::lay_out::{SkippedRuns, SKIPPED_RUNS_FILE};
    let dir = tempfile::tempdir().unwrap();
    let b = bucket(dir.path());
    folded_then_landed(b.clone());
    let entries = listed(&b).entries;
    let folded: Vec<String> = (0..3).map(|i| format!("research/tables/filings/data/runs/run-{i}/ingest-a/_manifest.json")).collect();
    assert_eq!(folded.len(), 3);
    assert!(folded.iter().all(|k| entries.contains_key(k)), "{:?}", entries.keys().collect::<Vec<_>>());
    let mut metadata: Vec<String> = entries.keys().filter(|k| !contextful_sync::lazy::is_lazy_part(k) && !folded.contains(k)).cloned().collect();
    metadata.sort();
    assert!(metadata.iter().any(|k| k.contains("/data/snapshots/")), "the table is folded: {metadata:?}");
    assert!(metadata.iter().any(|k| k.contains("/run-9/")), "the later run is listed: {metadata:?}");

    let cold = lazy(b, None);
    let report = cold.syncer.pull(&PullScope::default()).unwrap();
    let mut downloaded = report.downloaded.clone();
    downloaded.sort();
    assert_eq!(downloaded, metadata, "a lazy pull brings home every object but a data part and a folded run's manifest");
    assert_eq!(report.pointers, ["research/tables/filings/_pointer.json"]);
    assert!(local_parts(&cold).is_empty(), "the pointer stands on its snapshot manifest alone");
    let skipped: SkippedRuns = serde_json::from_slice(&std::fs::read(cold.root().join(SKIPPED_RUNS_FILE)).unwrap()).unwrap();
    let expected: Vec<String> = folded.iter().map(|k| k.strip_prefix("research/").unwrap().to_string()).collect();
    assert_eq!(skipped.manifests, expected);

    let store = cold.syncer.store.clone().with_part_source(cold.syncer.part_source());
    assert_eq!(ids(table_rows(&store, &TableDecl::named("filings"), &["id"]).unwrap()), [0, 1, 2, 9]);
}

/// Objects a cold lazy pull of two folded tables fetches before a read is ready: per table,
/// its pointer, its schema and its snapshot manifest.
// spec: store.pull.lazy-ready@c9de184d
#[test]
fn a_cold_lazy_pull_fetches_three_objects_per_folded_table_before_a_read_is_ready() {
    use crate::support::{Script, Scripted};
    let dir = tempfile::tempdir().unwrap();
    let inner = bucket(dir.path());
    let a = node("ingest-a", inner.clone(), "");
    for i in 0..4 {
        a.land(&format!("run-{i}"), json!([{ "id": i }]), "2030-01-01T00:00:00Z");
        a.land_into("notes", &format!("run-{i}"), json!([{ "id": i }]), "2030-01-01T00:00:00Z");
    }
    for t in ["filings", "notes"] {
        contextful_context::fold::fold(&a.syncer.store, &TableDecl::named(t), at("2030-01-02T00:00:00Z")).unwrap();
    }
    a.syncer.push(at("2030-01-02T01:00:00Z")).unwrap();
    let runs = listed(&inner).entries.keys().filter(|k| k.contains("/data/runs/") && k.ends_with("/_manifest.json")).count();
    assert_eq!(runs, 8, "each run's manifest stays listed while retention keeps it");

    let scripted = Arc::new(Scripted { inner, script: Script::default() });
    let mut cold = node("ingest-b", scripted.clone(), "");
    cold.syncer.config.lazy_parts = Some(true);
    cold.syncer.pull(&PullScope::default()).unwrap();
    let fetched: Vec<String> = scripted.script.fetched.lock().unwrap().iter().map(|(k, _)| k.clone()).filter(|k| k != "team/manifest.json").collect();
    assert!(!fetched.is_empty());
    let tables = 2;
    let per_table = fetched.len() as f64 / tables as f64;
    contextful_eval::record::emit("sync-cold-pull-objects-before-ready", per_table, tables, 0x5eed_0643);
    assert_eq!(per_table, 3.0, "{fetched:?}");
}

/// An `as_of` read resolving before the pointer's snapshot on a cold lazy store resolves the
/// runs a store holding the whole bucket resolves, fetching their manifests.
// spec: store.pull.folded-runs-fetched@ff7203dc
#[test]
fn an_as_of_read_on_a_cold_lazy_store_resolves_the_folded_runs_an_eager_store_does() {
    use contextful_core::store::bound_time::{Bound, Bounds};
    let dir = tempfile::tempdir().unwrap();
    let b = bucket(dir.path());
    folded_then_landed(b.clone());
    let bounds = Bounds { as_of: Some(Bound::parse("2030-01-01T12:00:00Z").unwrap()), ..Bounds::default() };
    let decl = TableDecl::named("filings");

    let eager = node("ingest-c", b.clone(), "");
    eager.syncer.pull(&PullScope::default()).unwrap();
    let expected = contextful_context::scan::scan(&eager.syncer.store, &decl, bounds).unwrap().files;
    assert_eq!(expected.len(), 3, "the bound precedes the fold: {expected:?}");

    let cold = lazy(b, None);
    cold.syncer.pull(&PullScope::default()).unwrap();
    let store = cold.syncer.store.clone().with_part_source(cold.syncer.part_source());
    assert_eq!(contextful_context::scan::scan(&store, &decl, bounds).unwrap().files, expected);
    assert!(cold.root().join("tables/filings/data/runs/run-0/ingest-a/_manifest.json").is_file());
}

/// A folded run manifest the bucket lost after a lazy pull skipped it refuses the read that
/// needs it, rather than resolving without the run.
// spec: store.pull.folded-run-collected@7a1472fe
#[test]
fn a_skipped_manifest_gone_from_the_bucket_refuses_the_read_that_needs_it() {
    use contextful_core::store::bound_time::{Bound, Bounds};
    let dir = tempfile::tempdir().unwrap();
    let b = bucket(dir.path());
    folded_then_landed(b.clone());
    let cold = lazy(b.clone(), None);
    cold.syncer.pull(&PullScope::default()).unwrap();
    b.delete("team/research/tables/filings/data/runs/run-1/ingest-a/_manifest.json").unwrap();
    let store = cold.syncer.store.clone().with_part_source(cold.syncer.part_source());
    let bounds = Bounds { as_of: Some(Bound::parse("2030-01-01T12:00:00Z").unwrap()), ..Bounds::default() };
    let err = contextful_context::scan::scan(&store, &TableDecl::named("filings"), bounds).unwrap_err().to_string();
    assert!(err.starts_with("SyncPartCollected") && err.contains("run-1"), "{err}");
}

/// A lazy pull still brings home a folded run's `_group.json`, so a grouped member run
/// whose root run folded reads as committed.
// spec: store.pull.lazy-folded-runs@2552f902
#[test]
fn a_lazy_pull_brings_home_a_folded_runs_group_marker() {
    let dir = tempfile::tempdir().unwrap();
    let b = bucket(dir.path());
    let a = folded_then_landed(b.clone());
    let marker = "tables/filings/data/runs/run-0/ingest-a/_group.json";
    std::fs::write(a.root().join(marker), b"[\"filings\"]").unwrap();
    a.syncer.push(at("2030-01-02T02:00:00Z")).unwrap();
    assert!(listed(&b).entries.contains_key(&format!("research/{marker}")));

    let cold = lazy(b, None);
    let report = cold.syncer.pull(&PullScope::default()).unwrap();
    assert!(report.downloaded.iter().any(|k| k.ends_with(marker)), "{:?}", report.downloaded);
    assert!(!cold.root().join("tables/filings/data/runs/run-0/ingest-a/_manifest.json").exists());
}

/// A lazy pull downloads each folded run manifest its own node wrote, so no push of a
/// replacement host deletes one it never held.
// spec: store.pull.lazy-folded-runs@2552f902
#[test]
fn a_lazy_pull_skips_no_manifest_its_own_node_wrote() {
    let dir = tempfile::tempdir().unwrap();
    let b = bucket(dir.path());
    folded_then_landed(b.clone());
    let mut own = node("ingest-a", b, "");
    own.syncer.config.lazy_parts = Some(true);
    own.syncer.pull(&PullScope::default()).unwrap();
    assert!(own.root().join("tables/filings/data/runs/run-0/ingest-a/_manifest.json").is_file());
    assert!(!own.root().join(contextful_core::store::lay_out::SKIPPED_RUNS_FILE).exists());
}

/// An eager pull after a lazy one brings every skipped manifest home and leaves no record.
// spec: store.pull.skipped-record@5f2c6451
#[test]
fn an_eager_pull_clears_the_skipped_runs_record() {
    let dir = tempfile::tempdir().unwrap();
    let b = bucket(dir.path());
    folded_then_landed(b.clone());
    let mut cold = lazy(b, None);
    cold.syncer.pull(&PullScope::default()).unwrap();
    assert!(cold.root().join(contextful_core::store::lay_out::SKIPPED_RUNS_FILE).exists());
    cold.syncer.config.lazy_parts = Some(false);
    cold.syncer.pull(&PullScope::default()).unwrap();
    assert!(!cold.root().join(contextful_core::store::lay_out::SKIPPED_RUNS_FILE).exists());
    assert_eq!(cold.syncer.store.committed_runs("filings").unwrap().len(), 4);
}

/// Once a collect takes the folded runs a lazy pull skipped, an unbounded listing leaves them out, an `as_of` read
/// needing them refuses as collected, and a pull scoped to another table drops them from the record.
// spec: store.pull.folded-run-collected@7a1472fe
// spec: store.pull.skipped-record@5f2c6451
#[test]
fn a_listing_leaves_out_skipped_runs_a_collect_took() {
    use contextful_core::store::bound_time::{Bound, Bounds};
    use contextful_core::store::lay_out::{SkippedRuns, SKIPPED_RUNS_FILE};
    let dir = tempfile::tempdir().unwrap();
    let b = bucket(dir.path());
    let mut a = folded_then_landed(b.clone());
    a.land_into("orders", "run-1", json!([{ "id": 1 }]), "2030-01-02T00:00:00Z");
    a.syncer.push(at("2030-01-02T02:00:00Z")).unwrap();
    let cold = lazy(b.clone(), None);
    cold.syncer.pull(&PullScope::default()).unwrap();
    let record = cold.root().join(SKIPPED_RUNS_FILE);
    let skipped: SkippedRuns = serde_json::from_slice(&std::fs::read(&record).unwrap()).unwrap();
    assert_eq!(skipped.manifests.len(), 3);

    a.syncer.config.retain_generations = std::num::NonZeroU64::new(1);
    let report = a.syncer.collect_declared(at("2030-01-20T00:00:00Z"), false, &[TableDecl::named("filings"), TableDecl::named("orders")]).unwrap();
    assert!(report.folded_runs.deleted > 0, "{report:?}");
    let store = cold.syncer.store.clone().with_part_source(cold.syncer.part_source());
    let runs: Vec<String> = store.committed_runs("filings").unwrap().iter().map(|r| r.run_id.clone()).collect();
    assert_eq!(runs, ["run-9"]);
    let bounds = Bounds { as_of: Some(Bound::parse("2030-01-01T12:00:00Z").unwrap()), ..Bounds::default() };
    let err = contextful_context::scan::scan(&store, &TableDecl::named("filings"), bounds).unwrap_err().to_string();
    assert!(err.starts_with("SyncPartCollected"), "{err}");

    let scope = PullScope { tables: vec![contextful_core::grant::TablePattern::parse("orders").unwrap()], ..PullScope::default() };
    cold.syncer.pull(&scope).unwrap();
    assert!(!record.exists(), "the record names no manifest a collect took");
}
