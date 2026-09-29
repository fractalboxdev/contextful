//! `store.reconcile`: the type lattice and the merged schema.

use contextful_core::store::reconcile::{decode_binary, encode_binary, supertype, Column, ColumnType, FloatItem, Schema};
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

/// The lattice holds `Binary`, `FixedSizeBinary(n)` and a `FixedSizeList` of `Float32` or `Float16` at dimension n; none takes a promotion, so a width, item or dimension change meets {{store.reconcile.incompatible}}.
// spec: store.reconcile.binary-and-vector@bf9db2d8
#[test]
fn binary_and_vector_types_take_no_promotion() {
    let typed = [Binary, FixedSizeBinary(16), FixedSizeBinary(32), FixedSizeList(FloatItem::Float32, 4), FixedSizeList(FloatItem::Float16, 4), FixedSizeList(FloatItem::Float32, 8)];
    let scalar = [Boolean, Int32, Int64, Float64, Utf8, Json, Timestamp];
    for a in typed {
        assert_eq!(supertype(a, a), Some(a));
        assert_eq!(supertype(Null, a), Some(a));
        assert_eq!(supertype(a, Null), Some(a));
        for b in typed.into_iter().chain(scalar).filter(|b| *b != a) {
            assert_eq!(supertype(a, b), None, "{a:?} with {b:?}");
            assert_eq!(supertype(b, a), None, "{b:?} with {a:?}");
        }
    }
    let cases = [
        (FixedSizeBinary(16), FixedSizeBinary(32), "FixedSizeBinary(16)", "FixedSizeBinary(32)"),
        (FixedSizeList(FloatItem::Float32, 4), FixedSizeList(FloatItem::Float16, 4), "FixedSizeList<Float32, 4>", "FixedSizeList<Float16, 4>"),
        (FixedSizeList(FloatItem::Float16, 4), FixedSizeList(FloatItem::Float16, 8), "FixedSizeList<Float16, 4>", "FixedSizeList<Float16, 8>"),
        (Binary, Utf8, "Binary", "Utf8"),
        (FixedSizeList(FloatItem::Float32, 4), Json, "FixedSizeList<Float32, 4>", "Json"),
    ];
    for (stored, arriving, stored_name, arriving_name) in cases {
        match s(&[("v", stored)]).merge(&s(&[("v", arriving)]), &[]) {
            Err(StoreError::StoreSchemaIncompatible(m)) => {
                assert!(m.contains("`v`") && m.contains(stored_name) && m.contains(arriving_name), "{m}")
            }
            other => panic!("{stored:?} with {arriving:?}: expected StoreSchemaIncompatible, got {other:?}"),
        }
    }
}

/// A producer spells binary and vector types; padded base64 decodes and other spellings carry no bytes.
#[test]
fn a_producer_spells_binary_and_vector_types() {
    assert_eq!(ColumnType::parse("binary"), Some(Binary));
    assert_eq!(ColumnType::parse("bytes"), Some(Binary));
    assert_eq!(ColumnType::parse("binary(32)"), Some(FixedSizeBinary(32)));
    assert_eq!(ColumnType::parse("float32[768]"), Some(FixedSizeList(FloatItem::Float32, 768)));
    assert_eq!(ColumnType::parse("Float16[384]"), Some(FixedSizeList(FloatItem::Float16, 384)));
    for bad in ["binary(0)", "binary()", "float16[0]", "float64[4]", "float32[x]", "binary(4294967295)"] {
        assert_eq!(ColumnType::parse(bad), None, "{bad}");
    }
    assert_eq!(encode_binary(&[0xAA, 0x01]), "qgE=");
    assert_eq!(decode_binary("qgE="), Some(vec![0xAA, 0x01]));
    // Unpadded, hex and URL-safe spellings carry no bytes.
    for bad in ["qgE", "0xaa01", "-_8="] {
        assert_eq!(decode_binary(bad), None, "{bad}");
    }
}

/// The Arrow JSON form carries a binary width, and a vector's dimension and element width, so `schema.json` round-trips them.
#[test]
fn the_arrow_json_form_round_trips_binary_and_vector_types() {
    let typed = Schema {
        columns: vec![
            Column::new("blob", Binary, true),
            Column::new("digest", FixedSizeBinary(32), false),
            Column::new("embedding", FixedSizeList(FloatItem::Float16, 384), true),
            Column::new("wide", FixedSizeList(FloatItem::Float32, 3), true),
        ],
    };
    let doc = typed.to_arrow_json();
    assert_eq!(doc["fields"][0]["type"], serde_json::json!({"name": "binary"}));
    assert_eq!(doc["fields"][1]["type"], serde_json::json!({"name": "fixedsizebinary", "byteWidth": 32}));
    assert_eq!(doc["fields"][2]["type"], serde_json::json!({"name": "fixedsizelist", "listSize": 384}));
    assert_eq!(doc["fields"][2]["children"][0]["type"], serde_json::json!({"name": "floatingpoint", "precision": "HALF"}));
    assert_eq!(doc["fields"][3]["children"][0]["type"], serde_json::json!({"name": "floatingpoint", "precision": "SINGLE"}));
    assert_eq!(Schema::from_arrow_json(&doc).unwrap(), typed);
    // The engine reads either binary width as BLOB and either vector width as FLOAT.
    assert_eq!(FixedSizeBinary(32).sql(), "BLOB");
    assert_eq!(Binary.sql(), "BLOB");
    assert_eq!(FixedSizeList(FloatItem::Float16, 384).sql(), "FLOAT[384]");
}
