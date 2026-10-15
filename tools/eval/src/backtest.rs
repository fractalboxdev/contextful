//! The backtest (`assurance.backtest`): bounded cases, their label intervals and splits,
//! checkpoint routing and the judge's answer-key mode, the cutoff-relative slicing of a
//! report and its sensitivity, fills, the execution component, the trial ledger and its
//! deflated Sharpe ratio, and the checks a case's recorded trajectory answers to.
//!
//! Every function is a pure computation over declared cases, model references, price
//! rows, return series and recorded audit attributes.

use std::collections::{BTreeMap, BTreeSet};

use contextful_core::connector::model_ref::{CutoffTag, ModelRef};
use contextful_core::store::declare::TableDecl;
use contextful_core::time::Instant;
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};

use crate::error::EvalError;

const DAY_SECS: f64 = 86_400.0;

/// A label's observation span `[t0, t1]` (`assurance.backtest.label-interval`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LabelInterval {
    pub t0: Instant,
    pub t1: Instant,
}

impl LabelInterval {
    fn overlaps(&self, other: &LabelInterval) -> bool {
        self.t0 <= other.t1 && other.t0 <= self.t1
    }
}

/// One recorded step of a case's run: a tool call, its query text and the documents it
/// retrieved (`assurance.backtest.bounded-case`).
#[derive(Debug, Clone, PartialEq)]
pub struct Step {
    pub tool: String,
    pub query: Option<String>,
    pub documents: Vec<String>,
}

/// A case read under a horizon (`assurance.backtest.bounded-case`).
#[derive(Debug, Clone, PartialEq)]
pub struct BoundedCase {
    pub id: String,
    /// The instant each of the case's reads passes as `available_as_of`.
    pub horizon: Instant,
    pub label: Option<LabelInterval>,
    /// The ordered steps the case's run recorded.
    pub trajectory: Vec<Step>,
}

impl BoundedCase {
    /// `arguments` as the case's read sends them: `available_as_of` at the horizon,
    /// replacing any the caller wrote.
    pub fn read_arguments(&self, mut arguments: Map<String, Value>) -> Map<String, Value> {
        arguments.insert("available_as_of".into(), json!(self.horizon.to_rfc3339_nanos()));
        arguments
    }

    /// The label interval, or `EvalLabelIntervalMissing` before any read.
    pub fn label_interval(&self) -> Result<LabelInterval, EvalError> {
        self.label.ok_or_else(|| EvalError::EvalLabelIntervalMissing { case: self.id.clone() })
    }
}

/// A train/test split with purge and embargo (`assurance.backtest.split`): every
/// training case whose label interval overlaps a test case's is purged, and every
/// training case starting within `embargo_secs` after a test fold ends is embargoed.
pub fn split(train: &[BoundedCase], test: &[BoundedCase], embargo_secs: u64) -> Result<Vec<String>, EvalError> {
    let tests: Vec<LabelInterval> = test.iter().map(BoundedCase::label_interval).collect::<Result<_, _>>()?;
    let fold_end = tests.iter().map(|l| l.t1).max();
    let mut kept = Vec::new();
    for case in train {
        let label = case.label_interval()?;
        let purged = tests.iter().any(|t| t.overlaps(&label));
        let embargoed = fold_end.is_some_and(|end| label.t0 > end && label.t0 <= end.plus_secs(embargo_secs));
        if !purged && !embargoed {
            kept.push(case.id.clone());
        }
    }
    Ok(kept)
}

/// The newest checkpoint of a family whose post-training cutoff precedes the case's
/// horizon; an undeclared cutoff compares as the call's instant `now`
/// (`assurance.backtest.checkpoint-route`).
pub fn route(family: &[ModelRef], horizon: Instant, now: Instant) -> Option<&ModelRef> {
    family.iter().filter(|m| m.posttrain_end(now) <= horizon).max_by_key(|m| m.posttrain_end(now))
}

