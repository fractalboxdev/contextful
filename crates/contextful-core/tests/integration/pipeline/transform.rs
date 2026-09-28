//! `run.transform`: the chain over a batch.

use contextful_core::pipeline::transform::{apply, checked, Chain, TransformOp};
use contextful_core::run::ports::{Row, Shape};
use contextful_core::run::RunError;
use serde_json::{json, Value};

fn rows(v: Value) -> Vec<Row> {
    v.as_array().unwrap().iter().map(|r| r.as_object().unwrap().clone()).collect()
}

fn ops(v: Value) -> Vec<TransformOp> {
    serde_json::from_value(v).unwrap()
}

/// The chain is an ordered list of `select`, `rename`, `cast` and a single-column `filter`, declared once per
/// pipeline and bound to the root table.
// spec: run.transform.chain@c3be3181
#[test]
fn the_chain_runs_its_four_operations_in_order() {
    let chain = ops(json!([
        {"op": "filter", "column": "status", "equals": "open"},
        {"op": "cast", "column": "total", "to": "float64"},
        {"op": "rename", "from": "total", "to": "amount"},
        {"op": "select", "columns": ["id", "amount"]}
    ]));
    let batch = rows(json!([{"id": 1, "status": "open", "total": "9.5"}, {"id": 2, "status": "closed", "total": "1"}]));
    assert_eq!(apply(&chain, batch.clone(), "orders_items").unwrap(), rows(json!([{"id": 1, "amount": 9.5}])));
    // Order matters: renaming before the cast leaves the cast nothing to name.
    let reordered = vec![chain[2].clone(), chain[1].clone()];
    assert!(matches!(apply(&reordered, batch.clone(), "orders_items"), Err(RunError::PipelineTransformColumnMissing(_))));
    // Bound to the root table as the stage between the journal and the land path.
    let stage = Chain { ops: chain, table: "orders_items".into() };
    assert_eq!(stage.shape(batch).unwrap().len(), 1);
    assert!(serde_json::from_value::<Vec<TransformOp>>(json!([{"op": "explode", "column": "x"}])).is_err(), "a fifth operation does not parse");
}

/// A chain operation emitting more rows than it consumed raises `PipelineTransformArity`.
// spec: run.transform.arity@955774b7
#[test]
fn an_operation_emitting_more_rows_than_it_consumed_is_refused() {
    let op = TransformOp::Select { columns: vec!["id".into()] };
    let doubling = |_: &TransformOp, rs: Vec<Row>, _: &str| Ok(rs.iter().chain(rs.iter()).cloned().collect());
    match checked(&op, rows(json!([{"id": 1}])), "orders_items", doubling) {
        Err(RunError::PipelineTransformArity(m)) => assert!(m.contains("2 rows from 1") && m.contains("orders_items"), "{m}"),
        other => panic!("{other:?}"),
    }
    assert_eq!(checked(&op, rows(json!([{"id": 1}])), "orders_items", |_, rs, _| Ok(rs)).unwrap().len(), 1);
}

/// A filter or cast naming a column the batch does not carry raises `PipelineTransformColumnMissing`, printing
/// the column and the table.
// spec: run.transform.column-missing@fa996ce3
#[test]
fn a_filter_or_cast_over_an_absent_column_is_refused() {
    let batch = rows(json!([{"id": 1}]));
    for op in [json!({"op": "cast", "column": "total", "to": "int64"}), json!({"op": "filter", "column": "total", "equals": 1})] {
        match apply(&ops(json!([op])), batch.clone(), "orders_items") {
            Err(RunError::PipelineTransformColumnMissing(m)) => assert!(m.contains("`total`") && m.contains("`orders_items`"), "{m}"),
            other => panic!("{other:?}"),
        }
    }
    // A column present on some rows is carried by the batch.
    assert!(apply(&ops(json!([{"op": "cast", "column": "total", "to": "int64"}])), rows(json!([{"id": 1}, {"total": "3"}])), "t").is_ok());
}

/// A cast rewrites one column's type and keeps its name and position.
// spec: run.transform.cast@4530b2c2
#[test]
fn a_cast_rewrites_the_type_in_place() {
    let batch = rows(json!([{"a": 1, "n": "42", "z": true}, {"a": 2, "n": "x", "z": false}]));
    let out = apply(&ops(json!([{"op": "cast", "column": "n", "to": "int64"}])), batch, "t").unwrap();
    assert_eq!(out, rows(json!([{"a": 1, "n": 42, "z": true}, {"a": 2, "n": null, "z": false}])));
    assert_eq!(out[0].keys().collect::<Vec<_>>(), ["a", "n", "z"]);
    let s = apply(&ops(json!([{"op": "cast", "column": "z", "to": "string"}])), out, "t").unwrap();
    assert_eq!(s[0]["z"], json!("true"));
    assert!(TransformOp::Cast { column: "z".into(), to: "decimal".into() }.validate().is_err());
}

