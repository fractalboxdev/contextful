use contextful_eval::metrics::*;

use crate::{rows, set, EPS};

#[test]
fn the_ranked_metrics_take_their_textbook_values() {
    let r = rows(&["a", "b", "c", "d"]);
    let g = set(&["a", "c"]);
    assert!((recall_at_k(&r, &g, 4) - 1.0).abs() < EPS);
    assert!((recall_at_k(&r, &g, 2) - 0.5).abs() < EPS);
    assert!((hit_rate_at_k(&r, &g, 1) - 1.0).abs() < EPS);
    assert!((reciprocal_rank(&rows(&["x", "y", "c"]), &g) - 1.0 / 3.0).abs() < EPS);
    assert_eq!(reciprocal_rank(&rows(&["x", "y"]), &g), 0.0);
    // dcg = 1/log2(2) + 1/log2(4); idcg = 1/log2(2) + 1/log2(3).
    let expected = 1.5 / (1.0 + 1.0 / 3f64.log2());
    assert!((ndcg_at_k(&r, &g, 4) - expected).abs() < 1e-6);
    assert!((ndcg_at_k(&rows(&["a", "c", "b"]), &g, 4) - 1.0).abs() < EPS);
}

/// Membership in the top k is distinct, and a repeated id counts once.
// spec: assurance.evaluate.distinct-top-k@67b5fa69
#[test]
fn a_repeated_row_counts_once_in_the_top_k() {
    let g = set(&["a", "b"]);
    // `a` fills every position: one distinct relevant row of two.
    let flooded = rows(&["a", "a", "a"]);
    assert!((recall_at_k(&flooded, &g, 3) - 0.5).abs() < EPS);
    assert!((r_precision(&flooded, &g, 3) - 0.5).abs() < EPS);
    let ideal = 1.0 + 1.0 / 3f64.log2();
    assert!((ndcg_at_k(&flooded, &g, 3) - 1.0 / ideal).abs() < EPS);
    // A repeat still occupies its position: `b` at rank 3 earns rank 3's gain.
    let repeated = rows(&["a", "a", "b"]);
    assert!((recall_at_k(&repeated, &g, 3) - 1.0).abs() < EPS);
    assert!((ndcg_at_k(&repeated, &g, 3) - 1.5 / ideal).abs() < EPS);
    // The same key under another table is another row.
    let other = vec![RowRef::new("notes", "a"), RowRef::new("plants", "a")];
    assert!((recall_at_k(&other, &g, 2) - 0.5).abs() < EPS);
}

/// A ranked metric over a case with no relevant set is NaN and drops out of every
/// aggregate; an empty ranking against a non-empty relevant set scores zero.
// spec: assurance.evaluate.absent-truth@63ae22b3
#[test]
fn a_case_with_no_truth_is_nan_and_leaves_the_aggregate() {
    let r = rows(&["a", "b"]);
    let none = set(&[]);
    let s = score_ranked(&r, &none, 2);
    for v in [s.recall_at_k, s.r_precision, s.hit_rate_at_k, s.reciprocal_rank, s.ndcg_at_k] {
        assert!(v.is_nan(), "a metric with no relevant set scored {v}");
    }

    let empty = score_ranked(&[], &set(&["a"]), 2);
    for v in [empty.recall_at_k, empty.r_precision, empty.hit_rate_at_k, empty.reciprocal_rank, empty.ndcg_at_k] {
        assert_eq!(v, 0.0, "an empty ranking against real truth is a miss");
    }

    let hit = score_ranked(&r, &set(&["a"]), 2);
    let agg: Aggregate = [hit.recall_at_k, s.recall_at_k, empty.recall_at_k].into_iter().collect();
    let sum = agg.summary();
    assert_eq!(sum.n, 2, "the truthless case counts toward no sample");
    assert!((sum.mean - 0.5).abs() < EPS);

    let unmeasured = serde_json::to_value([f64::NAN].into_iter().collect::<Aggregate>().summary()).unwrap();
    assert_eq!(unmeasured["n"], 0);
    assert!(unmeasured["mean"].is_null(), "a mean over no case serializes null, never zero");
}

#[test]
fn r_precision_measures_at_min_k_r_where_precision_at_k_caps_at_r_over_k() {
    let g = set(&["a", "b"]);
    // A perfect ranking of two relevant rows at k = 10 scores 1.0, not 0.2.
    let perfect = rows(&["a", "b", "x", "y", "z"]);
    assert!((r_precision(&perfect, &g, 10) - 1.0).abs() < EPS);
    // One of the first two positions relevant.
    assert!((r_precision(&rows(&["a", "x", "b"]), &g, 10) - 0.5).abs() < EPS);
    // k below R: the window is k.
    let many = set(&["a", "b", "c", "d"]);
    assert!((r_precision(&rows(&["a", "x", "b", "c"]), &many, 2) - 0.5).abs() < EPS);
}

#[test]
fn the_relevance_rates_are_defined_only_where_the_case_declares_their_input() {
    assert!(forbidden_row_rate(&rows(&["a"]), &set(&[])).is_nan());
    assert_eq!(forbidden_row_rate(&[], &set(&["a"])), 0.0);
    assert_eq!(duplicate_row_rate(&[]), 0.0);
    assert!(in_window_rate(&crate::returned(&[("a", true)]), false).is_nan());
    assert_eq!(in_window_rate(&[], true), 0.0);
}