/// How a judge scores a case (`assurance.backtest.answer-key-only`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JudgeMode {
    /// The judge reasons over the answer, the evidence and the expected answer.
    Reasoned,
    /// The judge compares the answer with the stored expected answer alone.
    AnswerKeyOnly,
}

/// A case resolved before the judge's post-training cutoff ends is scored against its
/// stored answer alone.
pub fn judge_mode(judge: &ModelRef, resolved: Instant, now: Instant) -> JudgeMode {
    if resolved < judge.posttrain_end(now) {
        JudgeMode::AnswerKeyOnly
    } else {
        JudgeMode::Reasoned
    }
}

/// What a report states leakage against (`assurance.backtest.cutoff-slice`): never a
/// bare pre/post gap.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum LeakageReference {
    /// A declared expected score.
    Declared { score: String },
    /// A discontinuity at the cutoff.
    CutoffDiscontinuity,
    /// A model whose cutoff postdates no case.
    CleanControl { model: String },
}

/// One scored case for the cutoff-relative report.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Scored {
    pub resolved: Instant,
    pub score: f64,
}

/// A cutoff-relative slice: cases whose resolution falls on one side of the cutoff.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct CutoffSlice {
    /// Days from the cutoff to each case's resolution, in case order.
    pub offsets_days: Vec<f64>,
    pub before: Side,
    pub after: Side,
    pub reference: LeakageReference,
}

/// The cases on one side of the cutoff.
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct Side {
    pub n: usize,
    pub mean: f64,
}

fn side(scores: impl Iterator<Item = f64>) -> Side {
    let v: Vec<f64> = scores.collect();
    let mean = if v.is_empty() { f64::NAN } else { v.iter().sum::<f64>() / v.len() as f64 };
    Side { n: v.len(), mean }
}

/// Place each case by its resolution minus the reader's post-training cutoff end
/// (`assurance.backtest.cutoff-slice`).
pub fn cutoff_slice(cases: &[Scored], reader: &ModelRef, now: Instant, reference: LeakageReference) -> CutoffSlice {
    let end = reader.posttrain_end(now);
    let offset = |c: &Scored| (c.resolved.unix_nanos() - end.unix_nanos()) as f64 / 1e9 / DAY_SECS;
    CutoffSlice {
        offsets_days: cases.iter().map(offset).collect(),
        before: side(cases.iter().filter(|c| c.resolved < end).map(|c| c.score)),
        after: side(cases.iter().filter(|c| c.resolved >= end).map(|c| c.score)),
        reference,
    }
}

/// The two-sided 95 percent and 80 percent power normal quantiles behind the minimum
/// detectable effect.
const Z_ALPHA: f64 = 1.959_963_984_540_054;
const Z_POWER: f64 = 0.841_621_233_572_914;

/// The minimum detectable difference of two proportions near `p` with `n1` and `n2`
/// cases (`assurance.backtest.cutoff-sensitivity`).
pub fn minimum_detectable_effect(p: f64, n1: usize, n2: usize) -> f64 {
    if n1 == 0 || n2 == 0 {
        return f64::INFINITY;
    }
    let p = p.clamp(0.0, 1.0);
    (Z_ALPHA + Z_POWER) * (p * (1.0 - p) * (1.0 / n1 as f64 + 1.0 / n2 as f64)).sqrt()
}

