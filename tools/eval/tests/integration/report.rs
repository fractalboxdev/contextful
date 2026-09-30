use std::collections::BTreeMap;

use contextful_eval::baseline::{RunStamp, Tier};
use contextful_eval::case::{load, Case};
use contextful_eval::floors;
use contextful_eval::metrics::{Returned, RowRef, LEGS, RANKED_METRICS, RELEVANCE_METRICS};
use contextful_eval::report::{build, CaseRun};
use serde_json::{json, Value};

use crate::EPS;

fn stamp() -> RunStamp {
    RunStamp { k: 3, tier: Tier::Deterministic, model: None, samples: 1 }
}

fn cases() -> Vec<Case> {
    let lines = [
        json!({"id": "hit", "corpus": "c", "question": "q", "tags": ["cjk"], "expected": {"artifacts": ["t#a", "t#b"]}}),
        json!({"id": "miss", "corpus": "c", "question": "q", "tags": ["cjk", "regression"],
               "expected": {"artifacts": ["t#c"], "must_not_retrieve": ["t#x"]}}),
        json!({"id": "fresh", "corpus": "c", "question": "q", "expected": {"artifacts": ["t#a"], "since": "2030-03-01T00:00:00Z"}}),
        json!({"id": "graph", "corpus": "c", "question": "q", "expected": {"edges": ["e1"]}}),
    ];
    load(&lines.iter().map(Value::to_string).collect::<Vec<_>>().join("\n")).unwrap()
}

fn ret(keys: &[(&str, bool)]) -> Vec<Returned> {
    keys.iter().map(|(k, w)| Returned { row: RowRef::new("t", *k), in_window: *w }).collect()
}

/// Every leg returns `rows` for each case.
fn runs<'a>(cases: &'a [Case], rows: &[Vec<Returned>]) -> Vec<CaseRun<'a>> {
    cases
        .iter()
        .zip(rows)
        .map(|(case, r)| CaseRun { case, legs: LEGS.iter().map(|l| (l.to_string(), r.clone())).collect::<BTreeMap<_, _>>() })
        .collect()
}

fn mean(report: &Value, path: &str) -> f64 {
    report.pointer(path).and_then(Value::as_f64).unwrap_or(f64::NAN)
}

/// Recall@k, R-precision, hit-rate@k, reciprocal rank and nDCG@k run with no model call, reported per leg — lexical, vector, hybrid — and per slice.
// spec: assurance.evaluate.deterministic-tier@0d6252fa
#[test]
fn every_ranked_metric_reports_per_leg_and_per_slice() {
    let cases = cases();
    let rows = [ret(&[("a", true), ("b", true)]), ret(&[("x", true), ("y", true)]), ret(&[("a", true)]), ret(&[])];
    let mut runs = runs(&cases, &rows);
    // The vector leg misses the first case entirely.
    runs[0].legs.insert("vector".into(), ret(&[("z", true)]));
    let report = build(&stamp(), 7, &runs);
    for leg in LEGS {
        for metric in RANKED_METRICS.iter().chain(RELEVANCE_METRICS.iter()) {
            assert!(report["retrieval"][leg][metric].is_object(), "retrieval.{leg}.{metric}");
            assert!(report["slices"]["cjk"]["retrieval"][leg][metric].is_object(), "slices.cjk.retrieval.{leg}.{metric}");
        }
    }
    // Three cases carry artifact truth: two perfect, one empty-handed.
    assert!((mean(&report, "/retrieval/hybrid/recall_at_k/mean") - 2.0 / 3.0).abs() < EPS);
    assert_eq!(report["retrieval"]["hybrid"]["recall_at_k"]["n"], json!(3));
    assert!((mean(&report, "/retrieval/vector/recall_at_k/mean") - 1.0 / 3.0).abs() < EPS);
    assert!((mean(&report, "/slices/cjk/retrieval/hybrid/reciprocal_rank/mean") - 0.5).abs() < EPS);
    assert_eq!(report["slices"]["cjk"]["n_cases"], json!(2));
    assert!((mean(&report, "/retrieval/lexical/ndcg_at_k/mean") - 2.0 / 3.0).abs() < EPS);
    // The report's legs are the rows each call returned.
    assert_eq!(report["cases"][0]["legs"]["vector"], json!(["t#z"]));
    assert_eq!(report["cases"][0]["legs"]["hybrid"], json!(["t#a", "t#b"]));
}