/// `select` fixes the outgoing column set by name, and `rename` maps an incoming name onto an outgoing one.
// spec: run.transform.projection@7ab51a12
#[test]
fn select_fixes_the_column_set_and_rename_maps_a_name() {
    let batch = rows(json!([{"id": 1, "name": "a", "secret_note": "x"}]));
    let out = apply(&ops(json!([{"op": "select", "columns": ["id", "name", "missing"]}])), batch, "t").unwrap();
    assert_eq!(out, rows(json!([{"id": 1, "name": "a"}])));
    let out = apply(&ops(json!([{"op": "rename", "from": "name", "to": "title"}])), out, "t").unwrap();
    assert_eq!(out, rows(json!([{"id": 1, "title": "a"}])));
}

/// A cast to `binary`, `binary(n)`, `float32[n]` or `float16[n]` keeps a value that type's JSON form reads, nulls
/// any other, and lands the column in that type.
// spec: run.transform.typed-cast@9c74c021
#[test]
fn a_cast_to_a_binary_or_vector_type_keeps_readable_values_and_types_the_column() {
    use contextful_core::store::reconcile::{ColumnType, FloatItem};
    let chain = ops(json!([
        {"op": "cast", "column": "digest", "to": "binary(4)"},
        {"op": "cast", "column": "blob", "to": "binary"},
        {"op": "cast", "column": "embedding", "to": "float16[3]"},
        {"op": "cast", "column": "wide", "to": "float32[2]"}
    ]));
    for op in &chain {
        op.validate().unwrap();
    }
    let batch = rows(json!([
        {"digest": "3q2+7w==", "blob": "qgE=", "embedding": [0.5, -1, 0.25], "wide": [1, 2]},
        {"digest": "qgE=", "blob": "qgE", "embedding": [0.5, 1], "wide": "1,2"},
        {"digest": 7, "blob": null, "embedding": [0.5, 1, "x"], "wide": [1, null]}
    ]));
    assert_eq!(
        apply(&chain, batch, "vectors").unwrap(),
        rows(json!([
            {"digest": "3q2+7w==", "blob": "qgE=", "embedding": [0.5, -1, 0.25], "wide": [1, 2]},
            {"digest": null, "blob": null, "embedding": null, "wide": null},
            {"digest": null, "blob": null, "embedding": null, "wide": null}
        ]))
    );
    let stage = Chain { ops: chain, table: "vectors".into() };
    let types = stage.shape_types(Default::default());
    assert_eq!(
        types,
        [
            ("blob".to_string(), ColumnType::Binary),
            ("digest".to_string(), ColumnType::FixedSizeBinary(4)),
            ("embedding".to_string(), ColumnType::FixedSizeList(FloatItem::Float16, 3)),
            ("wide".to_string(), ColumnType::FixedSizeList(FloatItem::Float32, 2)),
        ]
        .into_iter()
        .collect()
    );
    for bad in ["binary(0)", "float64[3]", "float16[]", "bytes(4)"] {
        assert!(TransformOp::Cast { column: "v".into(), to: bad.into() }.validate().is_err(), "{bad}");
    }
}

/// A `rename` carries a pulled column type to the new name, a `select` drops it with its column, and a cast to a
/// scalar type drops it.
// spec: run.transform.type-carry@b2650639
#[test]
fn a_pulled_type_follows_a_rename_and_leaves_with_a_select_or_a_scalar_cast() {
    use contextful_core::store::reconcile::{ColumnType, FloatItem};
    let pulled: std::collections::BTreeMap<String, ColumnType> = [
        ("digest".to_string(), ColumnType::FixedSizeBinary(32)),
        ("embedding".to_string(), ColumnType::FixedSizeList(FloatItem::Float16, 3)),
        ("blob".to_string(), ColumnType::Binary),
        ("extra".to_string(), ColumnType::Binary),
    ]
    .into_iter()
    .collect();
    let stage = Chain {
        ops: ops(json!([
            {"op": "rename", "from": "digest", "to": "sha"},
            {"op": "cast", "column": "blob", "to": "string"},
            {"op": "select", "columns": ["sha", "embedding", "blob"]}
        ])),
        table: "vectors".into(),
    };
    assert_eq!(
        stage.shape_types(pulled),
        [("embedding".to_string(), ColumnType::FixedSizeList(FloatItem::Float16, 3)), ("sha".to_string(), ColumnType::FixedSizeBinary(32))]
            .into_iter()
            .collect()
    );
}
