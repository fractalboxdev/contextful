//! `store.pull.husks-skipped` and `store.collect.index-walk`: a lazy pull leaves each collected
//! snapshot's manifest a valid folded-runs index covers in the bucket, and collect and push read
//! that index in place of the chain.

use crate::support::{at, bucket, node, Node, Script, Scripted};
use contextful_context::rows::table_rows;
use contextful_context::Store;
use contextful_core::store::bound_time::Bound;
use contextful_core::store::declare::TableDecl;
use contextful_core::store::lay_out::{SkippedRuns, SKIPPED_RUNS_FILE};
use contextful_core::store::object::ObjectStore;
use contextful_core::store::sync::BucketManifest;
use contextful_sync::PullScope;
use serde_json::json;
use std::collections::BTreeSet;
use std::num::NonZeroU64;
use std::sync::Arc;

const PREFIX: &str = "research/tables/filings/data/snapshots/";

fn table() -> TableDecl {
    TableDecl::parse_pipeline("[[pipeline.tables]]\nname = \"filings\"\nprimary_key = [\"id\"]\nretain_runs = \"2d\"\n").unwrap().remove(0)
}

fn manifest(b: &dyn ObjectStore) -> BucketManifest {
    serde_json::from_slice(&b.get("team/manifest.json").unwrap().unwrap().0).unwrap()
}

/// The instant `days` days and `hours` hours after 2030-01-01T00:00:00Z.
fn day(days: u64, hours: u64) -> String {
    at("2030-01-01T00:00:00Z").plus_secs(days * 86_400 + hours * 3600).to_rfc3339_nanos()
}

/// A bucket whose `filings` chain holds `passes` folds a day apart, three runs each, collected under a two-day window
/// after every pass: the early snapshots are husks, and the head's index flushed a segment.
struct History {
    _dir: tempfile::TempDir,
    b: Arc<dyn ObjectStore>,
    folder: Node,
    runs: Vec<String>,
}

fn history(passes: u64) -> History {
    let dir = tempfile::tempdir().unwrap();
    let b = bucket(dir.path());
    let mut folder = node("ingest-a", b.clone(), "");
    folder.syncer.config.retain_generations = NonZeroU64::new(1);
    let writer = node("ingest-b", b.clone(), "");
    let decl = table();
    let mut runs = Vec::new();
    for i in 0..passes {
        for j in 0..3 {
            let run = format!("run-{i:02}-{j}");
            // Key 1 moves every pass, so each fold rewrites its parent's part; every run adds a key of its own.
            writer.land(&run, json!([{"id": 1, "v": i}, {"id": 100 + i * 3 + j}]), &day(i, 0));
            runs.push(run);
        }
        writer.syncer.push(at(&day(i, 0))).unwrap();
        folder.syncer.pull(&PullScope::default()).unwrap();
        contextful_context::fold::fold(&folder.syncer.store, &decl, at(&day(i, 1))).unwrap();
        folder.syncer.push(at(&day(i, 1))).unwrap();
        folder.syncer.collect_declared(at(&day(i, 2)), false, std::slice::from_ref(&decl)).unwrap();
    }
    History { _dir: dir, b, folder, runs }
}

/// The bucket's snapshot directories, split into husks, each holding `_collected.json`, and the rest.
fn snapshot_dirs(m: &BucketManifest) -> (BTreeSet<String>, BTreeSet<String>) {
    let dirs: BTreeSet<String> = m.entries.keys().filter_map(|k| k.strip_prefix(PREFIX)).filter_map(|rest| rest.split_once('/')).map(|(id, _)| id.to_string()).collect();
    dirs.into_iter().partition(|id| m.entries.contains_key(&format!("{PREFIX}{id}/_collected.json")))
}

/// A cold node declaring `lazy_parts`, pulled once, and its store reading through its part source.
fn cold(h: &History, id: &str) -> (Node, Store) {
    let mut n = node(id, h.b.clone(), "");
    n.syncer.config.lazy_parts = Some(true);
    n.syncer.pull(&PullScope::default()).unwrap();
    let store = n.syncer.store.clone().with_part_source(n.syncer.part_source());
    (n, store)
}

