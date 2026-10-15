//! A backtest run's pure core: the case file, the rows a run plants, and the report a run
//! and its replay both compute from the recorded calls and audit entries
//! (`assurance.backtest`, `assurance.forecast`). `contextful eval backtest` lands, plants
//! and reads; `contextful eval replay` re-serves the recording; both hand this module the
//! same inputs, so a replay reproduces the run's digest.

use std::collections::BTreeMap;

use contextful_core::memory::calibrate::{self, Settled};
use contextful_core::memory::declare::Shape;
use contextful_core::memory::settle::{self, Observation, RawRegistration, Registration};
use contextful_core::run::journal::sha256_hex;
use contextful_core::store::declare::TableDecl;
use contextful_core::time::Instant;
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};

use crate::backtest::{self, BoundedCase, LabelInterval, Step};
use crate::error::EvalError;
use crate::forecast::{self, Answer, ForecastCase, RoundReport, Source, Track};

/// The prefix every planted row's string cells carry, followed by the case id.
pub const PLANTED: &str = "__planted__";

/// Seconds past the horizon a planted row becomes available (`assurance.backtest.planted-future`).
pub const PLANTED_AFTER_SECS: u64 = 86_400;

/// The predicate a forecast's settled predictions group under in the calibration report.
pub const FORECAST_PREDICATE: &str = "forecast";

/// One backtest case as the case file writes it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CaseSpec {
    pub id: String,
    /// The corpus directory, relative to the case file's directory.
    pub corpus: String,
    pub horizon: Instant,
    #[serde(default)]
    pub label: Option<LabelSpec>,
    /// The reads the case makes, in order.
    pub reads: Vec<ReadSpec>,
    /// A universe member delisted inside the case's window (`assurance.backtest.planted-delisting`).
    #[serde(default)]
    pub delisting: Option<Delisting>,
    /// Entities and the instant each first became available (`assurance.backtest.query-finding`).
    #[serde(default)]
    pub entities: BTreeMap<String, Instant>,
    #[serde(default)]
    pub forecast: Option<ForecastSpec>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LabelSpec {
    pub t0: Instant,
    pub t1: Instant,
}

/// One read tool call.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReadSpec {
    pub tool: String,
    #[serde(default)]
    pub arguments: Map<String, Value>,
}

/// The delisting row a universe case plants: the member's delisting return.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Delisting {
    pub table: String,
    pub row: Map<String, Value>,
}

/// A forecast case's question, its answers and its settling observation.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ForecastSpec {
    pub resolution: String,
    pub resolved_at: Instant,
    pub outcome: f64,
    #[serde(default)]
    pub market_price: Option<f64>,
    #[serde(default)]
    pub min_lead_secs: u64,
    #[serde(default)]
    pub answer: Option<AnswerSpec>,
    #[serde(default)]
    pub no_evidence: Option<AnswerSpec>,
    /// When the outcome was observed, and by which source.
    pub observed_at: Instant,
    pub resolution_source: String,
    #[serde(default)]
    pub comparator: Option<String>,
    #[serde(default)]
    pub settling_citation: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AnswerSpec {
    pub probability: f64,
    pub recorded: Instant,
}

/// One recorded call: the case, its position, the arguments sent, the answer served and
/// the audit entry it appended.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RecordedCall {
    pub case: String,
    pub index: usize,
    pub tool: String,
    pub arguments: Map<String, Value>,
    pub answer: Value,
    #[serde(default)]
    pub seq: Option<u64>,
}

/// Load a backtest case file: one case per non-blank line, ids distinct.
pub fn load(text: &str) -> Result<Vec<CaseSpec>, String> {
    let mut cases: Vec<CaseSpec> = Vec::new();
    for (i, line) in text.lines().enumerate().filter(|(_, l)| !l.trim().is_empty()) {
        let case: CaseSpec = serde_json::from_str(line).map_err(|e| format!("line {}: {e}", i + 1))?;
        if cases.iter().any(|c| c.id == case.id) {
            return Err(format!("line {}: the id `{}` repeats an earlier case", i + 1, case.id));
        }
        cases.push(case);
    }
    if cases.is_empty() {
        return Err("the file holds no case".into());
    }
    Ok(cases)
}

impl CaseSpec {
    /// The bounded case, refusing a missing label interval and, for a forecast, a
    /// resolution before the freeze — both before any read.
    pub fn bounded(&self) -> Result<BoundedCase, EvalError> {
        let case = BoundedCase { id: self.id.clone(), horizon: self.horizon, label: self.label.map(|l| LabelInterval { t0: l.t0, t1: l.t1 }), trajectory: Vec::new() };
        case.label_interval()?;
        if let Some(f) = self.forecast_case() {
            f.backtest()?;
        }
        Ok(case)
    }

