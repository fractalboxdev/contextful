//! `store.fold`: a pass, its publication through the pointer, and what it collects.

use crate::support::{at, decl, s, Fixture};
use contextful_context::fold::{commit, fold, prepare, Committed, Prepared};
use contextful_core::store::bound_time::{Bound, Bounds};
use contextful_core::store::fold::FoldOutcome;
use contextful_core::store::StoreError;
use serde_json::json;
use std::fs;

fn staged(f: &Fixture, d: &contextful_core::store::declare::TableDecl, now: &str) -> contextful_context::fold::Staged {
    match prepare(&f.store, d, at(now)).unwrap() {
        Prepared::Staged(s) => *s,
        Prepared::NothingLanded => panic!("nothing staged"),
    }
}

/// A pass selects the committed runs the current snapshot omits, dedupes by key or unions, reconciles the schema, sorts by `cluster_by`, partitions, writes Parquet and every declared sidecar into staging, then commits by {{store.fold.pointer-commit}}.
// spec: store.fold.pass@65833c40
#[test]
fn a_pass_folds_the_omitted_runs_into_one_snapshot() {
    let f = Fixture::new();
    let d = decl("name = \"filings\"\nprimary_key = [\"doc\"]\norder_by = \"rev\"\ncluster_by = [\"issuer\", \"doc\"]");
    f.land(&d, "run-1", json!([{"doc": "a", "rev": 1, "issuer": "z"}, {"doc": "b", "rev": 1, "issuer": "y"}]), "2030-01-01T00:00:00Z").unwrap();
    f.land(&d, "run-2", json!([{"doc": "a", "rev": 2, "issuer": "x", "pages": 4}]), "2030-01-01T00:01:00Z").unwrap();
    let before = f.query(&d, Bounds::default(), "SELECT doc, rev, issuer, pages FROM t ORDER BY doc");

    let outcome = fold(&f.store, &d, at("2030-01-01T01:00:00Z")).unwrap();
    let FoldOutcome::Folded { runs, rows, .. } = outcome else { panic!("{outcome:?}") };
    assert_eq!((runs, rows), (2, 2));
    let files = f.scan(&d, Bounds::default()).unwrap().files;
    assert_eq!(files.len(), 1);
    // Deduped, reconciled and clustered: rows in `issuer` order within the one file.
    let path = f.store.root().join(&files[0]);
    let raw = crate::support::query(&format!("SELECT doc, issuer FROM read_parquet('{}')", path.display()));
    assert_eq!(raw, [[s("a"), s("x")], [s("b"), s("y")]]);
    assert_eq!(f.query(&d, Bounds::default(), "SELECT doc, rev, issuer, pages FROM t ORDER BY doc"), before);

    // A second pass with nothing new lands nothing.
    assert_eq!(fold(&f.store, &d, at("2030-01-01T02:00:00Z")).unwrap(), FoldOutcome::NothingLanded);
}

/// A keyed table declaring `valid_time` partitions on the key together with the valid-time line and keeps one row per line.
// spec: store.fold.valid-time-line@4db5edf4
#[test]
fn a_valid_time_table_keeps_one_row_per_key_and_line() {
    let f = Fixture::new();
    let d = decl("name = \"prices\"\nprimary_key = [\"sku\"]\n[pipeline.tables.valid_time]\nfrom = \"valid_from\"");
    let ts = [("valid_from", contextful_core::store::reconcile::ColumnType::Timestamp)];
    f.land_typed(&d, "run-1", json!([{"sku": "a", "valid_from": "2030-01-01T00:00:00Z", "price": 1}, {"sku": "a", "valid_from": "2030-02-01T00:00:00Z", "price": 2}]), "2030-01-01T00:00:00Z", &ts).unwrap();
    f.land_typed(&d, "run-2", json!([{"sku": "a", "valid_from": "2030-02-01T00:00:00Z", "price": 3}]), "2030-01-01T00:01:00Z", &ts).unwrap();
    let FoldOutcome::Folded { rows, .. } = fold(&f.store, &d, at("2030-01-01T01:00:00Z")).unwrap() else { panic!() };
    assert_eq!(rows, 2);
    let path = f.store.root().join(&f.scan(&d, Bounds::default()).unwrap().files[0]);
    let raw = crate::support::query(&format!("SELECT price FROM read_parquet('{}') ORDER BY valid_from", path.display()));
    assert_eq!(raw, [[s("1")], [s("3")]]);
}

