//! `store.pull.folded-runs-fetched`: a store whose lazy pull left folded run manifests in the
//! bucket answers every reader of its runs as a store holding them does, or refuses.

use crate::support::{at, decl, Fixture};
use contextful_context::fold::fold;
use contextful_context::Store;
use contextful_core::store::bound_time::{Bound, Bounds};
use contextful_core::store::declare::TableDecl;
use contextful_core::store::lay_out::{RunManifest, SkippedRuns, SKIPPED_RUNS_FILE};
use contextful_core::store::object::{PartHold, PartSource};
use contextful_core::store::StoreError;
use contextful_context::ContextError;
use serde_json::json;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

/// A source restoring each stashed manifest it is asked for, recording every ask.
#[derive(Debug, Default)]
struct Manifests {
    root: PathBuf,
    stash: Vec<(String, Vec<u8>)>,
    asked: Mutex<Vec<String>>,
}

impl PartSource for Manifests {
    fn hold(&self, files: &[String]) -> Result<PartHold, StoreError> {
        self.asked.lock().unwrap().extend(files.iter().cloned());
        for (rel, bytes) in &self.stash {
            if files.contains(rel) {
                std::fs::write(self.root.join(rel), bytes).unwrap();
            }
        }
        Ok(PartHold(Arc::new(())))
    }
}

/// `filings` with runs 1 and 2 folded at 03:00 and run 3 landed after.
fn folded_store() -> (Fixture, TableDecl) {
    let f = Fixture::new();
    let d = decl("name = \"filings\"\nprimary_key = [\"doc\"]");
    f.land(&d, "run-1", json!([{ "doc": "a", "v": 1 }]), "2030-01-01T00:00:00Z").unwrap();
    f.land(&d, "run-2", json!([{ "doc": "a", "v": 2 }]), "2030-01-01T02:00:00Z").unwrap();
    fold(&f.store, &d, at("2030-01-01T03:00:00Z")).unwrap();
    f.land(&d, "run-3", json!([{ "doc": "b", "v": 3 }]), "2030-01-01T04:00:00Z").unwrap();
    (f, d)
}

/// Leave the store as a lazy pull does: each run manifest the pointer's snapshot chain
/// folds is removed and recorded skipped. Returns the removed manifests and their bytes.
fn skip_folded(f: &Fixture, table: &str) -> Vec<(String, Vec<u8>)> {
    let (chain, _) = f.store.chain(table).unwrap();
    let mut stash = Vec::new();
    for key in chain.iter().flat_map(|s| s.includes_runs.iter()) {
        let rel = format!("tables/{table}/data/runs/{key}/_manifest.json");
        let path = f.store.root().join(&rel);
        stash.push((rel, std::fs::read(&path).unwrap()));
        std::fs::remove_file(&path).unwrap();
    }
    assert!(!stash.is_empty(), "the chain folds runs");
    let mut skipped = SkippedRuns { manifests: stash.iter().map(|(rel, _)| rel.clone()).collect() };
    skipped.manifests.sort();
    std::fs::write(f.store.root().join(SKIPPED_RUNS_FILE), serde_json::to_vec(&skipped).unwrap()).unwrap();
    stash
}

fn lazy(f: &Fixture, stash: Vec<(String, Vec<u8>)>) -> (Store, Arc<Manifests>) {
    let source = Arc::new(Manifests { root: f.store.root().to_path_buf(), stash, ..Manifests::default() });
    (f.store.clone().with_part_source(source.clone()), source)
}

fn keys(runs: &[RunManifest]) -> Vec<String> {
    let mut keys: Vec<String> = runs.iter().map(RunManifest::key).collect();
    keys.sort();
    keys
}

fn as_of(v: &str) -> Bounds {
    Bounds { as_of: Some(Bound::parse(v).unwrap()), ..Bounds::default() }
}

#[cfg(feature = "read")]
fn query(store: &Store, d: &TableDecl, bounds: Bounds) -> Vec<Vec<Option<String>>> {
    let s = contextful_context::scan::scan(store, d, bounds).unwrap();
    crate::support::query(&format!("WITH t AS ({}) SELECT doc, v FROM t ORDER BY doc", s.relation))
}

fn refusal(e: ContextError) -> StoreError {
    match e {
        ContextError::Store(s) => s,
        other => panic!("not a store refusal: {other}"),
    }
}

