//! The backtest run's pure core: planted rows, the delisting check, settlement and the
//! report a run and its replay compute alike.

use std::collections::BTreeMap;

use contextful_core::store::declare::TableDecl;
use contextful_eval::backtest_run::*;
use serde_json::{json, Map, Value};

const EARNINGS: &str = "[[pipeline.tables]]\nname = \"research/earnings\"\nprimary_key = [\"filing_id\"]\n[pipeline.tables.available_time]\ncolumn = \"accepted_at\"\n\n[[pipeline.tables]]\nname = \"research/prices\"\nprimary_key = [\"ticker\"]\n";

fn spec(extra: Value) -> CaseSpec {
    let mut c = json!({ "id": "q1", "corpus": "corpus", "horizon": "2030-05-03T00:00:00Z",
        "label": { "t0": "2030-03-31T00:00:00Z", "t1": "2030-05-02T00:00:00Z" },
        "reads": [{ "tool": "context.query", "arguments": { "sql": "SELECT 1" } }] });
    for (k, v) in extra.as_object().unwrap() {
        c[k] = v.clone();
    }
    serde_json::from_value(c).unwrap()
}

fn object(v: Value) -> Map<String, Value> {
    v.as_object().unwrap().clone()
}

#[test]
fn a_planted_row_carries_the_marker_and_becomes_available_after_the_horizon() {
    let decls = TableDecl::parse_pipeline(EARNINGS).unwrap();
    let case = spec(json!({}));
    let template = object(json!({ "filing_id": "acme-q1", "ticker": "ACME", "eps": 1.23, "accepted_at": "2030-05-02T16:05:00Z" }));
    let row = planted_row(&decls[0], &template, &case).unwrap();
    assert_eq!(row["filing_id"], json!("__planted__/q1"));
    assert_eq!(row["ticker"], json!("__planted__/q1"));
    assert_eq!(row["eps"], json!(1.23));
    assert_eq!(row["accepted_at"], json!("2030-05-04T00:00:00.000000000Z"));
    assert!(planted_row(&decls[1], &template, &case).is_none(), "a table declaring no availability time takes no planted row");
}

fn report(case: &CaseSpec, answer: Value, entry: Value) -> Value {
    let decls: BTreeMap<String, Vec<TableDecl>> = [("corpus".to_string(), TableDecl::parse_pipeline(EARNINGS).unwrap())].into();
    let call = RecordedCall { case: case.id.clone(), index: 0, tool: "context.query".into(), arguments: Map::new(), answer, seq: Some(1) };
    let entries: BTreeMap<u64, Value> = [(1, entry)].into();
    assess(&Recording { cases: std::slice::from_ref(case), decls: &decls, calls: &[call], entries: &entries }).unwrap()
}

fn bound_entry(answer: &Value) -> Value {
    json!({
        "contextful.bounds": { "available_as_of": "2030-05-03T00:00:00.000000000Z", "inclusive": true },
        "contextful.read.snapshots": {}, "contextful.read.query": "SELECT 1",
        "contextful.result.sha256": contextful_core::run::journal::sha256_hex(&serde_json::to_vec(answer).unwrap()),
    })
}

#[test]
fn a_planted_row_in_an_answer_fails_the_case() {
    let case = spec(json!({}));
    let clean = json!({ "columns": ["filing_id"], "rows": [["acme-q1"]] });
    assert_eq!(report(&case, clean.clone(), bound_entry(&clean))["cases"][0]["verdict"], json!("pass"));
    let leaked = json!({ "columns": ["filing_id"], "rows": [["acme-q1"], ["__planted__/q1"]] });
    let r = report(&case, leaked.clone(), bound_entry(&leaked));
    assert_eq!(r["cases"][0]["verdict"], json!("fail"));
    assert!(r["cases"][0]["failures"][0].as_str().unwrap().contains("planted after the horizon"), "{r}");
    // An answer other than the one its entry digested fails too.
    let r = report(&case, leaked, bound_entry(&clean));
    assert!(r["cases"][0]["failures"].as_array().unwrap().iter().any(|f| f.as_str().unwrap().contains("audit entry digested")), "{r}");
}

#[test]
fn the_delisting_return_holds_when_one_answer_row_carries_its_key() {
    let d = Delisting { table: "research/returns".into(), row: object(json!({ "ticker": "ZZZ", "ret": -1.0 })) };
    let key = vec!["ticker".to_string()];
    let with = json!({ "rows": [["ACME", 0.02], ["ZZZ", -1.0]] });
    let without = json!({ "rows": [["ACME", 0.02]] });
    assert!(holds_delisting(&d, &key, &[&without, &with]));
    assert!(!holds_delisting(&d, &key, &[&without]));
    assert!(!holds_delisting(&d, &[], &[&with]), "a delisting with no key value is never held");
}

#[test]
fn settlement_checks_the_source_and_keeps_the_grace_window() {
    let forecast = |observed: &str, source: &str| {
        spec(json!({ "forecast": { "resolution": "r", "resolved_at": "2030-08-01T00:00:00Z", "outcome": 1.0,
            "observed_at": observed, "resolution_source": source, "comparator": "eps >= 1.0" } }))
    };
    let answer = AnswerSpec { probability: 0.8, recorded: contextful_core::time::Instant::parse("2030-05-03T00:00:00Z").unwrap() };
    let settled = settle(&forecast("2030-08-02T00:00:00Z", "metric"), &answer).unwrap().unwrap();
    assert_eq!((settled.confidence, settled.held, settled.predicate.as_str()), (0.8, true, FORECAST_PREDICATE));
    assert!(settle(&forecast("2030-08-02T00:00:01Z", "metric"), &answer).unwrap().is_none(), "past the deadline plus a day");
    let err = settle(&forecast("2030-08-02T00:00:00Z", "manual"), &answer).unwrap_err();
    assert!(err.contains("OutcomeRegistrationInvalid"), "{err}");
}

#[test]
fn the_digest_omits_only_its_own_field() {
    let r = json!({ "cases": [], "run": { "seed": 1 } });
    let mut carried = r.clone();
    carried["digest"] = json!(digest(&r));
    assert_eq!(digest(&carried), digest(&r));
    carried["run"]["seed"] = json!(2);
    assert_ne!(digest(&carried), digest(&r));
}