/// A snapshot's `includes_runs` names each run it folded as `<run-id>/<node-id>`, the run's own directory; a run committed afterwards reads on top of it.
// spec: store.fold.includes-runs@f8549a98
#[test]
fn a_snapshot_names_the_runs_it_folded() {
    let f = Fixture::new();
    let d = decl("name = \"events\"");
    f.land(&d, "run-1", json!([{"e": 1}]), "2030-01-01T00:00:00Z").unwrap();
    f.land(&d, "run-2", json!([{"e": 2}]), "2030-01-01T00:01:00Z").unwrap();
    fold(&f.store, &d, at("2030-01-01T01:00:00Z")).unwrap();
    f.land(&d, "run-3", json!([{"e": 3}]), "2030-01-01T02:00:00Z").unwrap();
    let (chain, _) = f.store.chain("events").unwrap();
    assert_eq!(chain[0].includes_runs, ["run-1/ingest-a", "run-2/ingest-a"]);
    let files = f.scan(&d, Bounds::default()).unwrap().files;
    assert_eq!(files.len(), 2);
    assert!(files.iter().any(|p| p.contains("/runs/run-3/")));
    assert_eq!(f.query(&d, Bounds::default(), "SELECT e FROM t ORDER BY e"), [[s("1")], [s("2")], [s("3")]]);
}

/// `retain_runs` defaults to 7 d; a folded run, a superseded snapshot and its sidecars are collected once older than the window.
// spec: store.fold.retention@b5065918
#[test]
fn folded_runs_and_superseded_snapshots_are_collected_after_seven_days() {
    let f = Fixture::new();
    let d = decl("name = \"events\"");
    f.land(&d, "run-1", json!([{"e": 1}]), "2030-01-01T00:00:00Z").unwrap();
    fold(&f.store, &d, at("2030-01-01T01:00:00Z")).unwrap();
    let (first, _) = f.store.chain("events").unwrap();
    let first_dir = f.store.snapshot_dir("events", &first[0].snapshot_id).unwrap();
    let run_dir = f.table_dir("events").join("data/runs/run-1");

    // Superseded, but inside the window: both stay, and a bounded read reaches the older snapshot.
    f.land(&d, "run-2", json!([{"e": 2}]), "2030-01-02T00:00:00Z").unwrap();
    fold(&f.store, &d, at("2030-01-02T01:00:00Z")).unwrap();
    assert!(first_dir.is_dir() && run_dir.is_dir());

    // 6 days 23 hours after the second pass: still retained.
    f.land(&d, "run-3", json!([{"e": 3}]), "2030-01-09T00:00:00Z").unwrap();
    fold(&f.store, &d, at("2030-01-09T00:00:00Z")).unwrap();
    assert!(first_dir.is_dir(), "a snapshot superseded 6 d 23 h ago was collected");
    assert!(!run_dir.exists(), "run-1 was folded 8 days ago");

    // 7 days after the second pass supersedes it, the first snapshot is collected.
    f.land(&d, "run-4", json!([{"e": 4}]), "2030-01-09T01:00:00Z").unwrap();
    fold(&f.store, &d, at("2030-01-09T01:00:00Z")).unwrap();
    assert!(!first_dir.exists());
    assert_eq!(f.query(&d, Bounds::default(), "SELECT count(*) FROM t"), [[s("4")]]);
    let bound = Bounds { as_of: Some(Bound::parse("2030-01-01T12:00:00Z").unwrap()), valid_as_of: None };
    match f.scan(&d, bound).unwrap_err().store() {
        Some(StoreError::StoreAsOfUnretained(_)) => {}
        other => panic!("expected StoreAsOfUnretained, got {other:?}"),
    }
}

#[test]
fn a_pass_reports_folded_or_nothing_landed() {
    let f = Fixture::new();
    let d = decl("name = \"events\"");
    f.land(&d, "run-1", json!([{"e": 1}]), "2030-01-01T00:00:00Z").unwrap();
    assert!(matches!(fold(&f.store, &d, at("2030-01-01T01:00:00Z")).unwrap(), FoldOutcome::Folded { .. }));
    assert_eq!(fold(&f.store, &d, at("2030-01-01T02:00:00Z")).unwrap(), FoldOutcome::NothingLanded);
    assert_eq!(FoldOutcome::NothingLanded.to_string(), "nothing-landed");
    assert!(FoldOutcome::Failed("x".into()).to_string().starts_with("failed"));
}

