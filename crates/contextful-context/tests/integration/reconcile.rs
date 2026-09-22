//! `store.reconcile`: schema evolution across a table's files, as a scan resolves it.

use crate::support::{at, decl, query, s, Fixture};
use contextful_context::fold::fold;
use contextful_context::parquet_io;
use contextful_core::store::bound_time::Bounds;
use contextful_core::store::reconcile::ColumnType;
use contextful_core::store::StoreError;
use serde_json::json;
use std::fs;

/// Every read hands `read_parquet` an explicit sorted file list resolved from the pointer and the manifests, never a glob; a stray file joins nothing.
// spec: store.reconcile.explicit-file-list@74dc3f17
#[test]
fn a_read_hands_read_parquet_an_explicit_sorted_list() {
    let f = Fixture::new();
    let d = decl("name = \"filings\"");
    f.land(&d, "run-2", json!([{"id": "b"}]), "2030-01-01T00:01:00Z").unwrap();
    f.land(&d, "run-1", json!([{"id": "a"}]), "2030-01-01T00:00:00Z").unwrap();
    // A stray Parquet file in the tree, readable and well-formed, joins nothing.
    let src = f.table_dir("filings").join("data/runs/run-1/ingest-a/part-00000.parquet");
    fs::copy(&src, f.table_dir("filings").join("data/runs/run-1/ingest-a/part-00099.parquet")).unwrap();
    fs::copy(&src, f.table_dir("filings").join("data/stray.parquet")).unwrap();
    let scan = f.scan(&d, Bounds::default()).unwrap();
    assert_eq!(
        scan.files,
        ["tables/filings/data/runs/run-1/ingest-a/part-00000.parquet", "tables/filings/data/runs/run-2/ingest-a/part-00000.parquet"]
    );
    assert!(!scan.relation.contains('*') || !scan.relation.contains("*.parquet"), "{}", scan.relation);
    assert!(!scan.relation.contains("**"));
    assert_eq!(f.query(&d, Bounds::default(), "SELECT count(*) FROM t"), [[s("2")]]);
}

/// Every read passes `union_by_name=true`; a column resolves to the common supertype of the files carrying it, and the generated relation adds no per-column cast.
// spec: store.reconcile.union-by-name@5daa36e1
#[test]
fn a_column_resolves_to_the_supertype_of_its_files_with_no_cast() {
    let f = Fixture::new();
    let d = decl("name = \"prices\"");
    f.land(&d, "run-1", json!([{"sku": "a", "price": 3}]), "2030-01-01T00:00:00Z").unwrap();
    f.land(&d, "run-2", json!([{"sku": "b", "price": 2.5}]), "2030-01-01T00:01:00Z").unwrap();
    // The files are physically mixed: one Int64, one Float64.
    let files = f.scan(&d, Bounds::default()).unwrap().files;
    let types: Vec<_> = files
        .iter()
        .map(|p| query(&format!("SELECT type FROM parquet_schema('{}') WHERE name = 'price'", f.store.root().join(p).display())))
        .collect();
    assert_eq!(types, [vec![vec![s("INT64")]], vec![vec![s("DOUBLE")]]]);

    let rel = f.scan(&d, Bounds::default()).unwrap().relation;
    assert!(rel.contains("union_by_name = true"), "{rel}");
    assert!(!rel.contains("CAST("), "{rel}");
    let typed = query(&format!("SELECT typeof(price) FROM ({rel}) LIMIT 1"));
    assert_eq!(typed, [[s("DOUBLE")]]);
    assert_eq!(f.query(&d, Bounds::default(), "SELECT sku, price FROM t ORDER BY sku"), [[s("a"), s("3.0")], [s("b"), s("2.5")]]);
}

