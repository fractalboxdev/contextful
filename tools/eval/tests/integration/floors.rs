use contextful_eval::floors::{self, *};
use contextful_eval::metrics::*;
use serde_json::{json, Value};

use crate::{clean_retrieval, returned, rows, set, summary, EPS};

/// A report whose every floor holds but for the hybrid leg's `metric`, which summarizes `values`.
fn hybrid(metric: &str, values: &[f64]) -> Value {
    let mut report = json!({ "retrieval": clean_retrieval(40) });
    report["retrieval"]["hybrid"][metric] = summary(values);
    report
}

fn breached(report: &Value) -> Vec<String> {
    floors::check(report).breaches().map(|c| c.path.clone()).collect()
}

/// Precision at rank min(k, R), R being the relevant-set size, holds at or above 60 percent.
// spec: assurance.evaluate.precision-floor@fb332492
#[test]
fn a_leg_below_sixty_percent_r_precision_breaches_the_floor() {
    assert_eq!(PRECISION_FLOOR, 0.60);
    // Five cases, each with two relevant rows at k = 10. Precision@10 would cap every
    // one at 0.2; R-precision scores the ranking at rank 2.
    let g = set(&["a", "b"]);
    let rankings = [
        rows(&["a", "b", "x"]),
        rows(&["a", "b", "x"]),
        rows(&["a", "x", "b"]),
        rows(&["x", "a", "b"]),
        rows(&["x", "y", "a"]),
    ];
    let per_case: Vec<f64> = rankings.iter().map(|r| r_precision(r, &g, 10)).collect();
    let mean = per_case.iter().sum::<f64>() / 5.0;
    assert!((mean - 0.6).abs() < EPS, "{per_case:?}");
    assert!(breached(&hybrid("r_precision", &per_case)).is_empty(), "a mean at the floor holds");

    let below = [per_case.as_slice(), &[0.5]].concat();
    assert_eq!(breached(&hybrid("r_precision", &below)), ["retrieval.hybrid.r_precision.mean"]);

    // A leg of cases without truth measured nothing, which holds no floor.
    assert_eq!(breached(&hybrid("r_precision", &[f64::NAN])), ["retrieval.hybrid.r_precision.mean"]);
}

/// A row named in a must-not-retrieve set holds at 0 percent of a regression case's returned rows.
// spec: assurance.evaluate.forbidden-row-rate@ac6c7f70
#[test]
fn one_forbidden_row_in_one_case_breaches_the_floor() {
    assert_eq!(FORBIDDEN_ROW_RATE_CEILING, 0.0);
    let must_not = set(&["stale", "offtopic"]);
    let clean = forbidden_row_rate(&rows(&["a", "b", "c", "d"]), &must_not);
    let dirty = forbidden_row_rate(&rows(&["a", "b", "offtopic", "d"]), &must_not);
    assert_eq!(clean, 0.0);
    assert!((dirty - 0.25).abs() < EPS);
    // A case naming no forbidden row is not a regression case and drops out.
    let unnamed = forbidden_row_rate(&rows(&["offtopic"]), &set(&[]));
    assert!(unnamed.is_nan());
    // Nor is a case outside the regression tag, whatever its must-not set names.
    let exploratory = score_relevance(&returned(&[("offtopic", true)]), &must_not, false, false);
    assert!(exploratory.forbidden_row_rate.is_nan());
    let regression = score_relevance(&returned(&[("offtopic", true)]), &must_not, false, true);
    assert_eq!(regression.forbidden_row_rate, 1.0);

    assert!(breached(&hybrid("forbidden_row_rate", &[clean, clean, unnamed])).is_empty());
    // The floor holds every case, not the mean: one case in forty breaches it.
    let mut cases = vec![clean; 39];
    cases.push(dirty);
    assert_eq!(breached(&hybrid("forbidden_row_rate", &cases)), ["retrieval.hybrid.forbidden_row_rate.max"]);
}

/// A repeated table-and-row-key pair holds at 0 percent of the ranking.
// spec: assurance.evaluate.duplicate-row-rate@8edb7ef0
#[test]
fn a_repeated_table_and_row_key_pair_breaches_the_floor() {
    assert_eq!(DUPLICATE_ROW_RATE_CEILING, 0.0);
    // One key under two tables is two rows.
    let distinct = vec![RowRef::new("notes", "7"), RowRef::new("plants", "7"), RowRef::new("notes", "8")];
    assert_eq!(duplicate_row_rate(&distinct), 0.0);
    // The same pair twice in five positions; recall reads the repeat as clean.
    let repeated = vec![
        RowRef::new("notes", "7"),
        RowRef::new("notes", "8"),
        RowRef::new("notes", "8"),
        RowRef::new("plants", "7"),
        RowRef::new("notes", "9"),
    ];
    assert!((duplicate_row_rate(&repeated) - 0.2).abs() < EPS);
    assert!((recall_at_k(&repeated, &set(&["8"]), 5) - 1.0).abs() < EPS);

    assert!(breached(&hybrid("duplicate_row_rate", &[0.0, 0.0])).is_empty());
    assert_eq!(breached(&hybrid("duplicate_row_rate", &[0.0, 0.2])), ["retrieval.hybrid.duplicate_row_rate.max"]);
}