/// A pass naming a table no `schema.json` declares halts the command with {{store.lay-out.unknown-table}}.
// spec: store.fold.unknown-table@a3fe0968
#[test]
fn a_pass_over_an_unknown_table_halts() {
    let f = Fixture::new();
    let err = fold(&f.store, &decl("name = \"filling\""), at("2030-01-01T00:00:00Z")).unwrap_err();
    assert!(matches!(err.store(), Some(StoreError::StoreUnknownTable(_))), "{err}");
}

/// A pass publishes by replacing `_pointer.json` conditioned on the ETag it read at pass start, with `If-Match` on an object store and a version-checked rename on a filesystem.
// spec: store.fold.pointer-commit@1a70277c
#[test]
fn the_pointer_replace_is_conditioned_on_the_etag_read_at_pass_start() {
    let f = Fixture::new();
    let d = decl("name = \"events\"");
    f.land(&d, "run-1", json!([{"e": 1}]), "2030-01-01T00:00:00Z").unwrap();
    let a = staged(&f, &d, "2030-01-01T01:00:00Z");
    assert_eq!(a.etag, "absent");
    let Committed::Published(m) = commit(&f.store, a).unwrap() else { panic!("the first commit lost") };
    let (ptr, etag) = f.store.pointer("events").unwrap().unwrap();
    assert_eq!(ptr.snapshot_id, m.snapshot_id);
    assert_eq!(etag.len(), 64);

    // A held lock is a lost condition, never a wait.
    f.land(&d, "run-2", json!([{"e": 2}]), "2030-01-01T02:00:00Z").unwrap();
    let b = staged(&f, &d, "2030-01-01T03:00:00Z");
    fs::write(f.table_dir("events").join("_pointer.json.lock"), b"").unwrap();
    assert_eq!(commit(&f.store, b).unwrap(), Committed::Lost);
    assert_eq!(f.store.pointer("events").unwrap().unwrap().0.snapshot_id, m.snapshot_id);
}

/// A reader observes a snapshot and every declared sidecar together or neither; a commit exposing one without the other raises `StorePartialSnapshot`.
// spec: store.fold.partial-snapshot@f0abb63e
#[test]
fn a_snapshot_missing_a_file_it_names_is_not_published() {
    let f = Fixture::new();
    let d = decl("name = \"events\"");
    f.land(&d, "run-1", json!([{"e": 1}]), "2030-01-01T00:00:00Z").unwrap();
    let st = staged(&f, &d, "2030-01-01T01:00:00Z");
    fs::remove_file(st.staging.join("part-00000.parquet")).unwrap();
    let err = commit(&f.store, st).unwrap_err();
    assert!(matches!(err.store(), Some(StoreError::StorePartialSnapshot(_))), "{err}");
    assert!(f.store.pointer("events").unwrap().is_none());

    // A declared sidecar absent from the directory refuses the same way.
    let mut st2 = staged(&f, &d, "2030-01-01T02:00:00Z");
    st2.manifest.indexes.push(json!({"kind": "full-text", "column": "e", "path": "indexes/fts-e", "key_version": 0}));
    let err = commit(&f.store, st2).unwrap_err();
    assert!(matches!(err.store(), Some(StoreError::StorePartialSnapshot(_))), "{err}");
    assert!(f.store.pointer("events").unwrap().is_none());
}

