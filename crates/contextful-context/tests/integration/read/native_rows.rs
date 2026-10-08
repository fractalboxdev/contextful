//! Admitted native task inputs retain stored JSON types through the registered relation.

use super::*;
use contextful_core::run::derive::task::{DeriveTask, Derived, HostUnit, Tasks};
use contextful_core::run::RunError;
use std::sync::Arc;

struct Counter;

impl DeriveTask for Counter {
    fn version(&self) -> &str { "1" }
    fn columns(&self) -> Vec<String> { vec!["counter".into(), "text".into()] }
    fn marker_table(&self) -> String { "units".into() }
    fn content_tables(&self) -> Vec<String> { vec!["counts".into()] }
    fn derive(&self, unit: &HostUnit) -> Result<Derived, RunError> {
        let count = unit.row.get("counter").and_then(Value::as_i64)
            .ok_or_else(|| RunError::Invalid(format!("native task input is not an integer: {:?}", unit.row)))?;
        let text = unit.row.get("text").and_then(Value::as_str)
            .ok_or_else(|| RunError::Invalid("native text input changed JSON kind".into()))?;
        Ok([("counts".into(), vec![json!({"counter": count, "text": text}).as_object().unwrap().clone()])].into())
    }
}

#[test]
fn a_registered_task_receives_numeric_values_and_numeric_looking_text_distinctly() {
    let r = Reads::new();
    land_rows(&r.store, "research/counters", "counter-load", json!([
        {"id": "one", "counter": 9_007_199_254_740_993_i64, "text": "9007199254740993"}
    ]));
    let session = r.session(&["research/counters"], None, None);
    let mut tasks = Tasks::default();
    tasks.register("counter", Arc::new(Counter)).unwrap();
    let task = tasks.get("counter").unwrap();
    let response = r.face.source_rows(&session, "research/counters", None, &["counter", "text"]).unwrap();
    assert_eq!(response.rows.len(), 1, "the task input exists before its type assertions");
    let row = response.columns.iter().cloned().zip(response.rows[0].clone()).collect();
    let unit = HostUnit { key: "one".into(), row, prior_attempts: 0, derivation_key: "counter-v1".into() };
    let derived = task.derive(&unit).expect("an admitted native row preserves its original JSON kinds");
    assert_eq!(derived["counts"][0]["counter"], json!(9_007_199_254_740_993_i64));
    assert_eq!(derived["counts"][0]["text"], json!("9007199254740993"));
    let wire = r.face.rows(&session, "research/counters", None).unwrap();
    assert_eq!(column(&wire, "counter"), [json!("9007199254740993")], "the wire response retains its declared wide-integer encoding");
}

#[test]
fn admitted_table_rows_apply_the_grant_row_ceiling() {
    let r = Reads::new();
    land_rows(&r.store, "research/counters", "counter-load", json!([{"counter": 1}, {"counter": 2}]));
    let mut grant = read(&["research/counters"], None);
    grant.max_rows = Some(1);
    let session = r.session_for(loop_subject("agent://counter"), vec![grant], None);
    let response = r.face.rows(&session, "research/counters", None).unwrap();
    assert_eq!(response.rows.len(), 1, "an internal row path ignores the admitted grant's row ceiling");
    assert!(response.truncated, "a second stored row supplies the truncation witness");
    let native = r.face.source_rows(&session, "research/counters", None, &["counter"]).unwrap();
    assert_eq!(native.rows.len(), 1);
    assert!(native.truncated);
}

#[test]
fn admitted_table_rows_refuse_a_response_past_the_grant_byte_ceiling() {
    let r = Reads::new();
    land_rows(&r.store, "research/counters", "counter-load", json!([{"body": "x".repeat(1024)}]));
    let mut grant = read(&["research/counters"], None);
    grant.max_response_bytes = Some(64);
    let session = r.session_for(loop_subject("agent://counter"), vec![grant], None);
    refused_with(r.face.rows(&session, "research/counters", None), "ReadResponseTooLarge");
    refused_with(r.face.source_rows(&session, "research/counters", None, &["body"]), "ReadResponseTooLarge");
}

#[test]
fn native_rows_keep_zone_masks_and_restriction_metadata() {
    let r = Reads::new();
    let session = r.session(&["research/*"], Some(("research/notes", "acme")), Some("public-cloud:us-east-1"));
    let native = r.face.source_rows(&session, "research/visits", None, &["visit_id", "case_notes"]).unwrap();
    assert_eq!(column(&native, "visit_id"), [json!("w1")]);
    assert_eq!(column(&native, "case_notes"), [Value::Null]);
    let wire = r.face.rows(&session, "research/visits", None).unwrap();
    assert_eq!(native.blocks["contextful.restriction"], wire.blocks["contextful.restriction"]);
    assert_eq!(native.blocks["contextful.restriction"]["tables"][0]["columns_masked"], json!(["case_notes"]));
}

#[test]
fn native_rows_obey_the_grant_duration_and_the_next_read_recovers() {
    let r = Reads::new();
    let mut grant = read(&["research/visits"], None);
    grant.max_duration_ms = Some(0);
    let session = r.session_for(loop_subject("agent://counter"), vec![grant], None);
    refused_with(r.face.source_rows(&session, "research/visits", None, &["visit_id"]), "ReadDurationExceeded");
    let fresh = r.session(&["research/visits"], None, None);
    assert_eq!(r.face.source_rows(&fresh, "research/visits", None, &["visit_id"]).unwrap().rows, [vec![json!("w1")]]);
}

#[test]
fn independently_admitted_equivalent_sessions_reuse_the_native_connection() {
    let r = Reads::new();
    let authority = r.authority(loop_subject("agent://research-loop"), vec![read(&["research/visits"], None)]);
    let first = r.face.session(&authority, &Request::default(), Bounds::default()).unwrap();
    let one = r.face.source_rows(&first, "research/visits", None, &["visit_id"]).unwrap();
    let opens = r.face.pool().counts().engine_opens;
    let second = r.face.session(&authority, &Request::default(), Bounds::default()).unwrap();
    assert_eq!(r.face.source_rows(&second, "research/visits", None, &["visit_id"]).unwrap(), one);
    assert_eq!(r.face.pool().counts().engine_opens, opens);
    let empty = r.face.source_rows(&second, "research/visits", None, &["absent"]).unwrap();
    assert!(empty.columns.is_empty());
    assert_eq!(empty.rows, [Vec::<Value>::new()]);
}

// spec: read.respond.engine-composed-ceiling@dfaf65de
#[test]
fn engine_composed_rows_past_the_face_ceiling_arrive_complete_without_a_declared_ceiling() {
    let r = Reads::new();
    let rows: Vec<Value> = (0..10_001).map(|n| json!({ "counter": n })).collect();
    land_rows(&r.store, "research/counters", "counter-load", Value::Array(rows));
    let session = r.session(&["research/counters"], None, None);
    let wire = r.face.rows(&session, "research/counters", None).unwrap();
    assert_eq!(wire.rows.len(), 10_001, "a memory read sees every row an unbounded grant reads");
    assert!(!wire.truncated);
    let native = r.face.source_rows(&session, "research/counters", None, &["counter"]).unwrap();
    assert_eq!(native.rows.len(), 10_001, "a derive source over more rows than the face ceiling stays complete");
    assert!(!native.truncated);
}