/// An `Int64` value above 9007199254740992 loses precision once a `Float64` batch lands on its column.
// spec: store.reconcile.float-loss@126210d3
#[test]
fn an_integer_beyond_two_to_the_53_loses_precision_once_the_column_widens() {
    let f = Fixture::new();
    let d = decl("name = \"counters\"");
    f.land(&d, "run-1", json!([{"k": "a", "n": 9007199254740993_i64}]), "2030-01-01T00:00:00Z").unwrap();
    assert_eq!(f.query(&d, Bounds::default(), "SELECT n FROM t"), [[s("9007199254740993")]]);
    f.land(&d, "run-2", json!([{"k": "b", "n": 0.5}]), "2030-01-01T00:01:00Z").unwrap();
    let widened = f.query(&d, Bounds::default(), "SELECT CAST(n AS HUGEINT) FROM t WHERE k = 'a'");
    assert_eq!(widened, [[s("9007199254740992")]]);
}

/// Any other pair of types observed for one column raises `StoreSchemaIncompatible` at the write, naming the column, the stored type and the arriving type.
#[test]
fn an_incompatible_batch_refuses_before_any_parquet() {
    let f = Fixture::new();
    let d = decl("name = \"filings\"");
    f.land(&d, "run-1", json!([{"rev": 1}]), "2030-01-01T00:00:00Z").unwrap();
    let err = f.land(&d, "run-2", json!([{"rev": "yesterday"}]), "2030-01-01T00:01:00Z").unwrap_err();
    assert!(matches!(err.store(), Some(StoreError::StoreSchemaIncompatible(_))), "{err}");
    assert!(!f.table_dir("filings").join("data/runs/run-2").exists());
    // A type clash inside one batch refuses the same way.
    let err = f.land(&d, "run-3", json!([{"rev": 1}, {"rev": true}]), "2030-01-01T00:01:00Z").unwrap_err();
    assert!(matches!(err.store(), Some(StoreError::StoreSchemaIncompatible(_))), "{err}");
}

/// A primary-key column takes no `Float64` promotion: the widening batch raises `StoreKeyWidened` before any Parquet, as does a fold meeting a key already reconciled to `Float64`.
#[test]
fn a_widening_key_refuses_at_the_write_and_at_the_fold() {
    let f = Fixture::new();
    let keyed = decl("name = \"accounts\"\nprimary_key = [\"id\"]");
    f.land(&keyed, "run-1", json!([{"id": 1}]), "2030-01-01T00:00:00Z").unwrap();
    let err = f.land(&keyed, "run-2", json!([{"id": 1.5}]), "2030-01-01T00:01:00Z").unwrap_err();
    assert!(matches!(err.store(), Some(StoreError::StoreKeyWidened(_))), "{err}");
    assert!(!f.table_dir("accounts").join("data/runs/run-2").exists());

    // The column widened while the table was unkeyed; declaring the key then stops the fold.
    let g = Fixture::new();
    let unkeyed = decl("name = \"accounts\"");
    g.land(&unkeyed, "run-1", json!([{"id": 1}]), "2030-01-01T00:00:00Z").unwrap();
    g.land(&unkeyed, "run-2", json!([{"id": 1.5}]), "2030-01-01T00:01:00Z").unwrap();
    let err = fold(&g.store, &keyed, at("2030-01-01T01:00:00Z")).unwrap_err();
    assert!(matches!(err.store(), Some(StoreError::StoreKeyWidened(_))), "{err}");
    assert!(g.store.pointer("accounts").unwrap().is_none());
}

/// A scan invents no column: an ordering column absent from every file enters through a zero-row branch, and a column added after a table's first rows projects as a literal null.
// spec: store.reconcile.no-invented-column@8176f0e9
#[test]
fn a_scan_invents_no_column() {
    let f = Fixture::new();
    let d = decl("name = \"filings\"\nprimary_key = [\"doc\"]\norder_by = \"rev\"");
    f.land(&d, "run-1", json!([{"doc": "a", "rev": 1}]), "2030-01-01T00:00:00Z").unwrap();
    f.land(&d, "run-2", json!([{"doc": "b", "rev": 1, "pages": 3}]), "2030-01-01T00:01:00Z").unwrap();
    // A column added after the first rows reads as null for the older file.
    assert_eq!(f.query(&d, Bounds::default(), "SELECT doc, pages FROM t ORDER BY doc"), [[s("a"), None], [s("b"), s("3")]]);

    // Bounded to before the column existed, no listed file carries it: it enters through a zero-row branch.
    let bound = Bounds { as_of: Some(contextful_core::store::bound_time::Bound::parse("2030-01-01T00:00:30Z").unwrap()), valid_as_of: None };
    let rel = f.scan(&d, bound).unwrap().relation;
    assert!(rel.contains("UNION ALL BY NAME SELECT CAST(NULL AS BIGINT) AS \"pages\" WHERE false"), "{rel}");
    assert_eq!(f.query(&d, bound, "SELECT doc, pages FROM t"), [[s("a"), None]]);
}