/// A pass whose pointer replace loses its condition publishes nothing, and its staged snapshot is collected.
// spec: store.fold.lost-pointer@f40f34ab
#[test]
fn a_pass_losing_the_pointer_publishes_nothing() {
    let f = Fixture::new();
    let d = decl("name = \"events\"");
    f.land(&d, "run-1", json!([{"e": 1}]), "2030-01-01T00:00:00Z").unwrap();
    let a = staged(&f, &d, "2030-01-01T01:00:00Z");
    let b = staged(&f, &d, "2030-01-01T01:00:00.5Z");
    let b_id = b.manifest.snapshot_id.clone();
    let Committed::Published(won) = commit(&f.store, a).unwrap() else { panic!() };
    assert_eq!(commit(&f.store, b).unwrap(), Committed::Lost);
    assert_eq!(f.store.pointer("events").unwrap().unwrap().0.snapshot_id, won.snapshot_id);
    let loser = f.store.snapshot_dir("events", &b_id).unwrap();
    assert!(loser.is_dir());
    assert_eq!(f.scan(&d, Bounds::default()).unwrap().files.iter().filter(|p| p.contains(&b_id.to_string())).count(), 0);

    // The next pass collects it.
    f.land(&d, "run-2", json!([{"e": 2}]), "2030-01-01T02:00:00Z").unwrap();
    fold(&f.store, &d, at("2030-01-01T03:00:00Z")).unwrap();
    assert!(!loser.exists());
}

/// A staging directory, and a snapshot directory no pointer chain reaches, are collected by the next pass and read by nobody.
// spec: store.fold.staging-collected@7f61f813
#[test]
fn the_next_pass_collects_staging_and_unreachable_snapshots() {
    let f = Fixture::new();
    let d = decl("name = \"events\"");
    f.land(&d, "run-1", json!([{"e": 1}]), "2030-01-01T00:00:00Z").unwrap();
    let abandoned = staged(&f, &d, "2030-01-01T01:00:00Z").staging;
    let stray = f.table_dir("events").join("data/snapshots/snapshot-00000000000000000001");
    fs::create_dir_all(&stray).unwrap();
    assert!(abandoned.is_dir());
    fold(&f.store, &d, at("2030-01-01T02:00:00Z")).unwrap();
    assert!(!abandoned.exists() && !stray.exists());
    assert_eq!(fs::read_dir(f.table_dir("events").join("data/snapshots")).unwrap().count(), 1);
}

/// A new snapshot supersedes the previous one without deleting it, and a bounded read reaches the older one until retention collects it.
// spec: store.fold.supersedes@97cbcf3a
#[test]
fn a_new_snapshot_supersedes_without_deleting() {
    let f = Fixture::new();
    let d = decl("name = \"events\"\nprimary_key = [\"k\"]");
    f.land(&d, "run-1", json!([{"k": "a", "v": 1}]), "2030-01-01T00:00:00Z").unwrap();
    fold(&f.store, &d, at("2030-01-01T01:00:00Z")).unwrap();
    f.land(&d, "run-2", json!([{"k": "a", "v": 2}]), "2030-01-01T02:00:00Z").unwrap();
    fold(&f.store, &d, at("2030-01-01T03:00:00Z")).unwrap();
    let (chain, _) = f.store.chain("events").unwrap();
    assert_eq!(chain.len(), 2);
    assert!(f.store.snapshot_dir("events", &chain[1].snapshot_id).unwrap().is_dir());
    let old = Bounds { as_of: Some(Bound::parse("2030-01-01T01:30:00Z").unwrap()), valid_as_of: None };
    assert!(f.scan(&d, old).unwrap().files[0].contains(&chain[1].snapshot_id.to_string()));
    assert_eq!(f.query(&d, old, "SELECT v FROM t"), [[s("1")]]);
    assert_eq!(f.query(&d, Bounds::default(), "SELECT v FROM t"), [[s("2")]]);
}

/// A pointer lock a crashed pass left behind stops no later pass once it is stale.
#[test]
fn a_stale_pointer_lock_is_cleared() {
    let f = Fixture::new();
    let d = decl("name = \"events\"");
    f.land(&d, "run-1", json!([{"e": 1}]), "2030-01-01T00:00:00Z").unwrap();
    let lock = f.table_dir("events").join("_pointer.json.lock");
    let file = fs::File::create(&lock).unwrap();
    file.set_modified(std::time::SystemTime::now() - std::time::Duration::from_secs(3600)).unwrap();
    drop(file);
    assert!(matches!(fold(&f.store, &d, at("2030-01-01T01:00:00Z")).unwrap(), FoldOutcome::Folded { .. }));
    assert!(!lock.exists());
}

