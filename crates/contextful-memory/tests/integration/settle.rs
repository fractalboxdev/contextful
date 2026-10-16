//! Predictions and outcomes land through the writer's grant and settle through the label
//! join read in the reader's session.

use super::support::*;
use contextful_context::read::{Face, ReadOptions};
use contextful_context::Store;
use contextful_core::grant::Action;
use contextful_core::memory::settle::{Observation, RawRegistration};
use contextful_core::store::bound_time::Bounds;
use contextful_core::store::lay_out::NodeId;
use contextful_memory::settle::{labels, observe, predict};
use contextful_memory::MemoryFault;
use contextful_policy::enforce::mask::Pepper;
use contextful_policy::enforce::session::Request;
use serde_json::{json, Value};

const SETTLEMENT: &str = r#"
[[table]]
name = "memory/predictions"
shape = "predictions"
columns = ["prediction_id", "subject", "predicted_at", "horizon_secs", "deadline_at", "resolution_source", "comparator", "statement", "metric"]

[[table]]
name = "memory/outcomes"
shape = "outcomes"
columns = ["prediction_id", "observed_at", "verdict", "resolution_source", "signal", "settling_citation", "metric"]
"#;

fn settling() -> Fixture {
    let dir = tempfile::tempdir().unwrap();
    let face = Face::open(Store::open(dir.path(), "research").unwrap(), &format!("{MANIFEST}{SETTLEMENT}"), Pepper::resolve(|_| None)).unwrap();
    Fixture::over(dir, face)
}

fn registration(id: &str, source: &str) -> RawRegistration {
    RawRegistration {
        prediction_id: id.into(),
        subject: "acme".into(),
        predicted_at: at("2030-01-01T00:00:00Z"),
        horizon_secs: Some(86_400),
        deadline_at: None,
        watch: false,
        resolution_source: Some(source.into()),
        comparator: None,
        statement: Some("acme ships v2 by the deadline".into()),
        metric: None,
    }
}

fn observation(id: &str, verdict: bool, citation: Option<&str>) -> Observation {
    Observation {
        prediction_id: id.into(),
        observed_at: at("2030-01-02T06:00:00Z"),
        verdict: Some(verdict),
        resolution_source: Some("adjudicator".into()),
        signal: None,
        settling_citation: citation.map(str::to_string),
        metric: None,
    }
}

fn node() -> NodeId {
    NodeId::parse("memory-a").unwrap()
}

fn identifier(fault: MemoryFault) -> &'static str {
    match fault {
        MemoryFault::Memory(e) => e.identifier(),
        other => panic!("expected a memory refusal, got {other}"),
    }
}

fn raw_rows(f: &Fixture, sql: &str) -> Value {
    json!(f.face.operator_query(sql, ReadOptions::default()).unwrap().rows)
}

/// The label view of Shapes runs over a declared `predictions` and `outcomes` table pair as read through the reader's session.
// spec: read.settle.label-join@22db9811
#[test]
fn the_label_join_reads_a_settled_prediction_through_the_session() {
    let f = settling();
    let writer = f.writer();
    let now = at("2030-01-03T00:00:00Z");
    predict(&f.face, &writer, "memory/predictions", registration("p1", "adjudicator"), &node(), now, &super::synthesize::admit).unwrap();
    observe(&f.face, &writer, "memory/outcomes", None, observation("p1", true, Some("https://example.org/ruling")), &node(), now.plus_secs(1), &super::synthesize::admit)
        .unwrap();
    let reader = f.authority("agent://reader", &[Action::Read], &["memory/*"]);
    let session = f.face.session(&reader, &Request::default(), Bounds::default()).unwrap();
    let r = labels(&f.face, &session, "memory/predictions", "memory/outcomes", Some("p1")).unwrap();
    let col = |n: &str| r.columns.iter().position(|c| c == n).unwrap();
    assert_eq!(r.rows.len(), 1, "{:?}", r.rows);
    assert_eq!(r.rows[0][col("verdict")], json!(true));
    assert_eq!(r.rows[0][col("settling_citation")], json!("https://example.org/ruling"));
    assert_eq!(r.rows[0][col("lead_time_s")].as_f64(), Some(108_000.0));

    // A reader without the outcomes table reads no label.
    let blind = f.authority("agent://reader", &[Action::Read], &["memory/predictions"]);
    let session = f.face.session(&blind, &Request::default(), Bounds::default()).unwrap();
    assert!(labels(&f.face, &session, "memory/predictions", "memory/outcomes", Some("p1")).is_err());
}

/// A metric reference names a series no table of the store holds yet, and the registration lands.
#[test]
fn a_metric_reference_is_never_resolved_against_the_store() {
    let f = settling();
    let unlanded = RawRegistration {
        comparator: Some(">= 1000000".into()),
        metric: Some("metrics/revenue:acme-q1".into()),
        ..registration("m1", "metric")
    };
    assert!(f.face.store().try_schema("metrics/revenue").unwrap().is_none());
    let landed = predict(&f.face, &f.writer(), "memory/predictions", unlanded, &node(), at("2030-01-03T00:00:00Z"), &super::synthesize::admit).unwrap();
    assert!(landed.landed);
    assert_eq!(raw_rows(&f, "SELECT metric FROM \"memory/predictions\""), json!([["metrics/revenue:acme-q1"]]));
}

