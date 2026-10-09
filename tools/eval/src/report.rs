//! The run report (`assurance.evaluate.run-report`): each case's return on each leg,
//! scored by the deterministic metrics and folded into per-leg and per-slice summaries
//! (`assurance.evaluate.deterministic-tier`), beside the run block and the case tally.
//!
//! The report is what [`crate::baseline::gate`] and [`crate::floors::check`] read.

use std::collections::{BTreeMap, HashSet};

use serde_json::{json, Map, Value};

use crate::baseline::RunStamp;
use crate::case::{self, Case};
use crate::metrics::{
    abstention_rate, score_ranked, score_relevance, Aggregate, Returned, RowRef, LEGS, RANKED_METRICS, RELEVANCE_METRICS,
};
use crate::systems::Systems;

/// The table an edge id ranks under, so edge truth scores through the same metrics as rows.
pub const EDGE_TABLE: &str = "edge";

/// An edge id as the edge surface ranks it.
pub fn edge_ref(id: &str) -> RowRef {
    RowRef::new(EDGE_TABLE, id)
}

/// One case's return on each leg, in ranked order.
#[derive(Debug, Clone)]
pub struct CaseRun<'a> {
    pub case: &'a Case,
    /// Leg name → the rows the ranked call returned at limit k.
    pub legs: BTreeMap<String, Vec<Returned>>,
    /// Leg name → the edge ids the edge retriever returned at limit k; `None` when no edge
    /// surface is configured.
    pub edges: Option<BTreeMap<String, Vec<String>>>,
    /// The case's systems figures; `None` when the runner measured none.
    pub systems: Option<Systems>,
}

impl<'a> CaseRun<'a> {
    /// A case's artifact returns, with no edge surface and no systems figures.
    pub fn new(case: &'a Case, legs: BTreeMap<String, Vec<Returned>>) -> Self {
        CaseRun { case, legs, edges: None, systems: None }
    }
}

/// What one surface scores a case's return against.
struct Truth {
    relevant: HashSet<RowRef>,
    must_not: HashSet<RowRef>,
    recency_bound: bool,
    regression: bool,
    must_abstain: bool,
}

impl Truth {
    /// Expected artifacts, scored on the artifact retriever.
    fn artifacts(case: &Case) -> Truth {
        Truth {
            relevant: case.relevant(),
            must_not: case.must_not(),
            recency_bound: case.recency_bound(),
            regression: case.regression(),
            must_abstain: case.expected.must_abstain,
        }
    }

    /// Expected edges, scored on the edge retriever.
    fn edges(case: &Case) -> Truth {
        Truth {
            relevant: case.expected.edges.iter().map(|e| edge_ref(e)).collect(),
            must_not: HashSet::new(),
            recency_bound: false,
            regression: false,
            must_abstain: case.expected.must_abstain,
        }
    }
}

