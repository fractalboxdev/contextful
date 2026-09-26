//! Deterministic retrieval metrics: the five ranked metrics, the three relevance rates,
//! and the NaN-dropping aggregate every mean in a run report comes from.
//!
//! Top-k membership is distinct: a row repeated inside the top k counts once toward
//! every truth-based metric. A metric undefined for a case — no relevant set, no
//! must-not-retrieve set, no recency bound — is NaN, and [`Aggregate::push`] drops it,
//! so the case leaves the mean and its sample count alike. An empty ranking against a
//! non-empty relevant set is a real miss and scores zero.

use std::collections::HashSet;

use serde::{Deserialize, Serialize};

/// The retrieval legs every ranked metric reports under.
pub const LEGS: [&str; 3] = ["lexical", "vector", "hybrid"];

/// The ranked metrics, by the field name a run report carries them under.
pub const RANKED_METRICS: [&str; 5] = ["recall_at_k", "r_precision", "hit_rate_at_k", "reciprocal_rank", "ndcg_at_k"];

/// The relevance rates, by report field name. Each is lower-is-better except the in-window rate.
pub const RELEVANCE_METRICS: [&str; 3] = ["forbidden_row_rate", "duplicate_row_rate", "in_window_rate"];

/// A stored row a ranking names: its table and its row key.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct RowRef {
    pub table: String,
    pub key: String,
}

impl RowRef {
    pub fn new(table: impl Into<String>, key: impl Into<String>) -> Self {
        RowRef { table: table.into(), key: key.into() }
    }
}

/// One returned row with the in-window flag the engine projected for it against the
/// question's timeframe. The engine sets the flag false for a null or uncastable
/// publication value, so an undated row never reads as fresh.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Returned {
    pub row: RowRef,
    pub in_window: bool,
}

/// Distinct relevant rows among the first `window` positions of `ranked`.
fn distinct_hits(ranked: &[RowRef], relevant: &HashSet<RowRef>, window: usize) -> usize {
    let mut seen = HashSet::new();
    ranked.iter().take(window).filter(|r| seen.insert(*r) && relevant.contains(*r)).count()
}

/// Fraction of the relevant set present in the top k. NaN with no relevant set.
pub fn recall_at_k(ranked: &[RowRef], relevant: &HashSet<RowRef>, k: usize) -> f64 {
    if relevant.is_empty() {
        return f64::NAN;
    }
    distinct_hits(ranked, relevant, k) as f64 / relevant.len() as f64
}

/// Precision at rank min(k, R), R being the relevant-set size. NaN with no relevant set.
///
/// Precision at k caps at R/k, so a case with two relevant rows at k = 10 scores 0.2
/// with a perfect ranking; at min(k, R) that ranking scores 1.0.
pub fn r_precision(ranked: &[RowRef], relevant: &HashSet<RowRef>, k: usize) -> f64 {
    if relevant.is_empty() {
        return f64::NAN;
    }
    let window = k.min(relevant.len());
    if window == 0 {
        return 0.0;
    }
    distinct_hits(ranked, relevant, window) as f64 / window as f64
}

/// 1.0 when a relevant row sits in the top k, else 0.0. NaN with no relevant set.
pub fn hit_rate_at_k(ranked: &[RowRef], relevant: &HashSet<RowRef>, k: usize) -> f64 {
    if relevant.is_empty() {
        return f64::NAN;
    }
    if distinct_hits(ranked, relevant, k) > 0 {
        1.0
    } else {
        0.0
    }
}

/// Reciprocal of the 1-based rank of the first relevant row, 0.0 when none appears.
/// NaN with no relevant set.
pub fn reciprocal_rank(ranked: &[RowRef], relevant: &HashSet<RowRef>) -> f64 {
    if relevant.is_empty() {
        return f64::NAN;
    }
    ranked.iter().position(|r| relevant.contains(r)).map_or(0.0, |i| 1.0 / (i as f64 + 1.0))
}

/// Normalized discounted cumulative gain at k over binary relevance, each row counted
/// at its first position only. NaN with no relevant set.
pub fn ndcg_at_k(ranked: &[RowRef], relevant: &HashSet<RowRef>, k: usize) -> f64 {
    if relevant.is_empty() {
        return f64::NAN;
    }
    let gain = |i: usize| 1.0 / (i as f64 + 2.0).log2();
    let ideal: f64 = (0..relevant.len().min(k)).map(gain).sum();
    if ideal == 0.0 {
        return 0.0;
    }
    let mut seen = HashSet::new();
    let dcg: f64 = ranked
        .iter()
        .take(k)
        .enumerate()
        .filter(|(_, r)| seen.insert(*r) && relevant.contains(*r))
        .map(|(i, _)| gain(i))
        .sum();
    dcg / ideal
}

