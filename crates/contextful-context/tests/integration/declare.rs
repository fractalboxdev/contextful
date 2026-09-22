//! `store.declare`: what a read returns for a keyed, unkeyed, replacing and empty table.

use crate::support::{at, decl, query, s, Fixture};
use contextful_context::fold::fold;
use contextful_core::store::bound_time::Bounds;
use contextful_core::store::StoreError;
use serde_json::json;
use std::fs;

/// A table declaring no `primary_key` reads as the byte-identical union of its committed runs.
// spec: store.declare.unkeyed-union@65214f13
#[test]
fn an_unkeyed_table_reads_as_the_union_of_its_runs() {
    let f = Fixture::new();
    let d = decl("name = \"events\"");
    f.land(&d, "run-1", json!([{"id": "a", "v": 1}, {"id": "a", "v": 1}]), "2030-01-01T00:00:00Z").unwrap();
    f.land(&d, "run-2", json!([{"id": "a", "v": 2}]), "2030-01-01T00:01:00Z").unwrap();
    let from_view = f.query(&d, Bounds::default(), "SELECT id, v, _run_id FROM t ORDER BY _run_id, v");
    let files = f.scan(&d, Bounds::default()).unwrap().files;
    let direct = query(&format!(
        "SELECT id, v, _run_id FROM read_parquet([{}], union_by_name = true) ORDER BY _run_id, v",
        files.iter().map(|p| format!("'{}'", f.store.root().join(p).display())).collect::<Vec<_>>().join(", ")
    ));
    assert_eq!(from_view.len(), 3, "duplicate rows survive the union");
    assert_eq!(from_view, direct);
}

/// A table declaring `primary_key` reads through `ROW_NUMBER() OVER (PARTITION BY <pk> ORDER BY <order_by> DESC, _ingested_at DESC) = 1` over its current snapshot, if any, unioned with the committed runs that snapshot omits.
// spec: store.declare.dedup-view@82752c58
#[test]
fn a_keyed_table_reads_one_row_per_key_before_and_after_a_fold() {
    let f = Fixture::new();
    let d = decl("name = \"filings\"\nprimary_key = [\"doc\"]\norder_by = \"rev\"");
    f.land(&d, "run-1", json!([{"doc": "a", "rev": 1, "title": "draft"}, {"doc": "b", "rev": 1, "title": "memo"}]), "2030-01-01T00:00:00Z").unwrap();
    // An older revision landing later does not win: `order_by` decides, then `_ingested_at`.
    f.land(&d, "run-2", json!([{"doc": "a", "rev": 3, "title": "final"}, {"doc": "b", "rev": 0, "title": "stale"}]), "2030-01-01T00:01:00Z").unwrap();
    let rel = f.scan(&d, Bounds::default()).unwrap().relation;
    assert!(rel.contains("ROW_NUMBER() OVER (PARTITION BY \"doc\" ORDER BY \"rev\" DESC, \"_ingested_at\" DESC)"), "{rel}");
    let before = f.query(&d, Bounds::default(), "SELECT doc, title FROM t ORDER BY doc");
    assert_eq!(before, [[s("a"), s("final")], [s("b"), s("memo")]]);

    // Folded, then a run the snapshot omits: the view spans both.
    fold(&f.store, &d, at("2030-01-01T01:00:00Z")).unwrap();
    f.land(&d, "run-3", json!([{"doc": "b", "rev": 2, "title": "memo v2"}]), "2030-01-01T02:00:00Z").unwrap();
    let files = f.scan(&d, Bounds::default()).unwrap().files;
    assert!(files[0].contains("/data/runs/run-3/") && files[1].contains("/data/snapshots/"), "{files:?}");
    let after = f.query(&d, Bounds::default(), "SELECT doc, title FROM t ORDER BY doc");
    assert_eq!(after, [[s("a"), s("final")], [s("b"), s("memo v2")]]);

    // A tie on `order_by` goes to the later landing.
    f.land(&d, "run-4", json!([{"doc": "a", "rev": 3, "title": "final, relanded"}]), "2030-01-01T03:00:00Z").unwrap();
    let tie = f.query(&d, Bounds::default(), "SELECT title FROM t WHERE doc = 'a'");
    assert_eq!(tie, [[s("final, relanded")]]);
}

/// A run landing zero rows commits a manifest with no parts and replaces nothing; a table with no rows registers as a zero-row relation over its declared and injected columns.
// spec: store.declare.empty-run@c261166b
#[test]
fn a_zero_row_run_commits_no_parts_and_an_empty_table_registers() {
    let f = Fixture::new();
    let d = decl("name = \"filings\"\nwrite_mode = \"replace\"");
    let m = f.land(&d, "run-0", json!([]), "2030-01-01T00:00:00Z").unwrap();
    assert!(m.parts.is_empty());
    assert!(f.scan(&d, Bounds::default()).unwrap().files.is_empty());
    let cols = query(&format!(
        "SELECT column_name FROM (DESCRIBE {}) ORDER BY column_name",
        f.scan(&d, Bounds::default()).unwrap().relation
    ));
    assert_eq!(cols, [[s("_batch_seq")], [s("_ingested_at")], [s("_run_id")], [s("_site_id")]]);
    assert_eq!(f.query(&d, Bounds::default(), "SELECT count(*) FROM t"), [[s("0")]]);

    // Under `replace`, a later zero-row run leaves the last non-empty state in place.
    f.land(&d, "run-1", json!([{"id": "a"}]), "2030-01-01T00:01:00Z").unwrap();
    f.land(&d, "run-2", json!([]), "2030-01-01T00:02:00Z").unwrap();
    assert_eq!(f.query(&d, Bounds::default(), "SELECT id FROM t"), [[s("a")]]);
}