/// Per-metric aggregates of one leg.
#[derive(Debug, Default)]
struct Leg(BTreeMap<&'static str, Aggregate>);

impl Leg {
    fn push(&mut self, truth: &Truth, returned: &[Returned], k: usize) {
        let ranking: Vec<RowRef> = returned.iter().map(|r| r.row.clone()).collect();
        let ranked = score_ranked(&ranking, &truth.relevant, k);
        let rel = score_relevance(returned, &truth.must_not, truth.recency_bound, truth.regression);
        let values = [
            ranked.recall_at_k,
            ranked.r_precision,
            ranked.hit_rate_at_k,
            ranked.reciprocal_rank,
            ranked.ndcg_at_k,
            rel.forbidden_row_rate,
            rel.duplicate_row_rate,
            rel.in_window_rate,
            abstention_rate(returned.len(), truth.must_abstain),
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
    fn push(&mut self, truth: &Truth, legs: &BTreeMap<String, Vec<Returned>>, k: usize) {
        for leg in LEGS {
            let returned = legs.get(leg).map(Vec::as_slice).unwrap_or_default();
            self.0.entry(leg.to_string()).or_default().push(truth, returned, k);
        }
    }

    fn to_json(&self) -> Value {
        Value::Object(self.0.iter().map(|(leg, l)| (leg.clone(), l.to_json())).collect())
    }
}

/// The edge surface's returns as rankable rows.
fn edge_legs(edges: &BTreeMap<String, Vec<String>>) -> BTreeMap<String, Vec<Returned>> {
    edges
        .iter()
        .map(|(leg, ids)| (leg.clone(), ids.iter().map(|id| Returned { row: edge_ref(id), in_window: false }).collect()))
        .collect()
}

/// The artifact and edge surfaces of one population of cases.
#[derive(Debug, Default)]
struct Surfaces {
    retrieval: Surface,
    edge_retrieval: Option<Surface>,
}

impl Surfaces {
    fn push(&mut self, run: &CaseRun<'_>, k: usize) {
        self.retrieval.push(&Truth::artifacts(run.case), &run.legs, k);
        if let Some(edges) = &run.edges {
            self.edge_retrieval.get_or_insert_with(Surface::default).push(&Truth::edges(run.case), &edge_legs(edges), k);
        }
    }

    fn insert_into(&self, out: &mut Map<String, Value>) {
        out.insert("retrieval".into(), self.retrieval.to_json());
        if let Some(edges) = &self.edge_retrieval {
            out.insert("edge_retrieval".into(), edges.to_json());
        }
    }
}

/// The systems figures of one population of cases.
#[derive(Debug, Default)]
struct SystemsFold {
    tokens: Aggregate,
    latency_ms: Aggregate,
    cost: Aggregate,
}

impl SystemsFold {
    fn push(&mut self, s: &Systems) {
        self.tokens.push(s.tokens as f64);
        self.latency_ms.push(s.latency_ms);
        self.cost.push(s.cost);
    }

    fn to_json(&self) -> Map<String, Value> {
        [("tokens_per_query", &self.tokens), ("latency_ms", &self.latency_ms), ("cost", &self.cost)]
            .into_iter()
            .map(|(name, a)| (name.to_string(), serde_json::to_value(a.summary()).expect("a summary serializes")))
            .collect()
    }
}

/// Build the run report of `runs`, scored at `k`, under `stamp` and `seed`.
///
/// Every case answers on every leg: a leg missing from a case's run scores as an empty
/// return. A case with no artifact truth drops out of the ranked means and counts as
/// dropped. Expected artifacts score the artifact retriever under `retrieval` and expected
/// edges the edge retriever under `edge_retrieval`; a case carrying edge truth whose run
/// configures no edge surface counts as unscored. Systems figures report beside the
/// quality figures — `latency_ms` at the top, the rest under `systems` — and again per
/// corpus-size bucket.
pub fn build(stamp: &RunStamp, seed: u64, runs: &[CaseRun<'_>]) -> Value {
    let k = stamp.k;
    let mut surfaces = Surfaces::default();
    let mut slices: BTreeMap<&str, (usize, Surfaces)> = BTreeMap::new();
    let mut systems: Option<SystemsFold> = None;
    let mut buckets: BTreeMap<String, (usize, SystemsFold)> = BTreeMap::new();
    let mut cases = Vec::with_capacity(runs.len());
    let (mut dropped, mut unscored) = (0usize, 0usize);
    for run in runs {
        surfaces.push(run, k);
        for tag in &run.case.tags {
            let (n, s) = slices.entry(tag.as_str()).or_default();
            *n += 1;
            s.push(run, k);
        }
        if run.case.relevant().is_empty() {
            dropped += 1;
        }
        if !run.case.expected.edges.is_empty() && run.edges.is_none() {
            unscored += 1;
        }
        if let Some(s) = &run.systems {
            systems.get_or_insert_with(SystemsFold::default).push(s);
            let (n, fold) = buckets.entry(s.bucket()).or_default();
            *n += 1;
            fold.push(s);
        }
        let legs: Map<String, Value> = LEGS
            .iter()
            .map(|leg| {
                let rows: Vec<Value> =
                    run.legs.get(*leg).into_iter().flatten().map(|r| json!(case::render(&r.row))).collect();
                (leg.to_string(), Value::Array(rows))
            })
            .collect();
        let mut line = json!({ "id": run.case.id, "tags": run.case.tags, "legs": legs });
        if let Some(edges) = &run.edges {
            line["edges"] = json!(edges);
        }
        cases.push(line);
    }
    let slices: Map<String, Value> = slices
        .into_iter()
        .map(|(tag, (n, s))| {
            let mut out = Map::new();
            out.insert("n_cases".into(), json!(n));
            s.insert_into(&mut out);
            (tag.to_string(), Value::Object(out))
        })
        .collect();
    let mut report = Map::new();
    report.insert("run".into(), json!(stamp));
    report.insert("seed".into(), json!(seed));
    report.insert("n_cases".into(), json!(runs.len()));
    report.insert("tally".into(), json!({ "sampled": runs.len(), "dropped": dropped, "unscored": unscored }));
    surfaces.insert_into(&mut report);
    if let Some(fold) = systems {
        let mut node = fold.to_json();
        if let Some(latency) = node.remove("latency_ms") {
            report.insert("latency_ms".into(), latency);
        }
        let buckets: Map<String, Value> = buckets
            .into_iter()
            .map(|(label, (n, fold))| {
                let mut b = fold.to_json();
                b.insert("n_cases".into(), json!(n));
                (label, Value::Object(b))
            })
            .collect();
        node.insert("buckets".into(), Value::Object(buckets));
        report.insert("systems".into(), Value::Object(node));
    }
    report.insert("slices".into(), Value::Object(slices));
    report.insert("cases".into(), Value::Array(cases));
    Value::Object(report)
}