/// A destination creates a table on the first schema it sees and merges each later schema into `schema.json`.
// spec: store.reconcile.first-sight@e4ab9639
#[test]
fn the_first_batch_creates_the_table() {
    let f = Fixture::new();
    let d = decl("name = \"research/filings\"");
    assert!(f.store.try_schema("research/filings").unwrap().is_none());
    f.land(&d, "run-1", json!([{"id": "a"}]), "2030-01-01T00:00:00Z").unwrap();
    let schema = f.store.schema("research/filings").unwrap();
    assert_eq!(schema.get("id").unwrap().ty, ColumnType::Utf8);
    f.land(&d, "run-2", json!([{"id": "b", "meta": {"k": 1}}]), "2030-01-01T00:01:00Z").unwrap();
    assert_eq!(f.store.schema("research/filings").unwrap().get("meta").unwrap().ty, ColumnType::Json);
    assert_eq!(f.store.tables().unwrap(), ["research/filings"]);
}

/// A fold backfills nulls and widens types; it narrows no type and drops no column.
// spec: store.reconcile.fold-never-narrows@856e48d3
#[test]
fn a_fold_backfills_and_widens() {
    let f = Fixture::new();
    let d = decl("name = \"prices\"");
    f.land(&d, "run-1", json!([{"sku": "a", "price": 3}]), "2030-01-01T00:00:00Z").unwrap();
    f.land(&d, "run-2", json!([{"sku": "b", "price": 2.5, "note": "sale"}]), "2030-01-01T00:01:00Z").unwrap();
    fold(&f.store, &d, at("2030-01-01T01:00:00Z")).unwrap();
    let files = f.scan(&d, Bounds::default()).unwrap().files;
    assert_eq!(files.len(), 1);
    let path = f.store.root().join(&files[0]);
    let cols = parquet_io::columns(&path).unwrap();
    assert!(cols.contains(&"note".to_string()) && cols.contains(&"price".to_string()), "{cols:?}");
    let price = query(&format!("SELECT type FROM parquet_schema('{}') WHERE name = 'price'", path.display()));
    assert_eq!(price, [[s("DOUBLE")]]);
    assert_eq!(f.query(&d, Bounds::default(), "SELECT sku, price, note FROM t ORDER BY sku"), [[s("a"), s("3.0"), None], [s("b"), s("2.5"), s("sale")]]);
}

/// Concurrent landings into one table each merge into `schema.json`; neither loses the other's column.
#[test]
fn concurrent_landings_keep_every_column() {
    let f = std::sync::Arc::new(Fixture::new());
    let d = decl("name = \"events\"");
    f.land(&d, "run-0", json!([{"id": "x"}]), "2030-01-01T00:00:00Z").unwrap();
    for i in 0..8 {
        let handles: Vec<_> = ["left", "right"]
            .into_iter()
            .map(|side| {
                let (f, d) = (f.clone(), d.clone());
                std::thread::spawn(move || {
                    let col = format!("{side}_{i}");
                    f.land(&d, &format!("run-{side}-{i}"), json!([{"id": "x", col: 1}]), "2030-01-01T00:01:00Z").unwrap();
                })
            })
            .collect();
        handles.into_iter().for_each(|h| h.join().unwrap());
        let schema = f.store.schema("events").unwrap();
        for side in ["left", "right"] {
            assert!(schema.get(&format!("{side}_{i}")).is_some(), "round {i}: `{side}_{i}` lost from schema.json");
        }
    }
}