/// A case tagged `regression` is a regression case, and its must-not-retrieve set scores {{assurance.evaluate.forbidden-row-rate}}.
// spec: assurance.evaluate.regression-case@400cf44d
#[test]
fn only_a_regression_case_scores_its_must_not_retrieve_set() {
    let cases = cases();
    let clean = [ret(&[("a", true)]), ret(&[("c", true)]), ret(&[("a", true)]), ret(&[])];
    let report = build(&stamp(), 7, &runs(&cases, &clean));
    assert_eq!(report["retrieval"]["hybrid"]["forbidden_row_rate"]["n"], json!(1), "one regression case");
    assert_eq!(mean(&report, "/retrieval/hybrid/forbidden_row_rate/max"), 0.0);

    let leaked = [ret(&[("a", true)]), ret(&[("c", true), ("x", true)]), ret(&[("a", true)]), ret(&[])];
    let report = build(&stamp(), 7, &runs(&cases, &leaked));
    assert!((mean(&report, "/retrieval/hybrid/forbidden_row_rate/max") - 0.5).abs() < EPS);
    assert!(floors::check(&report).breaches().any(|b| b.path == "retrieval.hybrid.forbidden_row_rate.max"));

    // The same must-not set on a case without the tag scores nothing.
    let untagged: Vec<Case> = cases.into_iter().map(|mut c| {
        c.tags.retain(|t| t != "regression");
        c
    }).collect();
    let report = build(&stamp(), 7, &runs(&untagged, &leaked));
    assert_eq!(report["retrieval"]["hybrid"]["forbidden_row_rate"]["n"], json!(0));
}

/// A run report carries one field per gated metric, the sample count behind each mean, the run block, the per-slice breakdown, and the tally of cases sampled, dropped and unscored.
// spec: assurance.evaluate.run-report@11fd2470
#[test]
fn a_report_carries_its_run_block_counts_and_tally() {
    let cases = cases();
    let rows = [ret(&[("a", true), ("b", true)]), ret(&[("c", true)]), ret(&[("a", true), ("q", false)]), ret(&[])];
    let report = build(&stamp(), 0x5eed, &runs(&cases, &rows));
    assert_eq!(report["run"], json!({ "k": 3, "tier": "deterministic", "model": null, "samples": 1 }));
    assert_eq!(report["seed"], json!(0x5eed));
    assert_eq!(report["n_cases"], json!(4));
    assert_eq!(report["tally"], json!({ "sampled": 4, "dropped": 1, "unscored": 1 }));
    // One case declares a recency bound, and one of its two rows sits outside the window.
    assert_eq!(report["retrieval"]["lexical"]["in_window_rate"]["n"], json!(1));
    assert!((mean(&report, "/retrieval/lexical/in_window_rate/min") - 0.5).abs() < EPS);
    // The duplicate rate needs no truth, so every case counts toward it.
    assert_eq!(report["retrieval"]["hybrid"]["duplicate_row_rate"]["n"], json!(4));
    // A metric no case defines serializes its figures null.
    let graph = &cases[3..];
    let only = build(&stamp(), 0, &runs(graph, &rows[3..]));
    assert_eq!(only["retrieval"]["hybrid"]["recall_at_k"], json!({ "n": 0, "mean": null, "min": null, "max": null }));
    // The baseline gate reads the report as written.
    let file = r#"{"_run": {"k": 3, "tier": "deterministic", "model": null, "samples": 1}, "n_cases": 4, "retrieval.hybrid.forbidden_row_rate.n": 1}"#;
    let b = contextful_eval::baseline::Baselines::parse(file).unwrap();
    assert!(contextful_eval::baseline::gate(&report, &b).unwrap().baseline_passed());
}