/// A case declaring a recency bound holds its in-window rate at or above 95 percent.
// spec: assurance.evaluate.in-window-rate@b4ee1f57
#[test]
fn a_bounded_case_under_ninety_five_percent_in_window_breaches_the_floor() {
    assert_eq!(IN_WINDOW_RATE_FLOOR, 0.95);
    let mut fresh: Vec<(&str, bool)> = (0..19).map(|_| ("r", true)).collect();
    fresh.push(("r", false));
    let at_floor = in_window_rate(&returned(&fresh), true);
    assert!((at_floor - 0.95).abs() < EPS);
    // The right rows from the wrong month: every truth metric perfect, the window empty.
    let stale = returned(&[("a", false), ("b", false)]);
    let stale_rate = in_window_rate(&stale, true);
    assert_eq!(stale_rate, 0.0);
    let ids: Vec<RowRef> = stale.iter().map(|r| r.row.clone()).collect();
    assert!((recall_at_k(&ids, &set(&["a", "b"]), 2) - 1.0).abs() < EPS);
    // A case declaring no bound drops out.
    let unbounded = in_window_rate(&stale, false);
    assert!(unbounded.is_nan());

    assert!(breached(&hybrid("in_window_rate", &[at_floor, 1.0, unbounded])).is_empty());
    // A mean of 0.98 still hides one case at 0.9: the floor reads that case.
    let failing = hybrid("in_window_rate", &[1.0, 1.0, 1.0, 1.0, 0.9]);
    assert!(failing["retrieval"]["hybrid"]["in_window_rate"]["mean"].as_f64().unwrap() >= 0.95);
    assert_eq!(breached(&failing), ["retrieval.hybrid.in_window_rate.min"]);
}

#[test]
fn every_leg_of_every_surface_answers_to_the_floors() {
    let mut report = json!({ "retrieval": clean_retrieval(40), "edge_retrieval": { "hybrid": clean_retrieval(40)["hybrid"].clone() } });
    report["retrieval"]["lexical"]["duplicate_row_rate"] = summary(&[0.1]);
    report["edge_retrieval"]["hybrid"]["duplicate_row_rate"] = summary(&[0.5]);
    let v = floors::check(&report);
    assert_eq!(v.checks.len(), 16);
    assert!(!v.passed);
    assert_eq!(breached(&report), ["retrieval.lexical.duplicate_row_rate.max", "edge_retrieval.hybrid.duplicate_row_rate.max"]);
}

/// A report lacking a floor's figure on any leg of its `retrieval` surface breaches that floor; only a forbidden-
/// row or in-window rate over no case holds.
// spec: assurance.baseline.floor-coverage@b98d2d1f
#[test]
fn a_report_missing_a_floor_figure_breaches_that_floor() {
    // The runner dropped the retrieval block: no floor was read, so none holds.
    let v = floors::check(&json!({ "run": crate::run(10), "judge": {} }));
    assert!(!v.passed);
    assert_eq!(v.breaches().count(), 12, "{:?}", v.checks);
    // One leg without its precision figure breaches the precision floor there alone.
    let mut report = json!({ "retrieval": clean_retrieval(40) });
    report["retrieval"]["vector"].as_object_mut().unwrap().remove("r_precision");
    assert_eq!(breached(&report), ["retrieval.vector.r_precision.mean"]);
    // A rate summary without an integer sample count names no case count, and breaches.
    for summary in [json!({ "mean": 0.5, "min": 0.5, "max": 1.0 }), json!({ "n": 40.0, "min": 0.0, "max": 1.0 }), json!({ "n": null, "max": 1.0 })] {
        for metric in ["forbidden_row_rate", "in_window_rate"] {
            let mut r = json!({ "retrieval": clean_retrieval(40) });
            r["retrieval"]["hybrid"][metric] = summary.clone();
            let field = if metric == "forbidden_row_rate" { "max" } else { "min" };
            assert_eq!(breached(&r), [format!("retrieval.hybrid.{metric}.{field}")], "{metric} {summary}");
        }
    }
    // A duplicate rate over no case breaches; a forbidden-row or in-window rate over no case holds.
    for metric in ["forbidden_row_rate", "in_window_rate", "duplicate_row_rate"] {
        let mut r = json!({ "retrieval": clean_retrieval(40) });
        r["retrieval"]["lexical"][metric] = summary(&[f64::NAN]);
        let expected: Vec<String> = if metric == "duplicate_row_rate" { vec![format!("retrieval.lexical.{metric}.max")] } else { vec![] };
        assert_eq!(breached(&r), expected, "{metric}");
    }
}
