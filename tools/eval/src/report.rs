//! The run report (`assurance.evaluate.run-report`): each case's return on each leg,
//! scored by the deterministic metrics and folded into per-leg and per-slice summaries
//! (`assurance.evaluate.deterministic-tier`), beside the run block and the case tally.
//!
//! The report is what [`crate::baseline::gate`] and [`crate::floors::check`] read.

use std::collections::BTreeMap;

use serde_json::{json, Map, Value};

use crate::baseline::RunStamp;
use crate::case::{self, Case};
use crate::metrics::{score_ranked, score_relevance, Aggregate, Returned, RowRef, LEGS, RANKED_METRICS, RELEVANCE_METRICS};

/// One case's return on each leg, in ranked order.
#[derive(Debug, Clone)]
pub struct CaseRun<'a> {
    pub case: &'a Case,
    /// Leg name → the rows the ranked call returned at limit k.
    pub legs: BTreeMap<String, Vec<Returned>>,
}

/// Per-metric aggregates of one leg.
#[derive(Debug, Default)]
struct Leg(BTreeMap<&'static str, Aggregate>);

impl Leg {
    fn push(&mut self, case: &Case, returned: &[Returned], k: usize) {
        let ranking: Vec<RowRef> = returned.iter().map(|r| r.row.clone()).collect();
        let ranked = score_ranked(&ranking, &case.relevant(), k);
        let rel = score_relevance(returned, &case.must_not(), case.recency_bound(), case.regression());
        let values = [
            ranked.recall_at_k,
            ranked.r_precision,
            ranked.hit_rate_at_k,
            ranked.reciprocal_rank,
            ranked.ndcg_at_k,
            rel.forbidden_row_rate,
            rel.duplicate_row_rate,
            rel.in_window_rate,
        ];
        for (metric, v) in RANKED_METRICS.iter().chain(RELEVANCE_METRICS.iter()).zip(values) {
            self.0.entry(metric).or_default().push(v);
        }
    }

    fn to_json(&self) -> Value {
        let mut out = Map::new();
        for metric in RANKED_METRICS.iter().chain(RELEVANCE_METRICS.iter()) {
            let summary = self.0.get(metric).map(Aggregate::summary).unwrap_or_else(|| Aggregate::default().summary());
            out.insert(metric.to_string(), serde_json::to_value(summary).expect("a summary serializes"));
        }
        Value::Object(out)
    }
}

/// Every leg of one surface.
#[derive(Debug, Default)]
struct Surface(BTreeMap<String, Leg>);

impl Surface {
    fn push(&mut self, run: &CaseRun<'_>, k: usize) {
        for leg in LEGS {
            let returned = run.legs.get(leg).map(Vec::as_slice).unwrap_or_default();
            self.0.entry(leg.to_string()).or_default().push(run.case, returned, k);
        }
    }

    fn to_json(&self) -> Value {
        Value::Object(self.0.iter().map(|(leg, l)| (leg.clone(), l.to_json())).collect())
    }
}

/// Build the run report of `runs`, scored at `k`, under `stamp` and `seed`.
///
/// Every case answers on every leg: a leg missing from a case's run scores as an empty
/// return. A case with no artifact truth drops out of the ranked means and counts as
/// dropped; a case with edge truth counts as unscored, since no edge surface runs.
pub fn build(stamp: &RunStamp, seed: u64, runs: &[CaseRun<'_>]) -> Value {
    let k = stamp.k;
    let mut retrieval = Surface::default();
    let mut slices: BTreeMap<&str, (usize, Surface)> = BTreeMap::new();
    let mut cases = Vec::with_capacity(runs.len());
    let (mut dropped, mut unscored) = (0usize, 0usize);
    for run in runs {
        retrieval.push(run, k);
        for tag in &run.case.tags {
            let (n, s) = slices.entry(tag.as_str()).or_default();
            *n += 1;
            s.push(run, k);
        }
        if run.case.relevant().is_empty() {
            dropped += 1;
        }
        if !run.case.expected.edges.is_empty() {
            unscored += 1;
        }
        let legs: Map<String, Value> = LEGS
            .iter()
            .map(|leg| {
                let rows: Vec<Value> =
                    run.legs.get(*leg).into_iter().flatten().map(|r| json!(case::render(&r.row))).collect();
                (leg.to_string(), Value::Array(rows))
            })
            .collect();
        cases.push(json!({ "id": run.case.id, "tags": run.case.tags, "legs": legs }));
    }
    let slices: Map<String, Value> = slices
        .into_iter()
        .map(|(tag, (n, s))| (tag.to_string(), json!({ "n_cases": n, "retrieval": s.to_json() })))
        .collect();
    json!({
        "run": stamp,
        "seed": seed,
        "n_cases": runs.len(),
        "tally": { "sampled": runs.len(), "dropped": dropped, "unscored": unscored },
        "retrieval": retrieval.to_json(),
        "slices": slices,
        "cases": cases,
    })
}