/// What `store` answers: unbounded rows and files, the files or refusal of an `as_of` at and just before every
/// snapshot the bucket ever held, and the committed rows of every run, the dedup a resent receive batch reads.
fn answers(store: &Store, h: &History) -> Vec<String> {
    let decl = table();
    let mut out = Vec::new();
    let mut rows = table_rows(store, &decl, &["id", "v"]).unwrap();
    rows.sort_by_key(|r| serde_json::Value::Object(r.clone()).to_string());
    out.push(format!("rows {rows:?}"));
    let (husks, kept) = snapshot_dirs(&manifest(h.b.as_ref()));
    let mut bounds = vec![None];
    for id in husks.iter().chain(&kept) {
        let nanos: i128 = id.trim_start_matches("snapshot-").parse().unwrap();
        let instant = contextful_core::time::Instant::from_unix_nanos(nanos).unwrap();
        bounds.push(Some(Bound::parse(&instant.to_rfc3339_nanos()).unwrap()));
        bounds.push(Some(Bound::parse(&contextful_core::time::Instant::from_unix_nanos(nanos - 1).unwrap().to_rfc3339_nanos()).unwrap()));
    }
    for bound in bounds {
        let answer = match store.state_at(&decl, bound) {
            Err(e) => format!("err {}", e.to_string().split(':').next().unwrap_or_default()),
            Ok(state) => match state.resolve(bound) {
                Ok(r) => format!("ok {:?}", r.files()),
                Err(e) => format!("err {}", e.to_string().split(':').next().unwrap_or_default()),
            },
        };
        out.push(format!("{bound:?} {answer}"));
    }
    for run in &h.runs {
        out.push(format!("{run} {:?}", store.committed_rows("filings", run).unwrap()));
    }
    out
}

/// Bring every collected snapshot manifest the pull skipped home from the bucket, as a pull without the skip leaves
/// the store.
fn unskip(n: &Node, b: &dyn ObjectStore) -> usize {
    let path = n.root().join(SKIPPED_RUNS_FILE);
    let mut record: SkippedRuns = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    let (husks, runs): (Vec<String>, Vec<String>) = record.manifests.drain(..).partition(|rel| rel.contains("/data/snapshots/"));
    for rel in &husks {
        let (bytes, _) = b.get(&format!("team/research/{rel}")).unwrap().unwrap();
        std::fs::write(n.root().join(rel), bytes).unwrap();
    }
    record.manifests = runs;
    std::fs::write(&path, serde_json::to_vec(&record).unwrap()).unwrap();
    husks.len()
}

/// A cold node pulled with the husk skip answers every unbounded read, every `as_of` and every receive dedup as one
/// whose collected snapshot manifests are home, and as the folding node does unbounded.
// spec: store.pull.husks-skipped@8df41067
#[test]
fn a_husk_skipping_pull_answers_as_a_whole_one() {
    let h = history(25);
    let (husks, kept) = snapshot_dirs(&manifest(h.b.as_ref()));
    assert!(husks.len() > 15 && !kept.is_empty(), "{husks:?} {kept:?}");
    let (skipping, skipping_store) = cold(&h, "reader-a");
    let (whole, whole_store) = cold(&h, "reader-b");
    let brought = unskip(&whole, h.b.as_ref());
    assert_eq!(brought, husks.len(), "the pull skipped every husk");
    for id in &husks {
        assert!(!skipping.root().join(format!("tables/filings/data/snapshots/{id}/_manifest.json")).exists());
    }
    let skipped = answers(&skipping_store, &h);
    assert_eq!(skipped, answers(&whole_store, &h));
    assert_eq!(skipped[0], answers(&h.folder.syncer.store, &h)[0], "the folding node reads the same rows");
    assert!(skipped.iter().any(|a| a.contains("ok")) && skipped.iter().any(|a| a.contains("StoreAsOfUnretained")), "{skipped:?}");
}