    /// The forecast case, frozen at the horizon.
    pub fn forecast_case(&self) -> Option<ForecastCase> {
        let f = self.forecast.as_ref()?;
        Some(ForecastCase {
            id: self.id.clone(),
            freeze: self.horizon,
            resolution: f.resolution.clone(),
            resolved_at: f.resolved_at,
            outcome: f.outcome,
            source: f.market_price.map_or(Source::Dataset, |freeze_price| Source::Market { freeze_price }),
            min_lead_secs: f.min_lead_secs,
        })
    }

    /// The marker a planted row's string cells carry.
    pub fn marker(&self) -> String {
        format!("{PLANTED}/{}", self.id)
    }
}

/// The row a case plants in `decl`'s table: `template`'s columns, each string cell the
/// case's marker, the availability column one day past the horizon
/// (`assurance.backtest.planted-future`). `None` for a table declaring no availability time.
pub fn planted_row(decl: &TableDecl, template: &Map<String, Value>, case: &CaseSpec) -> Option<Map<String, Value>> {
    let available = decl.available_time.as_ref()?;
    let marker = case.marker();
    let mut row: Map<String, Value> = template.iter().map(|(k, v)| (k.clone(), if v.is_string() { json!(marker) } else { v.clone() })).collect();
    for key in decl.primary_key() {
        row.insert(key.clone(), json!(marker));
    }
    row.insert(available.column.clone(), json!(case.horizon.plus_secs(PLANTED_AFTER_SECS).to_rfc3339_nanos()));
    Some(row)
}

fn text_of(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

/// Whether the recorded answers hold the delisting row: every primary-key value of it
/// appears in one answer row (`assurance.backtest.planted-delisting`).
pub fn holds_delisting(delisting: &Delisting, key: &[String], answers: &[&Value]) -> bool {
    let wanted: Vec<String> = key.iter().filter_map(|k| delisting.row.get(k)).map(text_of).collect();
    if wanted.is_empty() {
        return false;
    }
    answers.iter().filter_map(|a| a.get("rows").and_then(Value::as_array)).flatten().any(|row| {
        let cells: Vec<String> = match row {
            Value::Array(cells) => cells.iter().map(text_of).collect(),
            Value::Object(m) => m.values().map(text_of).collect(),
            other => vec![text_of(other)],
        };
        wanted.iter().all(|w| cells.iter().any(|c| c == w))
    })
}

/// Settle one forecast case through the label join: its registration checks, its
/// observation checks against it, and the join keeps it within the grace window
/// (`assurance.forecast.settlement`). `Ok(None)` for an observation outside the window.
pub fn settle(case: &CaseSpec, answer: &AnswerSpec) -> Result<Option<Settled>, String> {
    let f = case.forecast.as_ref().ok_or("no forecast")?;
    let registration = Registration::check(RawRegistration {
        prediction_id: case.id.clone(),
        subject: case.id.clone(),
        predicted_at: answer.recorded,
        horizon_secs: None,
        deadline_at: Some(f.resolved_at),
        watch: false,
        resolution_source: Some(f.resolution_source.clone()),
        comparator: f.comparator.clone(),
    })
    .map_err(|e| e.to_string())?;
    let held = f.outcome >= 0.5;
    let observation = Observation {
        prediction_id: case.id.clone(),
        observed_at: f.observed_at,
        verdict: Some(held),
        resolution_source: Some(f.resolution_source.clone()),
        signal: None,
        settling_citation: f.settling_citation.clone(),
    };
    settle::observe(&registration, &observation).map_err(|e| e.to_string())?;
    if !settle::in_label_window(registration.predicted_at, registration.deadline(), observation.observed_at) {
        return Ok(None);
    }
    Ok(Some(Settled { shape: Shape::Facts, predicate: FORECAST_PREDICATE.into(), confidence: answer.probability, held }))
}

/// What a run's report computes from: the cases, each table's declaration, the recorded
/// calls and the audit attributes by entry seq.
pub struct Recording<'a> {
    pub cases: &'a [CaseSpec],
    pub decls: &'a BTreeMap<String, Vec<TableDecl>>,
    pub calls: &'a [RecordedCall],
    pub entries: &'a BTreeMap<u64, Value>,
}

/// A forecast case beside the reader's and the no-evidence forecaster's answers.
type Answered = (ForecastCase, Option<AnswerSpec>, Option<AnswerSpec>);