/// A declaration key changes what a read returns and rewrites no committed part; a key added after rows land applies from the next read.
// spec: store.declare.read-side-keys@71b2bfcf
#[test]
fn a_key_added_after_rows_land_applies_at_the_next_read() {
    let f = Fixture::new();
    let unkeyed = decl("name = \"filings\"");
    f.land(&unkeyed, "run-1", json!([{"doc": "a", "title": "draft"}]), "2030-01-01T00:00:00Z").unwrap();
    f.land(&unkeyed, "run-2", json!([{"doc": "a", "title": "final"}]), "2030-01-01T00:01:00Z").unwrap();
    let parts: Vec<Vec<u8>> = f
        .scan(&unkeyed, Bounds::default())
        .unwrap()
        .files
        .iter()
        .map(|p| fs::read(f.store.root().join(p)).unwrap())
        .collect();
    assert_eq!(f.query(&unkeyed, Bounds::default(), "SELECT count(*) FROM t"), [[s("2")]]);

    let keyed = decl("name = \"filings\"\nprimary_key = [\"doc\"]");
    assert_eq!(f.query(&keyed, Bounds::default(), "SELECT title FROM t"), [[s("final")]]);
    let after: Vec<Vec<u8>> = f
        .scan(&keyed, Bounds::default())
        .unwrap()
        .files
        .iter()
        .map(|p| fs::read(f.store.root().join(p)).unwrap())
        .collect();
    assert_eq!(parts, after, "declaring a key rewrote a committed part");
}

/// A replacing run leaves the runs it displaced on disk until `retain_runs` passes, writes no erasure receipt and walks no lineage.
// spec: store.declare.replace-retains@3ffbc5fd
#[test]
fn a_replacing_run_leaves_displaced_runs_until_retention() {
    let f = Fixture::new();
    let d = decl("name = \"inventory\"\nwrite_mode = \"replace\"\nretain_runs = \"1d\"");
    f.land(&d, "run-1", json!([{"sku": "a"}, {"sku": "b"}]), "2030-01-01T00:00:00Z").unwrap();
    f.land(&d, "run-2", json!([{"sku": "b"}]), "2030-01-01T01:00:00Z").unwrap();
    assert_eq!(f.query(&d, Bounds::default(), "SELECT sku FROM t"), [[s("b")]]);
    let displaced = f.table_dir("inventory").join("data/runs/run-1/ingest-a/part-00000.parquet");
    assert!(displaced.is_file());

    // Folding keeps the displaced run through the window, then collects it.
    fold(&f.store, &d, at("2030-01-01T02:00:00Z")).unwrap();
    assert!(displaced.is_file());
    assert_eq!(f.query(&d, Bounds::default(), "SELECT sku FROM t"), [[s("b")]]);
    f.land(&d, "run-3", json!([{"sku": "c"}]), "2030-01-03T00:00:00Z").unwrap();
    fold(&f.store, &d, at("2030-01-03T01:00:00Z")).unwrap();
    assert!(!displaced.exists());
    assert!(!f.table_dir("inventory").join("requests").exists(), "a replacing run writes no receipt");
    assert_eq!(f.query(&d, Bounds::default(), "SELECT sku FROM t"), [[s("c")]]);
}

/// An `order_by` naming a column neither declared nor injected raises `StoreOrderByUnknownColumn` at validation, before the first batch.
#[test]
fn an_unknown_order_by_refuses_the_first_batch_before_any_parquet() {
    let f = Fixture::new();
    let d = decl("name = \"filings\"\nprimary_key = [\"doc\"]\norder_by = \"revised_on\"");
    let err = f.land(&d, "run-1", json!([{"doc": "a", "revised_at": 1}]), "2030-01-01T00:00:00Z").unwrap_err();
    assert!(matches!(err.store(), Some(StoreError::StoreOrderByUnknownColumn(_))), "{err}");
    assert!(!f.table_dir("filings").join("data").exists());
    assert!(!f.table_dir("filings").join("schema.json").exists());
}

/// A read holds the declaration to the table's schema exactly as a landing and a fold do:
/// an `order_by` edited to a column the table does not carry raises
/// `StoreOrderByUnknownColumn`, not a binder error from the relation it would emit.
#[test]
fn a_read_refuses_an_unknown_order_by_rather_than_emitting_it() {
    let f = Fixture::new();
    let good = decl("name = \"filings\"\nprimary_key = [\"doc\"]\norder_by = \"rev\"");
    f.land(&good, "run-1", json!([{"doc": "a", "rev": 1}]), "2030-01-01T00:00:00Z").unwrap();
    assert_eq!(f.query(&good, Bounds::default(), "SELECT doc FROM t"), [[s("a")]]);

    // The same table, read through a declaration whose ordering column it does not carry.
    let edited = decl("name = \"filings\"\nprimary_key = [\"doc\"]\norder_by = \"nope\"");
    let err = f.scan(&edited, Bounds::default()).unwrap_err();
    assert!(matches!(err.store(), Some(StoreError::StoreOrderByUnknownColumn(_))), "{err}");
}