/// The cutoff slice repeated with an `estimated` post-training cutoff moved by each of
/// `shifts_months` across its uncertainty interval; a `documented` cutoff is not moved.
pub fn cutoff_sensitivity(cases: &[Scored], reader: &ModelRef, now: Instant, shifts_months: &[i32], reference: &LeakageReference) -> Vec<(i32, CutoffSlice, f64)> {
    let estimated = reader.posttrain_cutoff.is_some_and(|c| c.tag == CutoffTag::Estimated);
    let shifts: Vec<i32> = if estimated { shifts_months.to_vec() } else { vec![0] };
    shifts
        .into_iter()
        .map(|shift| {
            let mut moved = reader.clone();
            if let Some(c) = moved.posttrain_cutoff.as_mut() {
                let months = i32::from(c.year) * 12 + i32::from(c.month) - 1 + shift;
                c.year = (months / 12) as u16;
                c.month = (months % 12 + 1) as u8;
            }
            let slice = cutoff_slice(cases, &moved, now, reference.clone());
            let all: Vec<f64> = cases.iter().map(|c| c.score).collect();
            let p = if all.is_empty() { 0.5 } else { all.iter().sum::<f64>() / all.len() as f64 };
            let mde = minimum_detectable_effect(p, slice.before.n, slice.after.n);
            (shift, slice, mde)
        })
        .collect()
}

/// The paired latest-minus-routed difference over cases scored under both arms
/// (`assurance.backtest.latest-arm`).
pub fn latest_premium(routed: &BTreeMap<String, f64>, latest: &BTreeMap<String, f64>) -> Option<f64> {
    let diffs: Vec<f64> = routed.iter().filter_map(|(case, r)| latest.get(case).map(|l| l - r)).collect();
    (!diffs.is_empty()).then(|| diffs.iter().sum::<f64>() / diffs.len() as f64)
}

/// What a backtest run block stamps beyond the judged cutoffs
/// (`assurance.backtest.run-stamp`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BacktestStamp {
    pub reader: ModelRef,
    pub judge: ModelRef,
    pub derive: ModelRef,
    pub embedder: ModelRef,
    pub temperature: f64,
    pub seed: u64,
    /// SHA-256 of the prompt template, hex.
    pub prompt_digest: String,
    pub tool_schema_version: String,
}

/// A case's universe (`assurance.backtest.universe-table`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Universe {
    /// A membership table declaring valid time and `available_time`.
    Table(String),
    /// Literal symbols, reported under the `fixed-universe` slice.
    Symbols(Vec<String>),
}

/// The slice tag a fixed universe reports under.
pub const FIXED_UNIVERSE: &str = "fixed-universe";

impl Universe {
    /// The separate slice a literal list reports under; `None` for a membership table,
    /// which must declare both clocks.
    pub fn slice(&self, tables: &[TableDecl]) -> Result<Option<&'static str>, String> {
        match self {
            Universe::Symbols(_) => Ok(Some(FIXED_UNIVERSE)),
            Universe::Table(name) => {
                let decl = tables.iter().find(|t| &t.name == name).ok_or_else(|| format!("universe table `{name}` is not declared"))?;
                if decl.valid_time.is_none() || decl.available_time.is_none() {
                    return Err(format!("universe table `{name}` declares valid time and `available_time`"));
                }
                Ok(None)
            }
        }
    }
}

/// One price as a fill reads it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Price {
    pub available: Instant,
    pub price: f64,
}

/// The first price available strictly after the decision (`assurance.backtest.fill-instant`).
pub fn fill(prices: &[Price], decided: Instant) -> Option<Price> {
    prices.iter().filter(|p| p.available > decided).min_by_key(|p| p.available).copied()
}

/// The declared component owning costs, slippage, sizing and liquidity caps
/// (`assurance.backtest.execution-component`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExecutionComponent {
    pub digest: String,
    pub parameters: BTreeMap<String, Value>,
}

/// The execution block a backtest report stamps, or `EvalExecutionUndeclared`.
pub fn execution_block(report: &str, component: Option<&ExecutionComponent>) -> Result<Value, EvalError> {
    let c = component.ok_or_else(|| EvalError::EvalExecutionUndeclared { report: report.to_string() })?;
    Ok(json!({ "digest": c.digest, "parameters": c.parameters }))
}