/// A batch carrying a column the merged schema lacks refuses to conform rather than lose the column.
#[test]
fn conforming_never_drops_a_column() {
    use arrow_array::{ArrayRef, Int64Array, RecordBatch};
    use contextful_core::store::reconcile::{Column, ColumnType, Schema};
    use std::sync::Arc;
    let target = contextful_context::parquet_io::arrow_schema(&Schema { columns: vec![Column::new("a", ColumnType::Int64, true)] });
    let wide = RecordBatch::try_from_iter([
        ("a", Arc::new(Int64Array::from(vec![1])) as ArrayRef),
        ("c", Arc::new(Int64Array::from(vec![2])) as ArrayRef),
    ])
    .unwrap();
    let err = contextful_context::parquet_io::conform(&wide, &target).unwrap_err();
    assert!(err.to_string().contains("`c`"), "{err}");
}

/// Retention ages from the fold, not from a landing: a table that stops receiving runs
/// still collects a snapshot the window has passed, on a pass with nothing to fold.
#[test]
fn an_idle_table_still_collects_what_retention_allows() {
    let f = Fixture::new();
    let d = decl("name = \"events\"\nretain_runs = \"1d\"");
    f.land(&d, "run-1", json!([{"e": 1}]), "2030-01-01T00:00:00Z").unwrap();
    fold(&f.store, &d, at("2030-01-01T01:00:00Z")).unwrap();
    let (first, _) = f.store.chain("events").unwrap();
    let first_dir = f.store.snapshot_dir("events", &first[0].snapshot_id).unwrap();
    let run_dir = f.table_dir("events").join("data/runs/run-1");

    f.land(&d, "run-2", json!([{"e": 2}]), "2030-01-01T02:00:00Z").unwrap();
    fold(&f.store, &d, at("2030-01-01T03:00:00Z")).unwrap();
    assert!(first_dir.is_dir() && run_dir.is_dir(), "inside the window, both stay");

    // Nothing lands again. The pass folds nothing and still collects both.
    assert_eq!(fold(&f.store, &d, at("2030-01-03T00:00:00Z")).unwrap(), FoldOutcome::NothingLanded);
    assert!(!first_dir.exists(), "a snapshot superseded 2 d ago survived an idle pass");
    assert!(!run_dir.exists(), "a run folded 2 d ago survived an idle pass");
    assert_eq!(f.query(&d, Bounds::default(), "SELECT count(*) FROM t"), [[s("2")]]);
}

/// A collection that fails leaves the published snapshot published: the pointer has
/// already moved, so the pass reports what it did, not what retention could not finish.
#[test]
fn a_failed_collection_does_not_unpublish_the_snapshot() {
    let f = Fixture::new();
    let d = decl("name = \"events\"\nretain_runs = \"1d\"");
    f.land(&d, "run-1", json!([{"e": 1}]), "2030-01-01T00:00:00Z").unwrap();
    fold(&f.store, &d, at("2030-01-01T01:00:00Z")).unwrap();
    f.land(&d, "run-2", json!([{"e": 2}]), "2030-01-01T02:00:00Z").unwrap();

    // Retention alone reads the window, so a declaration carrying one outside the grammar
    // fails the collection and nothing else: staging and the pointer replace are untouched.
    let mut broken = d.clone();
    broken.retain_runs = Some("1 day".into());
    assert!(broken.retain_runs_secs().is_err(), "the window still parses");
    let run_dir = f.table_dir("events").join("data/runs/run-1");
    assert!(run_dir.is_dir());

    let outcome = fold(&f.store, &broken, at("2030-01-03T00:00:00Z")).unwrap();
    assert!(matches!(outcome, FoldOutcome::Folded { .. }), "retention failure reported as {outcome:?}");
    let (chain, _) = f.store.chain("events").unwrap();
    assert_eq!(chain.len(), 2, "the pointer did not move");
    assert_eq!(f.query(&d, Bounds::default(), "SELECT count(*) FROM t"), [[s("2")]]);
    assert!(run_dir.is_dir(), "the collection ran despite its window");

    // The same pass with a window that parses collects what it could not before.
    fold(&f.store, &d, at("2030-01-03T01:00:00Z")).unwrap();
    assert!(!run_dir.exists());
}