/// An observation naming a `prediction_id` no row of the declared `predictions` table carries in the writer's session raises `OutcomePredictionUnregistered`, and nothing lands.
// spec: read.settle.prediction-unregistered@79aa8751
#[test]
fn an_observation_of_an_unregistered_prediction_refuses() {
    let f = settling();
    let writer = f.writer();
    let refused = observe(&f.face, &writer, "memory/outcomes", None, observation("ghost", true, Some("https://example.org/r")), &node(), at("2030-01-03T00:00:00Z"), &super::synthesize::admit);
    assert_eq!(identifier(refused.unwrap_err()), "OutcomePredictionUnregistered");
    assert!(f.face.store().try_schema("memory/outcomes").unwrap().is_none(), "nothing landed");
}

/// A registration or observation identical to the stored row of its `prediction_id` lands nothing, and the verb succeeds with its usual output.
// spec: read.settle.restated-landing@fea518f8
#[test]
fn a_restated_registration_or_observation_lands_nothing() {
    let f = settling();
    let writer = f.writer();
    let now = at("2030-01-03T00:00:00Z");
    for (i, landed) in [(0, true), (1, false)] {
        let p = predict(&f.face, &writer, "memory/predictions", registration("p1", "adjudicator"), &node(), now.plus_secs(i), &super::synthesize::admit).unwrap();
        assert_eq!((p.prediction_id.as_str(), p.landed), ("p1", landed));
    }
    for (i, landed) in [(2, true), (3, false)] {
        let o = observe(&f.face, &writer, "memory/outcomes", None, observation("p1", true, Some("https://example.org/r")), &node(), now.plus_secs(i), &super::synthesize::admit)
            .unwrap();
        assert_eq!(o.landed, landed);
    }
    assert_eq!(raw_rows(&f, "SELECT CAST(count(DISTINCT _run_id) AS INTEGER) FROM \"memory/predictions\""), json!([[1]]));
    assert_eq!(raw_rows(&f, "SELECT CAST(count(DISTINCT _run_id) AS INTEGER) FROM \"memory/outcomes\""), json!([[1]]));
}

/// A registration whose `prediction_id` a stored registration with any other field already holds raises `OutcomeRegistrationConflict`, and nothing lands.
// spec: read.settle.registration-conflict@e4a77cad
#[test]
fn a_reused_prediction_id_with_other_fields_refuses() {
    let f = settling();
    let writer = f.writer();
    let now = at("2030-01-03T00:00:00Z");
    predict(&f.face, &writer, "memory/predictions", registration("p1", "adjudicator"), &node(), now, &super::synthesize::admit).unwrap();
    let refused = predict(&f.face, &writer, "memory/predictions", registration("p1", "manual"), &node(), now.plus_secs(1), &super::synthesize::admit);
    assert_eq!(identifier(refused.unwrap_err()), "OutcomeRegistrationConflict");
    let restated = RawRegistration { statement: Some("acme ships v3 by the deadline".into()), ..registration("p1", "adjudicator") };
    let refused = predict(&f.face, &writer, "memory/predictions", restated, &node(), now.plus_secs(2), &super::synthesize::admit);
    assert_eq!(identifier(refused.unwrap_err()), "OutcomeRegistrationConflict", "the statement takes part in the conflict");
    assert_eq!(raw_rows(&f, "SELECT resolution_source FROM \"memory/predictions\""), json!([["adjudicator"]]));
}

/// An observation differing from a stored outcome of its `prediction_id` that carries a verdict raises `OutcomeAlreadySettled`; a stored outcome without a verdict is replaced.
// spec: read.settle.outcome-settled@557a58ab
#[test]
fn a_settled_verdict_is_not_rewritten_but_an_open_outcome_is_replaced() {
    let f = settling();
    let writer = f.writer();
    let now = at("2030-01-03T00:00:00Z");
    predict(&f.face, &writer, "memory/predictions", registration("p1", "adjudicator"), &node(), now, &super::synthesize::admit).unwrap();
    let open = Observation { verdict: None, resolution_source: None, ..observation("p1", false, None) };
    observe(&f.face, &writer, "memory/outcomes", None, open, &node(), now.plus_secs(1), &super::synthesize::admit).unwrap();
    observe(&f.face, &writer, "memory/outcomes", None, observation("p1", true, Some("https://example.org/r")), &node(), now.plus_secs(2), &super::synthesize::admit).unwrap();
    let refused = observe(&f.face, &writer, "memory/outcomes", None, observation("p1", false, Some("https://example.org/r")), &node(), now.plus_secs(3), &super::synthesize::admit);
    assert_eq!(identifier(refused.unwrap_err()), "OutcomeAlreadySettled");
    assert_eq!(raw_rows(&f, "SELECT verdict FROM \"memory/outcomes\""), json!([[true]]));
}