/// A return series' Sharpe ratio, per period.
pub fn sharpe(returns: &[f64]) -> f64 {
    let n = returns.len() as f64;
    if returns.len() < 2 {
        return f64::NAN;
    }
    let mean = returns.iter().sum::<f64>() / n;
    let var = returns.iter().map(|r| (r - mean).powi(2)).sum::<f64>() / (n - 1.0);
    mean / var.sqrt()
}

fn moments(returns: &[f64]) -> (f64, f64) {
    let n = returns.len() as f64;
    let mean = returns.iter().sum::<f64>() / n;
    let m2 = returns.iter().map(|r| (r - mean).powi(2)).sum::<f64>() / n;
    let m3 = returns.iter().map(|r| (r - mean).powi(3)).sum::<f64>() / n;
    let m4 = returns.iter().map(|r| (r - mean).powi(4)).sum::<f64>() / n;
    (m3 / m2.powf(1.5), m4 / (m2 * m2))
}

/// The standard normal distribution function.
pub fn normal_cdf(x: f64) -> f64 {
    0.5 * (1.0 + erf(x / std::f64::consts::SQRT_2))
}

fn erf(x: f64) -> f64 {
    // Abramowitz and Stegun 7.1.26.
    let t = 1.0 / (1.0 + 0.327_591_1 * x.abs());
    let y = 1.0 - (((((1.061_405_429 * t - 1.453_152_027) * t) + 1.421_413_741) * t - 0.284_496_736) * t + 0.254_829_592) * t * (-x * x).exp();
    if x >= 0.0 { y } else { -y }
}

/// The standard normal quantile (Acklam's rational approximation).
pub fn normal_quantile(p: f64) -> f64 {
    const A: [f64; 6] = [-3.969_683_028_665_376e1, 2.209_460_984_245_205e2, -2.759_285_104_469_687e2, 1.383_577_518_672_69e2, -3.066_479_806_614_716e1, 2.506_628_277_459_239];
    const B: [f64; 5] = [-5.447_609_879_822_406e1, 1.615_858_368_580_409e2, -1.556_989_798_598_866e2, 6.680_131_188_771_972e1, -1.328_068_155_288_572e1];
    const C: [f64; 6] = [-7.784_894_002_430_293e-3, -3.223_964_580_411_365e-1, -2.400_758_277_161_838, -2.549_732_539_343_734, 4.374_664_141_464_968, 2.938_163_982_698_783];
    const D: [f64; 4] = [7.784_695_709_041_462e-3, 3.224_671_290_700_398e-1, 2.445_134_137_142_996, 3.754_408_661_907_416];
    let low = 0.024_25;
    if p < low {
        let q = (-2.0 * p.ln()).sqrt();
        (((((C[0] * q + C[1]) * q + C[2]) * q + C[3]) * q + C[4]) * q + C[5]) / ((((D[0] * q + D[1]) * q + D[2]) * q + D[3]) * q + 1.0)
    } else if p <= 1.0 - low {
        let q = p - 0.5;
        let r = q * q;
        (((((A[0] * r + A[1]) * r + A[2]) * r + A[3]) * r + A[4]) * r + A[5]) * q / (((((B[0] * r + B[1]) * r + B[2]) * r + B[3]) * r + B[4]) * r + 1.0)
    } else {
        -normal_quantile(1.0 - p)
    }
}

const EULER_GAMMA: f64 = 0.577_215_664_901_532_9;

/// One configuration's runs against a case set: its repeats count as one trial
/// (`assurance.backtest.seed-repeats`).
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Trial {
    /// One return series per repeat, in run order.
    pub repeats: Vec<Vec<f64>>,
}

impl Trial {
    /// The trial's Sharpe ratio: the mean over its repeats.
    pub fn sharpe(&self) -> f64 {
        let s: Vec<f64> = self.repeats.iter().map(|r| sharpe(r)).collect();
        s.iter().sum::<f64>() / s.len() as f64
    }