/// A pass whose id an earlier lost pass already promoted publishes anyway: what sits at
/// that id is the lost pass's orphan, which no pointer chain reaches, so the retry is not
/// wedged by its own earlier attempt.
#[test]
fn a_retry_at_the_same_instant_publishes_over_the_lost_passs_orphan() {
    let f = Fixture::new();
    let d = decl("name = \"events\"");
    f.land(&d, "run-1", json!([{"e": 1}]), "2030-01-01T00:00:00Z").unwrap();

    // A pass at this instant loses the pointer and leaves its snapshot promoted. The id
    // is a function of the instant and the parent, so the retry recomputes the same one.
    let now = at("2030-01-01T01:00:00Z");
    let orphan_id = contextful_core::store::lay_out::SnapshotId::next(now, None);
    let orphan = f.store.snapshot_dir("events", &orphan_id).unwrap();
    fs::create_dir_all(&orphan).unwrap();
    fs::write(orphan.join("part-00000.parquet"), b"not a snapshot any pointer reaches").unwrap();

    // The retry at that same instant publishes over it rather than failing to rename.
    let outcome = fold(&f.store, &d, now).unwrap();
    let FoldOutcome::Folded { snapshot_id, .. } = outcome else { panic!("the retry was wedged: {outcome:?}") };
    assert_eq!(snapshot_id, orphan_id.to_string(), "the retry took a different id");
    assert_eq!(f.query(&d, Bounds::default(), "SELECT count(*) FROM t"), [[s("1")]]);
}

/// A pointer lock a live pass holds is contention, not a verdict: the commit waits for it
/// and then reads the ETag, so two passes racing one table never both report failure when
/// the pointer did not move under either.
#[test]
fn a_busy_pointer_lock_is_waited_for_rather_than_read_as_a_moved_pointer() {
    let f = std::sync::Arc::new(Fixture::new());
    let d = decl("name = \"events\"");
    f.land(&d, "run-1", json!([{"e": 1}]), "2030-01-01T00:00:00Z").unwrap();
    let lock = f.table_dir("events").join("_pointer.json.lock");

    // A holder that releases well inside the wait: the pass publishes, not fails.
    fs::write(&lock, b"").unwrap();
    let releaser = {
        let lock = lock.clone();
        std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(40));
            fs::remove_file(&lock).unwrap();
        })
    };
    let st = staged(&f, &d, "2030-01-01T01:00:00Z");
    assert!(matches!(commit(&f.store, st).unwrap(), Committed::Published(_)), "contention was read as a lost pointer");
    releaser.join().unwrap();

    // A holder that outlasts the wait is still a lost condition, so a commit terminates.
    f.land(&d, "run-2", json!([{"e": 2}]), "2030-01-01T02:00:00Z").unwrap();
    let st = staged(&f, &d, "2030-01-01T03:00:00Z");
    fs::write(&lock, b"").unwrap();
    assert_eq!(commit(&f.store, st).unwrap(), Committed::Lost);
    fs::remove_file(&lock).unwrap();
}

/// A staging directory whose pass still holds its lock is in flight, whatever id it
/// carries: a concurrent pass's collection leaves it alone, so the earlier pass reaches
/// its own commit and reports a lost pointer rather than a missing directory.
#[test]
fn a_collection_leaves_an_in_flight_staging_directory_alone() {
    let f = Fixture::new();
    let d = decl("name = \"events\"");
    f.land(&d, "run-1", json!([{"e": 1}]), "2030-01-01T00:00:00Z").unwrap();

    // An earlier pass stages and holds; a later pass folds and collects around it.
    let early = staged(&f, &d, "2030-01-01T01:00:00Z");
    let early_staging = early.staging.clone();
    let later = fold(&f.store, &d, at("2030-01-01T02:00:00Z")).unwrap();
    assert!(matches!(later, FoldOutcome::Folded { .. }), "{later:?}");
    assert!(early_staging.is_dir(), "a pass in flight had its staging directory collected");

    // The earlier pass reaches its commit and loses on the ETag, as the rules read it.
    assert_eq!(commit(&f.store, early).unwrap(), Committed::Lost);

    // Once that pass ends, the next collection takes what it left.
    f.land(&d, "run-2", json!([{"e": 2}]), "2030-01-01T03:00:00Z").unwrap();
    fold(&f.store, &d, at("2030-01-01T04:00:00Z")).unwrap();
    assert!(!early_staging.exists());
}