/// Listing a table's committed runs, with or without the held commit sequence, and counting
/// one run's rows fetch each skipped manifest and answer as the whole store does.
// spec: store.pull.folded-runs-fetched@ff7203dc
#[test]
fn a_store_skipping_folded_runs_lists_every_run_through_its_part_source() {
    let (f, _) = folded_store();
    let whole = keys(&f.store.committed_runs("filings").unwrap());
    let held = f.store.committed_runs_held("filings").unwrap().1;
    let rows = f.store.committed_rows("filings", "run-1").unwrap();
    assert_eq!(whole.len(), 3);
    assert_eq!(rows, Some(1));

    let stash = skip_folded(&f, "filings");
    let (store, source) = lazy(&f, stash);
    assert_eq!(store.committed_rows("filings", "run-1").unwrap(), rows);
    assert_eq!(keys(&store.committed_runs("filings").unwrap()), whole);
    assert_eq!(store.committed_runs_held("filings").unwrap().1, held);
    assert!(!source.asked.lock().unwrap().is_empty(), "the listing asked the source");
}

/// An unbounded read resolves the current snapshot and the runs it omits without asking for
/// a folded manifest; an `as_of` read resolving before that snapshot fetches them and
/// answers the rows the whole store answers.
#[cfg(feature = "read")]
// spec: store.pull.folded-runs-fetched@ff7203dc
#[test]
fn an_as_of_read_fetches_the_folded_runs_it_resolves_and_an_unbounded_read_none() {
    let (f, d) = folded_store();
    let latest = query(&f.store, &d, Bounds::default());
    let earlier = query(&f.store, &d, as_of("2030-01-01T01:00:00Z"));
    assert_eq!(earlier, [[Some("a".into()), Some("1".into())]]);

    let stash = skip_folded(&f, "filings");
    let (store, source) = lazy(&f, stash);
    assert_eq!(query(&store, &d, Bounds::default()), latest);
    let manifests = || source.asked.lock().unwrap().iter().filter(|f| f.ends_with("/_manifest.json")).count();
    assert_eq!(manifests(), 0, "an unbounded read asks for no folded manifest");
    assert_eq!(query(&store, &d, as_of("2030-01-01T01:00:00Z")), earlier);
    assert!(manifests() > 0, "an earlier `as_of` asks for them");
}

/// Without a part source, a store holding skipped manifests refuses to list its runs or
/// resolve an earlier `as_of`, and still answers an unbounded read.
// spec: store.pull.folded-runs-unreachable@c445f763
#[test]
fn without_a_part_source_skipped_runs_refuse_rather_than_drop_rows() {
    let (f, d) = folded_store();
    skip_folded(&f, "filings");
    let err = refusal(f.store.committed_runs("filings").unwrap_err());
    assert!(matches!(err, StoreError::SyncFoldedRunsUnfetched(_)), "{err}");
    let err = refusal(f.store.committed_rows("filings", "run-1").unwrap_err());
    assert!(matches!(err, StoreError::SyncFoldedRunsUnfetched(_)), "{err}");
    let err = contextful_context::scan::scan(&f.store, &d, as_of("2030-01-01T01:00:00Z")).unwrap_err().to_string();
    assert!(err.starts_with("SyncFoldedRunsUnfetched"), "{err}");
    assert!(contextful_context::scan::scan(&f.store, &d, Bounds::default()).is_ok());
}

/// A skipped manifest the part source leaves absent refuses as a collected part.
// spec: store.pull.folded-run-collected@7a1472fe
#[test]
fn a_skipped_manifest_the_source_cannot_bring_refuses_as_collected() {
    let (f, _) = folded_store();
    skip_folded(&f, "filings");
    let (store, _) = lazy(&f, Vec::new());
    let err = refusal(store.committed_runs("filings").unwrap_err());
    assert!(matches!(&err, StoreError::SyncPartCollected(m) if m.contains("run-1")), "{err}");
}

/// A source restoring every stashed manifest save those a collection took, which it answers as collected.
#[derive(Debug, Default)]
struct Collected {
    inner: Manifests,
    gone: Vec<String>,
}

impl PartSource for Collected {
    fn hold(&self, files: &[String]) -> Result<PartHold, StoreError> {
        if let Some(rel) = files.iter().find(|f| self.gone.contains(f)) {
            return Err(StoreError::SyncPartCollected(format!("`{rel}` is gone from the bucket")));
        }
        self.inner.hold(files)
    }
}