/// The report's `cases` and `forecast` blocks.
pub fn assess(r: &Recording<'_>) -> Result<Value, String> {
    let mut cases = Vec::new();
    let mut forecasts: Vec<Answered> = Vec::new();
    let mut settled = Vec::new();
    let mut unsettled = Vec::new();
    for spec in r.cases {
        let bounded = spec.bounded().map_err(|e| e.to_string())?;
        let calls: Vec<&RecordedCall> = r.calls.iter().filter(|c| c.case == spec.id).collect();
        let answers: Vec<&Value> = calls.iter().map(|c| &c.answer).collect();
        let entries: Vec<Value> = calls.iter().map(|c| c.seq.and_then(|s| r.entries.get(&s)).cloned().unwrap_or(Value::Null)).collect();
        let mut failures = backtest::trajectory_check(&entries, &BTreeMap::new(), u8::MAX);
        // Each answer is the one its signed entry digested (`disclosure.record.bound-entry`).
        let digests: Vec<String> = calls.iter().map(|c| sha256_hex(&serde_json::to_vec(&c.answer).expect("an answer serializes"))).collect();
        for ((c, entry), d) in calls.iter().zip(&entries).zip(&digests) {
            if entry.get("contextful.result.sha256").and_then(Value::as_str).is_some_and(|recorded| recorded != d) {
                failures.push(format!("call {} (`{}`) answers other than its audit entry digested", c.index, c.tool));
            }
        }
        // A planted row in the trajectory fails the case (`assurance.backtest.planted-future`).
        let marker = spec.marker();
        for c in &calls {
            if c.answer.to_string().contains(&marker) {
                failures.push(format!("call {} (`{}`) returned a row planted after the horizon", c.index, c.tool));
            }
        }
        if let Some(d) = &spec.delisting {
            let decls = r.decls.get(&spec.corpus).map(Vec::as_slice).unwrap_or_default();
            let key = decls.iter().find(|t| t.name == d.table).map(|t| t.primary_key().to_vec()).unwrap_or_default();
            if !holds_delisting(d, &key, &answers) {
                failures.push(format!("the trajectory holds no delisting return from `{}`", d.table));
            }
        }
        let trajectory = BoundedCase {
            trajectory: calls
                .iter()
                .map(|c| Step {
                    tool: c.tool.clone(),
                    query: c.seq.and_then(|s| r.entries.get(&s)).and_then(|e| e["contextful.read.query"].as_str()).map(str::to_string),
                    documents: Vec::new(),
                })
                .collect(),
            ..bounded
        };
        let findings = backtest::query_findings(&trajectory, &spec.entities);
        cases.push(json!({
            "id": spec.id,
            "horizon": spec.horizon.to_rfc3339_nanos(),
            "calls": calls.len(),
            "trajectory": trajectory.trajectory.iter().zip(&digests).map(|(s, d)| json!({ "tool": s.tool, "query": s.query, "answer_sha256": d })).collect::<Vec<_>>(),
            "verdict": if failures.is_empty() { "pass" } else { "fail" },
            "failures": failures,
            "findings": findings,
        }));
        if let (Some(fc), Some(f)) = (spec.forecast_case(), &spec.forecast) {
            match f.answer {
                Some(a) => match settle(spec, &a)? {
                    Some(s) => {
                        settled.push(s);
                        forecasts.push((fc, f.answer, f.no_evidence));
                    }
                    None => unsettled.push(spec.id.clone()),
                },
                None => forecasts.push((fc, None, f.no_evidence)),
            }
        }
    }
    let mut report = json!({ "cases": cases });
    if !forecasts.is_empty() || !unsettled.is_empty() {
        let fcs: Vec<ForecastCase> = forecasts.iter().map(|(c, _, _)| c.clone()).collect();
        let answers = |pick: fn(&Answered) -> Option<AnswerSpec>| -> BTreeMap<String, Answer> {
            forecasts.iter().filter_map(|f| pick(f).map(|a| (f.0.id.clone(), Answer { probability: a.probability, recorded: a.recorded }))).collect()
        };
        let reader = answers(|f| f.1);
        let no_evidence = answers(|f| f.2);
        let mut round = RoundReport::default();
        round.insert(Track::Backtest, "reader", forecast::standing(&fcs, &reader, Track::Backtest, (!no_evidence.is_empty()).then_some(&no_evidence)));
        let calibration: Vec<Value> = calibrate::report(&settled)
            .iter()
            .map(|g| json!({ "shape": g.shape.name(), "predicate": g.predicate, "samples": g.samples, "brier": g.brier, "ece": g.ece }))
            .collect();
        report["forecast"] = json!({ "round": round, "calibration": calibration, "unsettled": unsettled });
    }
    Ok(report)
}

/// The digest a report carries: the SHA-256 of its canonical JSON without the `digest`
/// field (`assurance.backtest.trajectory-replay`).
pub fn digest(report: &Value) -> String {
    let mut body = report.clone();
    if let Value::Object(m) = &mut body {
        m.remove("digest");
    }
    sha256_hex(&serde_json::to_vec(&body).expect("a report serializes"))
}
