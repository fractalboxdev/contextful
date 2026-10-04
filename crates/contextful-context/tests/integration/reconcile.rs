//! `store.reconcile`: schema evolution across a table's files, as a scan resolves it.

use crate::support::{at, decl, s, Fixture};
#[cfg(feature = "read")]
use crate::support::query;
use contextful_context::fold::fold;
use contextful_context::parquet_io;
use contextful_context::ContextError;
use contextful_core::store::bound_time::Bounds;
use contextful_core::store::reconcile::{ColumnType, FloatItem};
use contextful_core::store::StoreError;
use serde_json::json;
use std::fs;

#[cfg(feature = "read")]
#[test]
fn one_variant_column_keeps_each_scalar_kind_through_land_fold_and_read() {
    let f = Fixture::new();
    let d = decl("name = \"attributes\"\ncolumns = { value = \"variant\" }");
    let variant = ColumnType::parse("variant").expect("variant is a declared column type");
    f.land_typed(
        &d,
        "mixed",
        json!([
            {"id": "text", "value": "violet"},
            {"id": "integer", "value": 9007199254740993_i64},
            {"id": "float", "value": 1.25},
            {"id": "bytes", "value": {"kind": "bytes", "bytes": "/wA="}}
        ]),
        "2030-01-01T00:00:00Z",
        &[("value", variant)],
    )
    .unwrap();
    let select = "SELECT id, value.kind, value.str, value.int, value.double, value.bytes FROM t ORDER BY id";
    let expected = vec![
        vec![s("bytes"), s("bytes"), None, None, None, s("/wA=")],
        vec![s("float"), s("double"), None, None, s("1.25"), None],
        vec![s("integer"), s("int"), None, s("9007199254740993"), None, None],
        vec![s("text"), s("str"), s("violet"), None, None, None],
    ];
    assert_eq!(f.query(&d, Bounds::default(), select), expected);
    fold(&f.store, &d, at("2030-01-02T00:00:00Z")).unwrap();
    assert_eq!(f.query(&d, Bounds::default(), select), expected);
}

#[test]
fn a_variant_value_with_two_non_null_kinds_refuses_before_parquet() {
    let f = Fixture::new();
    let d = decl("name = \"attributes\"\ncolumns = { value = \"variant\" }");
    let variant = ColumnType::parse("variant").expect("variant is a declared column type");
    let result = f.land_typed(
        &d,
        "conflict",
        json!([{"id": "conflict", "value": {"kind": "str", "str": "violet", "int": 3}}]),
        "2030-01-01T00:00:00Z",
        &[("value", variant)],
    );
    assert!(matches!(result, Err(ContextError::Store(StoreError::StoreSchemaIncompatible(_)))), "{result:?}");
    assert!(!f.table_dir("attributes").join("data/runs/conflict").exists());
}

#[cfg(feature = "read")]
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

#[cfg(feature = "read")]
/// Every read passes `union_by_name=true`; a column resolves to the common supertype of the files carrying it, and the generated relation casts no column but a vector, per {{store.reconcile.half-width}}.
// spec: store.reconcile.union-by-name@804d751e
#[test]
fn a_column_resolves_to_the_supertype_of_its_files_with_no_cast() {
    let f = Fixture::new();
    let d = decl("name = \"prices\"");
    let vector = [("embedding", ColumnType::FixedSizeList(FloatItem::Float16, 4))];
    f.land_typed(&d, "run-1", json!([{"sku": "a", "price": 3, "embedding": [0.5, -1, 0.25, 2]}]), "2030-01-01T00:00:00Z", &vector).unwrap();
    f.land_typed(&d, "run-2", json!([{"sku": "b", "price": 2.5, "embedding": [1, 0, -0.5, 4]}]), "2030-01-01T00:01:00Z", &vector).unwrap();
    // The files are physically mixed: one Int64, one Float64.
    let files = f.scan(&d, Bounds::default()).unwrap().files;
    let types: Vec<_> = files
        .iter()
        .map(|p| query(&format!("SELECT type FROM parquet_schema('{}') WHERE name = 'price'", f.store.root().join(p).display())))
        .collect();
    assert_eq!(types, [vec![vec![s("INT64")]], vec![vec![s("DOUBLE")]]]);

    // The relation's one cast is the vector's; the mixed scalar column resolves by union_by_name alone.
    let rel = f.scan(&d, Bounds::default()).unwrap().relation;
    assert!(rel.contains("union_by_name = true"), "{rel}");
    assert_eq!(rel.matches("CAST(").count(), 1, "{rel}");
    assert!(rel.contains("REPLACE (CAST(\"embedding\" AS FLOAT[4]) AS \"embedding\")"), "{rel}");
    assert!(!rel.contains("CAST(\"price\"") && !rel.contains("CAST(\"sku\""), "{rel}");
    let typed = query(&format!("SELECT typeof(price), typeof(embedding) FROM ({rel}) LIMIT 1"));
    assert_eq!(typed, [[s("DOUBLE"), s("FLOAT[4]")]]);
    assert_eq!(
        f.query(&d, Bounds::default(), "SELECT sku, price, CAST(embedding AS VARCHAR) FROM t ORDER BY sku"),
        [[s("a"), s("3.0"), s("[0.5, -1.0, 0.25, 2.0]")], [s("b"), s("2.5"), s("[1.0, 0.0, -0.5, 4.0]")]]
    );
}