/// An unbounded listing leaves out a skipped manifest the source answers as collected and lists every other run;
/// an `as_of` read needing it still refuses as collected.
// spec: store.pull.folded-run-collected@7a1472fe
#[test]
fn an_unbounded_listing_leaves_out_a_collected_manifest() {
    let (f, d) = folded_store();
    let stash = skip_folded(&f, "filings");
    let gone = stash.iter().find(|(rel, _)| rel.contains("/run-1/")).map(|(rel, _)| rel.clone()).unwrap();
    let source = Arc::new(Collected { inner: Manifests { root: f.store.root().to_path_buf(), stash, ..Manifests::default() }, gone: vec![gone] });
    let store = f.store.clone().with_part_source(source);
    let listed = keys(&store.committed_runs("filings").unwrap());
    assert!(!listed.is_empty());
    assert_eq!(listed, ["run-2/ingest-a", "run-3/ingest-a"]);
    let err = contextful_context::scan::scan(&store, &d, as_of("2030-01-01T01:00:00Z")).unwrap_err().to_string();
    assert!(err.starts_with("SyncPartCollected"), "{err}");
}

/// The request ledger's fold reads each run's commit instant through the fetched manifests,
/// merging exactly the files the whole store merges.
#[test]
fn the_request_ledger_fold_reads_folded_runs_through_the_source() {
    use contextful_core::store::lay_out::NodeId;
    use contextful_core::store::ledger::RequestRecord;
    let run = |f: &Fixture| -> Vec<String> {
        contextful_context::ledger::files(&f.store, "filings").unwrap().iter().map(|p| p.file_name().unwrap().to_string_lossy().into_owned()).collect()
    };
    let build = || {
        let (f, _) = folded_store();
        for r in ["run-1", "run-3"] {
            let record = RequestRecord {
                request_id: format!("{r}-q"),
                vendor_request_id: None,
                connector: "http".into(),
                method: "GET".into(),
                url_host: "api.example.org".into(),
                status_code: Some(200),
                started_at: at("2030-01-01T00:00:00Z"),
                duration_ms: 3,
                batch_seq: Some(0),
            };
            contextful_context::ledger::append(&f.store, "filings", r, &NodeId::parse("ingest-a").unwrap(), &[record]).unwrap();
        }
        f
    };
    let whole = build();
    contextful_context::ledger::fold_ledgers(&whole.store, "filings", "snapshot-x", at("2030-01-02T00:00:00Z")).unwrap();
    let expected = run(&whole);
    assert!(!expected.is_empty());

    let f = build();
    let stash = skip_folded(&f, "filings");
    let (store, _) = lazy(&f, stash);
    contextful_context::ledger::fold_ledgers(&store, "filings", "snapshot-x", at("2030-01-02T00:00:00Z")).unwrap();
    assert_eq!(run(&f), expected);
}

/// A subscription resuming after a folded run delivers what it delivers on the whole store.
#[test]
fn a_subscription_resumes_over_fetched_folded_runs() {
    use contextful_context::subscribe::Subscription;
    use std::collections::BTreeMap;
    let positions = |store: &Store| -> Vec<String> {
        let tables = vec!["filings".to_string()];
        let mut s = Subscription::open(store, &tables, &BTreeMap::new()).unwrap();
        let messages = s.poll().unwrap();
        messages.iter().map(|m| format!("{m:?}")).collect()
    };
    let (f, _) = folded_store();
    let whole = positions(&f.store);
    assert!(!whole.is_empty());
    let stash = skip_folded(&f, "filings");
    let (store, _) = lazy(&f, stash);
    assert_eq!(positions(&store), whole);
}

/// The derived catalog's run rows, rebuilt from the tree, list each folded run.
#[test]
fn the_derived_catalog_lists_fetched_folded_runs() {
    let (f, _) = folded_store();
    let whole = contextful_context::catalog::derive(&f.store).unwrap().runs.len();
    assert_eq!(whole, 3);
    let stash = skip_folded(&f, "filings");
    let (store, _) = lazy(&f, stash);
    assert_eq!(contextful_context::catalog::derive(&store).unwrap().runs.len(), whole);
}

/// A later landing on a store skipping folded runs takes the commit sequence the whole store
/// assigns: the pointer's snapshot records the highest its folded runs carry.
#[test]
fn a_landing_after_skipped_runs_takes_the_whole_stores_commit_sequence() {
    let next = |f: &Fixture, d: &TableDecl| f.land(d, "run-4", json!([{ "doc": "c", "v": 4 }]), "2030-01-01T05:00:00Z").unwrap().commit_seq;
    let (whole, d) = folded_store();
    std::fs::remove_file(whole.table_dir("filings").join("data/.commit_seq")).unwrap();
    let expected = next(&whole, &d);
    assert!(expected.is_some());
    let (f, d) = folded_store();
    skip_folded(&f, "filings");
    // A cold store holds no node-local commit counter.
    std::fs::remove_file(f.table_dir("filings").join("data/.commit_seq")).unwrap();
    assert_eq!(next(&f, &d), expected);
}
