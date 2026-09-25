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
