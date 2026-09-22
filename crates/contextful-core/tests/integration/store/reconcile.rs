//! `store.reconcile`: the type lattice and the merged schema.

use contextful_core::store::reconcile::{supertype, Column, ColumnType, Schema};
use contextful_core::store::StoreError;
use ColumnType::*;

fn s(cols: &[(&str, ColumnType)]) -> Schema {
    Schema { columns: cols.iter().map(|(n, t)| Column::new(*n, *t, true)).collect() }
}

/// The type lattice holds one promotion, `Int64` with `Float64` to `Float64`; a JSON type absorbs `Utf8`.
// spec: store.reconcile.lattice@5f16fb55
#[test]
fn the_lattice_holds_one_promotion_and_json_absorbs_text() {
    assert_eq!(supertype(Int64, Float64), Some(Float64));
    assert_eq!(supertype(Float64, Int64), Some(Float64));
    assert_eq!(supertype(Json, Utf8), Some(Json));
    assert_eq!(supertype(Utf8, Json), Some(Json));
    for t in [Boolean, Int32, Int64, Float64, Utf8, Json, Timestamp] {
        assert_eq!(supertype(t, t), Some(t));
        assert_eq!(supertype(Null, t), Some(t));
    }
    let promoted = s(&[("n", Int64)]).merge(&s(&[("n", Float64)]), &[]).unwrap();
    assert_eq!(promoted.get("n").unwrap().ty, Float64);
}

/// Any other pair of types observed for one column raises `StoreSchemaIncompatible` at the write, naming the column, the stored type and the arriving type.
// spec: store.reconcile.incompatible@f1e8fc0a
#[test]
fn any_other_pair_is_incompatible() {
    let all = [Boolean, Int32, Int64, Float64, Utf8, Json, Timestamp];
    for a in all {
        for b in all {
            let allowed = a == b || matches!((a, b), (Int64, Float64) | (Float64, Int64) | (Json, Utf8) | (Utf8, Json));
            assert_eq!(supertype(a, b).is_some(), allowed, "{a:?} with {b:?}");
        }
    }
    match s(&[("revised_at", Int64)]).merge(&s(&[("revised_at", Utf8)]), &[]) {
        Err(StoreError::StoreSchemaIncompatible(m)) => {
            assert!(m.contains("revised_at") && m.contains("Int64") && m.contains("Utf8"), "{m}")
        }
        other => panic!("expected StoreSchemaIncompatible, got {other:?}"),
    }
}

/// A primary-key column takes no `Float64` promotion: the widening batch raises `StoreKeyWidened` before any Parquet, as does a fold meeting a key already reconciled to `Float64`.
// spec: store.reconcile.key-widening@216c675b
#[test]
fn a_key_column_takes_no_float_promotion() {
    let key = ["id".to_string()];
    match s(&[("id", Int64)]).merge(&s(&[("id", Float64)]), &key) {
        Err(StoreError::StoreKeyWidened(m)) => assert!(m.contains("id"), "{m}"),
        other => panic!("expected StoreKeyWidened, got {other:?}"),
    }
    match s(&[("id", Float64)]).merge(&s(&[("id", Int64)]), &key) {
        Err(StoreError::StoreKeyWidened(_)) => {}
        other => panic!("expected StoreKeyWidened, got {other:?}"),
    }
    // A non-key column takes the promotion.
    s(&[("id", Int64), ("n", Int64)]).merge(&s(&[("n", Float64)]), &key).unwrap();
}

/// An unseen column joins the merged schema, and files written before it read it as null.
// spec: store.reconcile.additive@6b1c6570
#[test]
fn an_unseen_column_joins_nullable() {
    let stored = Schema { columns: vec![Column::new("id", Utf8, false)] };
    let merged = stored.merge(&Schema { columns: vec![Column::new("pages", Int64, false)] }, &[]).unwrap();
    assert_eq!(merged.columns.iter().map(|c| c.name.as_str()).collect::<Vec<_>>(), ["id", "pages"]);
    assert!(merged.get("pages").unwrap().nullable);
}

/// Reconciliation keeps the current shape alone; a past widening is attributed from the run files.
// spec: store.reconcile.no-history@e477f7b7
#[test]
fn the_schema_document_keeps_the_current_shape_alone() {
    let merged = s(&[("n", Int64)]).merge(&s(&[("n", Float64)]), &[]).unwrap();
    let doc = merged.to_arrow_json();
    assert_eq!(doc, serde_json::json!({"fields": [
        {"name": "n", "nullable": true, "type": {"name": "floatingpoint", "precision": "DOUBLE"}, "children": []}
    ]}));
    assert_eq!(Schema::from_arrow_json(&doc).unwrap(), merged);
}

/// A table's schema is `schema.json` in Arrow JSON form; each arriving batch's schema merges into it and the merged result replaces it.
#[test]
fn the_arrow_json_form_round_trips_every_type() {
    let all = s(&[("b", Boolean), ("i", Int32), ("l", Int64), ("f", Float64), ("u", Utf8), ("j", Json), ("t", Timestamp)]);
    let doc = all.to_arrow_json();
    assert_eq!(doc["fields"][6]["type"], serde_json::json!({"name": "timestamp", "unit": "NANOSECOND", "timezone": "UTC"}));
    assert_eq!(doc["fields"][5]["metadata"][0]["value"], "arrow.json");
    assert_eq!(Schema::from_arrow_json(&doc).unwrap(), all);
}