/// A cold lazy pull downloads the manifests of the retained snapshots alone: it grows with them, not with history.
// spec: store.pull.husks-skipped@8df41067
#[test]
fn a_cold_pull_grows_with_retained_snapshots() {
    for passes in [10, 30] {
        let h = history(passes);
        let (husks, kept) = snapshot_dirs(&manifest(h.b.as_ref()));
        let mut n = node("reader", h.b.clone(), "");
        n.syncer.config.lazy_parts = Some(true);
        let report = n.syncer.pull(&PullScope::default()).unwrap();
        let snapshots = report.downloaded.iter().filter(|k| k.starts_with(PREFIX) && k.ends_with("/_manifest.json")).count();
        assert_eq!(snapshots, kept.len(), "{passes} passes: {} husks", husks.len());
        assert!(kept.len() <= 4, "{kept:?}");
    }
}

/// Where no retained snapshot's index reads valid any more, the walk fetches the husks the pull skipped through the
/// part source, and answers as a node whose husks are home and whose indexes broke alike.
// spec: store.pull.husks-fetched@2bad778e
#[test]
fn a_walk_without_a_valid_index_fetches_the_skipped_husks() {
    let h = history(25);
    let (skipping, skipping_store) = cold(&h, "reader-a");
    let (whole, whole_store) = cold(&h, "reader-b");
    unskip(&whole, h.b.as_ref());
    // Every folded-runs segment goes from both stores, so each index carrying one reads invalid.
    for n in [&skipping, &whole] {
        let snapshots = n.root().join("tables/filings/data/snapshots");
        let mut removed = 0;
        for dir in std::fs::read_dir(&snapshots).unwrap() {
            let folded = dir.unwrap().path().join("folded");
            if folded.is_dir() {
                std::fs::remove_dir_all(&folded).unwrap();
                removed += 1;
            }
        }
        assert!(removed > 0, "a segment existed");
    }
    assert_eq!(answers(&skipping_store, &h), answers(&whole_store, &h));
    let record: SkippedRuns = serde_json::from_slice(&std::fs::read(skipping.root().join(SKIPPED_RUNS_FILE)).unwrap()).unwrap();
    let husks: Vec<&String> = record.manifests.iter().filter(|rel| rel.contains("/data/snapshots/")).collect();
    assert!(husks.iter().any(|rel| skipping.root().join(rel).exists()), "the walk fetched a skipped husk");
}

/// Collect reads the pointer's index and the retained snapshots' manifests, not every husk, and reclaims what the whole
/// walk reclaims; a table whose bucket index does not read valid walks the whole chain.
// spec: store.collect.index-walk@dc3175aa
#[test]
fn a_collect_reads_the_index_instead_of_every_husk() {
    let h = history(25);
    let decl = table();
    let (husks, kept) = snapshot_dirs(&manifest(h.b.as_ref()));
    let collect = |dry_run: bool| {
        let watched = Arc::new(Scripted { inner: h.b.clone(), script: Script::default() });
        let collector = crate::collect::rebucketed(&h.folder, watched.clone());
        let report = collector.collect_declared(at(&day(26, 0)), dry_run, std::slice::from_ref(&decl)).unwrap();
        let read = watched.script.gets.lock().unwrap().iter().filter(|k| k.contains("/data/snapshots/") && k.ends_with("/_manifest.json")).count();
        (report, read)
    };
    let (indexed, read) = collect(true);
    assert!(read <= kept.len() + 1, "{read} manifests read for {} retained", kept.len());
    assert!(indexed.superseded.dropped + indexed.folded_runs.dropped > 0, "the collect reclaims something: {indexed:?}");

    // With the head's segment gone from the bucket and the collector, the collect walks every husk and reclaims alike.
    let head = h.folder.syncer.store.chain("filings").unwrap().0[0].clone();
    let ix = head.folded_runs.clone().unwrap();
    assert!(!ix.segments.is_empty(), "the head carries a segment");
    for seg in &ix.segments {
        let rel = format!("tables/filings/{}", seg.location(&head.snapshot_id));
        h.b.delete(&format!("team/research/{rel}")).unwrap();
        let _ = std::fs::remove_file(h.folder.root().join(&rel));
    }
    let (walked, read) = collect(true);
    assert!(read >= husks.len(), "{read} manifests read for {} husks", husks.len());
    assert_eq!(walked.superseded, indexed.superseded);
    assert_eq!(walked.folded_runs, indexed.folded_runs);
    collect(false);
}