    /// The repeats' Sharpe spread: least and greatest.
    pub fn spread(&self) -> (f64, f64) {
        let s = self.repeats.iter().map(|r| sharpe(r));
        let lo = s.clone().fold(f64::INFINITY, f64::min);
        (lo, s.fold(f64::NEG_INFINITY, f64::max))
    }
}

/// Every run against one case set, one trial row per configuration
/// (`assurance.backtest.trial-ledger`).
#[derive(Debug, Clone, PartialEq, Default)]
pub struct TrialLedger {
    pub trials: BTreeMap<String, Trial>,
}

impl TrialLedger {
    /// Land one run's return series under its configuration.
    pub fn land(&mut self, configuration: &str, returns: Vec<f64>) {
        self.trials.entry(configuration.to_string()).or_default().repeats.push(returns);
    }

    /// The deflated Sharpe ratio of `configuration`: the probability its Sharpe exceeds
    /// the expected maximum of as many trials under the ledger's Sharpe variance, given
    /// its series length, skew and kurtosis.
    pub fn deflated_sharpe(&self, configuration: &str) -> Option<f64> {
        let trial = self.trials.get(configuration)?;
        let series = trial.repeats.first()?;
        let n = self.trials.len() as f64;
        let sharpes: Vec<f64> = self.trials.values().map(Trial::sharpe).collect();
        let mean = sharpes.iter().sum::<f64>() / n;
        let variance = if sharpes.len() < 2 { 0.0 } else { sharpes.iter().map(|s| (s - mean).powi(2)).sum::<f64>() / (n - 1.0) };
        let expected_max = if n < 2.0 {
            0.0
        } else {
            variance.sqrt() * ((1.0 - EULER_GAMMA) * normal_quantile(1.0 - 1.0 / n) + EULER_GAMMA * normal_quantile(1.0 - 1.0 / (n * std::f64::consts::E)))
        };
        let sr = trial.sharpe();
        let (skew, kurtosis) = moments(series);
        let t = series.len() as f64;
        let denominator = (1.0 - skew * sr + (kurtosis - 1.0) / 4.0 * sr * sr).sqrt();
        Some(normal_cdf((sr - expected_max) * (t - 1.0).sqrt() / denominator))
    }
}

/// The audit attributes each trajectory call carries (`disclosure.record.bound-entry`).
pub const BOUND_ATTRIBUTES: [&str; 4] = ["contextful.bounds", "contextful.read.snapshots", "contextful.read.query", "contextful.result.sha256"];

/// A case's verdict over its recorded calls' audit attributes and its documents' leak
/// scores (`assurance.backtest.trajectory-check`): the reasons it fails, empty when it holds.
pub fn trajectory_check(entries: &[Value], leak_scores: &BTreeMap<String, u8>, threshold: u8) -> Vec<String> {
    let mut failures = Vec::new();
    for (i, entry) in entries.iter().enumerate() {
        if let Some(missing) = BOUND_ATTRIBUTES.iter().find(|k| entry.get(**k).is_none()) {
            failures.push(format!("call {i} lacks `{missing}`"));
        }
    }
    for (document, score) in leak_scores {
        if *score >= threshold {
            failures.push(format!("document `{document}` scores {score}, at or above the leak threshold {threshold}"));
        }
    }
    failures
}

/// A finding per recorded query naming an entity first available after the case's
/// horizon (`assurance.backtest.query-finding`).
pub fn query_findings(case: &BoundedCase, first_available: &BTreeMap<String, Instant>) -> Vec<String> {
    let mut findings = Vec::new();
    for (i, step) in case.trajectory.iter().enumerate() {
        let Some(query) = &step.query else { continue };
        let lowered = query.to_lowercase();
        let named: BTreeSet<&String> = first_available.iter().filter(|(e, at)| **at > case.horizon && lowered.contains(&e.to_lowercase())).map(|(e, _)| e).collect();
        for entity in named {
            findings.push(format!("call {i} (`{}`) names `{entity}`, first available after the horizon", step.tool));
        }
    }
    findings
}