#[cfg(feature = "read")]
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

#[cfg(feature = "read")]
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

#[cfg(feature = "read")]
/// A fold backfills nulls and widens types, a struct's fields included; it narrows no type and drops no column or field.
// spec: store.reconcile.fold-never-narrows@993fbab4
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

/// Every value of a JSON column is a JSON document: a string landing beside an object in
/// one batch, and a string column a later batch promotes to JSON, both read back encoded.
#[test]
fn every_value_of_a_json_column_is_a_json_document() {
    use arrow_array::{Array, StringArray};
    let f = Fixture::new();
    let d = decl("name = \"events\"");
    let values = |path: std::path::PathBuf| -> Vec<String> {
        let mut out = Vec::new();
        for b in parquet_io::read(&path).unwrap() {
            let c = b.column_by_name("x").unwrap().as_any().downcast_ref::<StringArray>().unwrap().clone();
            out.extend((0..c.len()).filter(|i| !c.is_null(*i)).map(|i| c.value(i).to_string()));
        }
        out
    };

    // One batch: a string and an object share the column, so it lands as JSON.
    f.land(&d, "run-1", json!([{"x": {"a": 1}}, {"x": "plain"}]), "2030-01-01T00:00:00Z").unwrap();
    let written = values(f.table_dir("events").join("data/runs/run-1/ingest-a/part-00000.parquet"));
    assert_eq!(written, [r#"{"a":1}"#, r#""plain""#]);

    // Two batches: a string column, then an object promoting it; the fold encodes the strings.
    let g = Fixture::new();
    g.land(&d, "run-1", json!([{"x": "plain"}]), "2030-01-01T00:00:00Z").unwrap();
    g.land(&d, "run-2", json!([{"x": [1, 2]}]), "2030-01-01T00:01:00Z").unwrap();
    fold(&g.store, &d, at("2030-01-01T01:00:00Z")).unwrap();
    let (chain, _) = g.store.chain("events").unwrap();
    let snapshot = g.store.snapshot_dir("events", &chain[0].snapshot_id).unwrap().join("part-00000.parquet");
    let mut folded = values(snapshot);
    folded.sort();
    assert_eq!(folded, [r#""plain""#, "[1,2]"]);
    for v in folded {
        serde_json::from_str::<serde_json::Value>(&v).unwrap_or_else(|e| panic!("`{v}` is not JSON: {e}"));
    }
}

fn typed_columns() -> [(&'static str, ColumnType); 4] {
    [
        ("blob", ColumnType::Binary),
        ("digest", ColumnType::FixedSizeBinary(4)),
        ("embedding", ColumnType::FixedSizeList(FloatItem::Float16, 3)),
        ("wide", ColumnType::FixedSizeList(FloatItem::Float32, 2)),
    ]
}

/// A producer declares these types; a JSON batch carries bytes as padded base64 and a vector as a number array of its dimension, and any other value meets {{store.reconcile.incompatible}}.
// spec: store.reconcile.typed-landing@de518808
#[test]
fn a_typed_batch_lands_bytes_from_base64_and_vectors_from_arrays() {
    let f = Fixture::new();
    let d = decl("name = \"embeddings\"\nprimary_key = [\"id\"]");
    let rows = json!([
        {"id": "a", "blob": "qgE=", "digest": "3q2+7w==", "embedding": [0.5, -1.0, 0.25], "wide": [1.5, 2]},
        {"id": "b", "blob": null, "digest": null, "embedding": null, "wide": null},
    ]);
    f.land_typed(&d, "run-1", rows, "2030-01-01T00:00:00Z", &typed_columns()).unwrap();
    let schema = f.store.schema("embeddings").unwrap();
    for (name, ty) in typed_columns() {
        assert_eq!(schema.get(name).unwrap().ty, ty, "{name}");
    }
    let read = contextful_context::rows::table_rows(&f.store, &d, &["id", "blob", "digest", "embedding", "wide"]).unwrap();
    let mut read: Vec<_> = read.into_iter().map(serde_json::Value::Object).collect();
    read.sort_by_key(|r| r["id"].to_string());
    assert_eq!(
        read,
        [
            json!({"id": "a", "blob": "qgE=", "digest": "3q2+7w==", "embedding": [0.5, -1.0, 0.25], "wide": [1.5, 2.0]}),
            json!({"id": "b", "blob": null, "digest": null, "embedding": null, "wide": null}),
        ]
    );

    // Unpadded or hex bytes, a wrong byte width, a wrong dimension and a non-number element each refuse before any Parquet.
    let refusals = [
        json!({"id": "c", "blob": "qgE"}),
        json!({"id": "c", "blob": "0xaa01"}),
        json!({"id": "c", "blob": 7}),
        json!({"id": "c", "digest": "qgE="}),
        json!({"id": "c", "embedding": [0.5, 1.0]}),
        json!({"id": "c", "embedding": [0.5, 1.0, "x"]}),
        json!({"id": "c", "embedding": [0.5, 1.0, null]}),
        json!({"id": "c", "wide": "1.5,2"}),
    ];
    for (i, row) in refusals.into_iter().enumerate() {
        let run = format!("run-bad-{i}");
        let err = f.land_typed(&d, &run, json!([row.clone()]), "2030-01-01T00:01:00Z", &typed_columns()).unwrap_err();
        assert!(matches!(err.store(), Some(StoreError::StoreSchemaIncompatible(_))), "{row}: {err}");
        assert!(!f.table_dir("embeddings").join("data/runs").join(&run).join("ingest-a/part-00000.parquet").exists(), "{row}");
    }

    // A later batch changing a width, an element type or a dimension refuses at the write.
    for (column, ty, value) in [
        ("digest", ColumnType::FixedSizeBinary(8), json!("3q2+796tvu8=")),
        ("embedding", ColumnType::FixedSizeList(FloatItem::Float32, 3), json!([0.5, 1.0, 2.0])),
        ("wide", ColumnType::FixedSizeList(FloatItem::Float32, 3), json!([0.5, 1.0, 2.0])),
    ] {
        let err = f.land_typed(&d, "run-2", json!([{"id": "d", column: value}]), "2030-01-01T00:02:00Z", &[(column, ty)]).unwrap_err();
        assert!(matches!(err.store(), Some(StoreError::StoreSchemaIncompatible(_))), "{column}: {err}");
    }
}

#[cfg(feature = "read")]
/// A column `schema.json` holds as a binary, vector or nested type lands a later undeclared JSON value in that type, so a run after the first needs no declaration.
// spec: store.reconcile.stored-type@42866563
#[test]
fn a_stored_binary_or_vector_column_types_a_later_undeclared_batch() {
    let f = Fixture::new();
    let d = decl("name = \"embeddings\"");
    let rows = json!([{"id": "a", "blob": "qgE=", "digest": "3q2+7w==", "embedding": [0.5, -1.0, 0.25], "wide": [1.5, 2]}]);
    f.land_typed(&d, "run-1", rows, "2030-01-01T00:00:00Z", &typed_columns()).unwrap();
    // The later batch declares nothing: base64 text and number arrays land in the stored types.
    let later = json!([{"id": "b", "blob": "/w==", "digest": "AAECAw==", "embedding": [1, 0, 0], "wide": [0.5, 0.5]}]);
    f.land(&d, "run-2", later, "2030-01-01T00:01:00Z").unwrap();
    let schema = f.store.schema("embeddings").unwrap();
    for (name, ty) in typed_columns() {
        assert_eq!(schema.get(name).unwrap().ty, ty, "{name}");
    }
    assert_eq!(
        f.query(&d, Bounds::default(), "SELECT id, typeof(embedding), CAST(embedding AS VARCHAR), octet_length(digest) FROM t ORDER BY id"),
        [[s("a"), s("FLOAT[3]"), s("[0.5, -1.0, 0.25]"), s("4")], [s("b"), s("FLOAT[3]"), s("[1.0, 0.0, 0.0]"), s("4")]]
    );
    // A value the stored type does not read still refuses before any Parquet.
    for (i, row) in [json!({"id": "c", "digest": "3q2+796tvu8="}), json!({"id": "c", "embedding": [1, 2]}), json!({"id": "c", "blob": "not base64"})].into_iter().enumerate() {
        let run = format!("run-bad-{i}");
        let err = f.land(&d, &run, json!([row.clone()]), "2030-01-01T00:02:00Z").unwrap_err();
        assert!(matches!(err.store(), Some(StoreError::StoreSchemaIncompatible(_))), "{row}: {err}");
        assert!(!f.table_dir("embeddings").join("data/runs").join(&run).join("ingest-a/part-00000.parquet").exists(), "{row}");
    }
}

/// `columns` maps a column to a type spelled as {{store.reconcile.typed-landing}} reads it; every landing into the table, a pipeline run included, lands that column in the declared type.
// spec: store.declare.column-types@bf4b3417
#[test]
fn a_declared_column_type_types_a_landing_that_declares_none() {
    let f = Fixture::new();
    let d = decl("name = \"embeddings\"\ncolumns = { digest = \"binary(4)\", embedding = \"float16[3]\", blob = \"binary\" }");
    let rows = json!([{"id": "a", "blob": "qgE=", "digest": "3q2+7w==", "embedding": [0.5, -1.0, 0.25]}]);
    f.land(&d, "run-1", rows, "2030-01-01T00:00:00Z").unwrap();
    let schema = f.store.schema("embeddings").unwrap();
    assert_eq!(schema.get("digest").unwrap().ty, ColumnType::FixedSizeBinary(4));
    assert_eq!(schema.get("embedding").unwrap().ty, ColumnType::FixedSizeList(FloatItem::Float16, 3));
    assert_eq!(schema.get("blob").unwrap().ty, ColumnType::Binary);
    // A producer type overrides nothing it agrees with, and a disagreeing one meets the stored type.
    let err = f
        .land_typed(&d, "run-2", json!([{"id": "b", "digest": "3q2+796tvu8="}]), "2030-01-01T00:01:00Z", &[("digest", ColumnType::FixedSizeBinary(8))])
        .unwrap_err();
    assert!(matches!(err.store(), Some(StoreError::StoreSchemaIncompatible(_))), "{err}");
}

#[cfg(feature = "read")]
/// A `Float16` vector stores each element at half width in Parquet. The engine reads its elements as `FLOAT`, either binary type as `BLOB`, and a vector as an `ARRAY` of its dimension through one relation cast.
// spec: store.reconcile.half-width@b93ac02d
#[test]
fn half_floats_store_at_half_width_and_read_as_float_arrays() {
    let f = Fixture::new();
    let d = decl("name = \"embeddings\"");
    let rows = json!([{"id": "a", "blob": "qgE=", "digest": "3q2+7w==", "embedding": [0.5, -1.0, 0.25], "wide": [1.5, 2]}]);
    f.land_typed(&d, "run-1", rows.clone(), "2030-01-01T00:00:00Z", &typed_columns()).unwrap();
    f.land_typed(&d, "run-2", rows, "2030-01-01T00:01:00Z", &typed_columns()).unwrap();

    let part = f.table_dir("embeddings").join("data/runs/run-1/ingest-a/part-00000.parquet");
    let physical = |name: &str| {
        query(&format!(
            "SELECT type, type_length, logical_type FROM parquet_schema('{}') WHERE name = '{name}'",
            part.display()
        ))
    };
    // The half-float element is a 2-byte fixed-length value carrying the Float16 logical type.
    let element = query(&format!(
        "SELECT type, type_length, logical_type FROM parquet_schema('{}') WHERE name = 'item' AND type = 'FIXED_LEN_BYTE_ARRAY'",
        part.display()
    ));
    assert_eq!(element.len(), 1, "{element:?}");
    assert_eq!(element[0][1], s("2"));
    assert!(element[0][2].as_deref().unwrap_or_default().contains("Float16"), "{element:?}");
    assert_eq!(physical("digest")[0][..2], [s("FIXED_LEN_BYTE_ARRAY"), s("4")]);
    assert_eq!(physical("blob")[0][0], s("BYTE_ARRAY"));

    // The engine reads FLOAT arrays of the declared dimension and BLOBs; the vector cast is the relation's one cast.
    let rel = f.scan(&d, Bounds::default()).unwrap().relation;
    assert_eq!(rel.matches("CAST(").count(), 2, "{rel}");
    assert!(rel.contains("REPLACE (CAST(\"embedding\" AS FLOAT[3]) AS \"embedding\", CAST(\"wide\" AS FLOAT[2]) AS \"wide\")"), "{rel}");
    let types = f.query(&d, Bounds::default(), "SELECT typeof(embedding), typeof(wide), typeof(blob), typeof(digest) FROM t LIMIT 1");
    assert_eq!(types, [[s("FLOAT[3]"), s("FLOAT[2]"), s("BLOB"), s("BLOB")]]);
    let values = f.query(&d, Bounds::default(), "SELECT CAST(embedding AS VARCHAR), array_cosine_similarity(embedding, embedding) FROM t LIMIT 1");
    assert_eq!(values, [[s("[0.5, -1.0, 0.25]"), s("1.0")]]);

    // A fold keeps both types, and a zero-row branch casts to the same engine types.
    fold(&f.store, &d, at("2030-01-01T01:00:00Z")).unwrap();
    let types = f.query(&d, Bounds::default(), "SELECT typeof(embedding), typeof(digest), count(*) FROM t GROUP BY ALL");
    assert_eq!(types, [[s("FLOAT[3]"), s("BLOB"), s("2")]]);
    // Bounded to before any run, the table is a zero-row relation over the same engine types.
    let bound = Bounds { as_of: Some(contextful_core::store::bound_time::Bound::parse("2029-12-31T00:00:00Z").unwrap()), valid_as_of: None };
    let rel = f.scan(&d, bound).unwrap().relation;
    assert!(rel.contains("CAST(NULL AS FLOAT[3]) AS \"embedding\"") && rel.contains("CAST(NULL AS BLOB) AS \"digest\""), "{rel}");
}

fn events() -> ColumnType {
    ColumnType::list(ColumnType::structure([
        ("name", ColumnType::Utf8),
        ("time", ColumnType::Timestamp),
        ("attrs", ColumnType::map(ColumnType::Utf8)),
    ]))
}

/// A producer or declaration types a column `struct<…>`, `list<…>` or `map<utf8, …>`; a JSON batch carries a struct and a map as an object and a list as an array, a struct key naming no field meeting {{store.reconcile.incompatible}}.
// spec: store.reconcile.nested-landing@d9d6ef37
#[test]
fn a_typed_batch_lands_nested_values_and_refuses_a_kind_change_by_path() {
    let f = Fixture::new();
    let d = decl("name = \"spans\"");
    let rows = json!([
        {"id": "a", "events": [{"name": "start", "time": "2030-01-01T00:00:00Z", "attrs": {"k": "v"}}], "tags": ["x", "y"]},
        {"id": "b", "events": null, "tags": []},
    ]);
    let types = [
        ("events", events()),
        ("tags", ColumnType::list(ColumnType::Utf8)),
    ];
    f.land_typed(&d, "run-1", rows, "2030-01-01T00:00:00Z", &types)
        .unwrap();
    let schema = f.store.schema("spans").unwrap();
    assert_eq!(schema.get("events").unwrap().ty, events());
    let read =
        contextful_context::rows::table_rows(&f.store, &d, &["id", "events", "tags"]).unwrap();
    let mut read: Vec<_> = read.into_iter().map(serde_json::Value::Object).collect();
    read.sort_by_key(|r| r["id"].to_string());
    assert_eq!(
        read,
        [
            json!({"id": "a", "events": [{"name": "start", "time": "2030-01-01T00:00:00.000000000Z", "attrs": {"k": "v"}}], "tags": ["x", "y"]}),
            json!({"id": "b", "events": null, "tags": []}),
        ]
    );

    // A value outside the declared shape refuses before any Parquet: a scalar for a list, a
    // key no field names, a list for a map, a mistyped item.
    for (i, row) in [
        json!({"id": "c", "tags": "x"}),
        json!({"id": "c", "events": [{"name": "n", "colour": "red"}]}),
        json!({"id": "c", "events": [{"attrs": ["k"]}]}),
        json!({"id": "c", "events": [{"attrs": {"k": 1}}]}),
        json!({"id": "c", "events": [{"time": "yesterday"}]}),
        json!({"id": "c", "tags": [1]}),
    ]
    .into_iter()
    .enumerate()
    {
        let run = format!("run-bad-{i}");
        let err = f
            .land_typed(
                &d,
                &run,
                json!([row.clone()]),
                "2030-01-01T00:01:00Z",
                &types,
            )
            .unwrap_err();
        assert!(
            matches!(err.store(), Some(StoreError::StoreSchemaIncompatible(_))),
            "{row}: {err}"
        );
        assert!(
            !f.table_dir("spans")
                .join("data/runs")
                .join(&run)
                .join("ingest-a/part-00000.parquet")
                .exists(),
            "{row}"
        );
    }

    // `List<Int64>` landing on the `List<Utf8>` column refuses at the write, naming the path.
    let err = f
        .land_typed(
            &d,
            "run-2",
            json!([{"id": "d", "tags": [1, 2]}]),
            "2030-01-01T00:02:00Z",
            &[("tags", ColumnType::list(ColumnType::Int64))],
        )
        .unwrap_err();
    match err.store() {
        Some(StoreError::StoreSchemaIncompatible(m)) => assert!(
            m.contains("`tags[]`") && m.contains("List<Utf8>") && m.contains("List<Int64>"),
            "{m}"
        ),
        other => panic!("expected StoreSchemaIncompatible, got {other:?}"),
    }
    assert!(!f.table_dir("spans").join("data/runs/run-2").exists());
}

/// A column `schema.json` holds as a binary, vector or nested type lands a later undeclared JSON value in that type, so a run after the first needs no declaration.
#[test]
fn a_stored_nested_column_types_a_later_undeclared_batch_and_a_declaration_spells_one() {
    let f = Fixture::new();
    let d = decl("name = \"spans\"\ncolumns = { events = \"list<struct<name: utf8, time: timestamp, attrs: map<utf8, utf8>>>\" }");
    f.land(
        &d,
        "run-1",
        json!([{"id": "a", "events": [{"name": "start"}]}]),
        "2030-01-01T00:00:00Z",
    )
    .unwrap();
    assert_eq!(
        f.store.schema("spans").unwrap().get("events").unwrap().ty,
        events()
    );
    // The undeclared table reads the stored type for a later batch.
    let plain = decl("name = \"spans\"");
    f.land(
        &plain,
        "run-2",
        json!([{"id": "b", "events": [{"attrs": {"k": "v"}}]}]),
        "2030-01-01T00:01:00Z",
    )
    .unwrap();
    assert_eq!(
        f.store.schema("spans").unwrap().get("events").unwrap().ty,
        events()
    );
    // An undeclared column of objects in a table that never typed it lands as JSON text.
    f.land(
        &plain,
        "run-3",
        json!([{"id": "c", "payload": {"k": [1]}}]),
        "2030-01-01T00:02:00Z",
    )
    .unwrap();
    assert_eq!(
        f.store.schema("spans").unwrap().get("payload").unwrap().ty,
        ColumnType::Json
    );
}