/// Fraction of the returned rows named in the case's must-not-retrieve set. NaN when
/// the case names none; 0.0 for an empty return, which carried no forbidden row.
pub fn forbidden_row_rate(returned: &[RowRef], must_not: &HashSet<RowRef>) -> f64 {
    if must_not.is_empty() {
        return f64::NAN;
    }
    if returned.is_empty() {
        return 0.0;
    }
    returned.iter().filter(|r| must_not.contains(*r)).count() as f64 / returned.len() as f64
}

/// Fraction of the ranking's positions repeating a table-and-row-key pair an earlier
/// position holds. Needs no truth, so it is always defined; 0.0 for an empty ranking.
///
/// Every truth-based metric counts a repeated row once, so a repeat is invisible to all
/// five of them and visible here alone.
pub fn duplicate_row_rate(ranking: &[RowRef]) -> f64 {
    if ranking.is_empty() {
        return 0.0;
    }
    let mut seen = HashSet::new();
    ranking.iter().filter(|r| !seen.insert(*r)).count() as f64 / ranking.len() as f64
}

/// Fraction of the returned rows the engine flagged in-window. NaN when the case
/// declares no recency bound, and NaN for an empty return: it holds no out-of-window
/// row, and a must-abstain case with a recency bound returns zero rows by design.
pub fn in_window_rate(returned: &[Returned], recency_bound: bool) -> f64 {
    if !recency_bound || returned.is_empty() {
        return f64::NAN;
    }
    returned.iter().filter(|r| r.in_window).count() as f64 / returned.len() as f64
}

/// One case's ranked metrics at `k`.
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct RankedScores {
    pub recall_at_k: f64,
    pub r_precision: f64,
    pub hit_rate_at_k: f64,
    pub reciprocal_rank: f64,
    pub ndcg_at_k: f64,
}

/// Score one case's ranking against its relevant set at `k`.
pub fn score_ranked(ranked: &[RowRef], relevant: &HashSet<RowRef>, k: usize) -> RankedScores {
    RankedScores {
        recall_at_k: recall_at_k(ranked, relevant, k),
        r_precision: r_precision(ranked, relevant, k),
        hit_rate_at_k: hit_rate_at_k(ranked, relevant, k),
        reciprocal_rank: reciprocal_rank(ranked, relevant),
        ndcg_at_k: ndcg_at_k(ranked, relevant, k),
    }
}

/// One case's relevance rates.
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct RelevanceScores {
    pub forbidden_row_rate: f64,
    pub duplicate_row_rate: f64,
    pub in_window_rate: f64,
}

/// Score what one case's return carried besides its truth.
pub fn score_relevance(returned: &[Returned], must_not: &HashSet<RowRef>, recency_bound: bool) -> RelevanceScores {
    let rows: Vec<RowRef> = returned.iter().map(|r| r.row.clone()).collect();
    RelevanceScores {
        forbidden_row_rate: forbidden_row_rate(&rows, must_not),
        duplicate_row_rate: duplicate_row_rate(&rows),
        in_window_rate: in_window_rate(returned, recency_bound),
    }
}

/// Per-case values of one metric, folded into the summary a run report carries.
#[derive(Debug, Clone, Default)]
pub struct Aggregate {
    values: Vec<f64>,
}

impl Aggregate {
    /// Add one case's value; a NaN or infinite value drops out of the aggregate.
    pub fn push(&mut self, v: f64) {
        if v.is_finite() {
            self.values.push(v);
        }
    }

    /// The cases behind the aggregate.
    pub fn n(&self) -> usize {
        self.values.len()
    }

    pub fn summary(&self) -> Summary {
        let n = self.values.len();
        if n == 0 {
            return Summary { n, mean: f64::NAN, min: f64::NAN, max: f64::NAN };
        }
        Summary {
            n,
            mean: self.values.iter().sum::<f64>() / n as f64,
            min: self.values.iter().copied().fold(f64::INFINITY, f64::min),
            max: self.values.iter().copied().fold(f64::NEG_INFINITY, f64::max),
        }
    }
}

impl FromIterator<f64> for Aggregate {
    fn from_iter<I: IntoIterator<Item = f64>>(iter: I) -> Self {
        let mut a = Aggregate::default();
        iter.into_iter().for_each(|v| a.push(v));
        a
    }
}

/// A mean-valued report field: the sample count, the mean, and the per-case extremes
/// the absolute floors read. With `n == 0` the other three are NaN, serialized `null`.
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct Summary {
    pub n: usize,
    pub mean: f64,
    pub min: f64,
    pub max: f64,
}
