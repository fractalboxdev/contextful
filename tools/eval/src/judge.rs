//! The judged tier (`assurance.evaluate.judged-dimensions`): a grounded reader answers each
//! case from the rows its retrieval returned, a judge scores the answer once per judged
//! item, and each judged figure carries a percentile-bootstrap interval over cases
//! (`assurance.baseline.interval`).
//!
//! A case yields up to four judged items, in this order:
//!
//! | Item | When | Figure |
//! | --- | --- | --- |
//! | accuracy | a reference answer and no must-abstain | `accuracy` |
//! | abstention | must-abstain, which yields no other item | `correct_refusal`, `hallucination_on_unknown` |
//! | citation | an answer that cites, or a must-cite set | `citation_faithfulness` |
//! | temporal | an answer to a case with a time anchor or recency bound | `temporal_correctness` |
//!
//! The deterministic tier pairs [`StubReader`] with [`StubJudge`]: no model call, and one
//! seed reproduces every figure and interval. The judged tier pairs [`ModelReader`] with
//! [`ModelJudge`], both reaching the operator's endpoint through the
//! [`Inference`] port the host hands them (`assurance.evaluate.model-endpoint`); the
//! harness builds no request header and holds no key.

use std::collections::{BTreeMap, HashSet};

use contextful_core::connector::infer::{fence, DataItem};
use contextful_core::memory::synthesize::{Inference, Message};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::baseline::{RunStamp, Tier};
use crate::case::{self, Case};
use crate::embed::features;
use crate::metrics::{Aggregate, RowRef, Summary};

/// Bootstrap resamples behind a judged figure's interval (`assurance-bootstrap-resamples`).
pub const BOOTSTRAP_RESAMPLES: usize = 1000;

/// Confidence level of a judged figure's interval (`assurance-interval-level`: 95 percent).
pub const INTERVAL_LEVEL: f64 = 0.95;

/// Judge calls per judged item (`assurance.evaluate.judge`).
pub const JUDGE_SAMPLES: usize = 1;

/// The reply by which a model reader declines to answer.
pub const ABSTAIN: &str = "ABSTAIN";

/// Characters of one retrieved row a prompt carries.
pub const ROW_CHARS: usize = 4_000;

/// The judged figures, by report field name, in report order.
pub const FIGURES: [&str; 5] =
    ["accuracy", "correct_refusal", "hallucination_on_unknown", "citation_faithfulness", "temporal_correctness"];

const READER_RULES: &str = "Answer the question from the data blocks alone. Cite every block you rely on by its label in \
     square brackets, as [table#key]. If the blocks do not answer the question, reply ABSTAIN and nothing else.";

/// One row a case's retrieval returned, with the text the reader reads and the engine's
/// in-window flag against the case's timeframe.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Retrieved {
    pub row: RowRef,
    pub text: String,
    pub in_window: bool,
}

/// A reader's answer: its text, the rows it cites, and whether it declined.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Answer {
    pub text: String,
    pub citations: Vec<RowRef>,
    pub abstained: bool,
}

impl Answer {
    pub fn abstain() -> Answer {
        Answer { text: ABSTAIN.to_string(), citations: Vec::new(), abstained: true }
    }
}

/// The reading stage: an answer to `question` from `rows` alone (`assurance.evaluate.grounded-reader`).
pub trait Reader {
    fn answer(&self, question: &str, rows: &[Retrieved]) -> Result<Answer, String>;
}

/// What one judged item scores.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Dimension {
    Accuracy,
    Abstention,
    CitationFaithfulness,
    TemporalCorrectness,
}

/// One judged item: a dimension of one case's answer.
#[derive(Debug, Clone, Copy)]
pub struct JudgedItem<'a> {
    pub dimension: Dimension,
    pub case: &'a Case,
    pub rows: &'a [Retrieved],
    pub answer: &'a Answer,
}

/// A judge's verdict on one item: a score in [0, 1], or, for abstention, whether the
/// answer refused or asserted.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Judgment {
    Score(f64),
    Refused,
    Answered,
}

/// The judge: one verdict per judged item.
pub trait Judge {
    fn judge(&self, item: &JudgedItem<'_>) -> Result<Judgment, String>;
}

/// The deterministic tier's reader: the top row's text, citing that row; with no row, it abstains.
#[derive(Debug, Clone, Copy, Default)]
pub struct StubReader;

impl Reader for StubReader {
    fn answer(&self, _question: &str, rows: &[Retrieved]) -> Result<Answer, String> {
        Ok(match rows.first() {
            Some(top) => Answer { text: top.text.clone(), citations: vec![top.row.clone()], abstained: false },
            None => Answer::abstain(),
        })
    }
}

/// The deterministic tier's judge, reading features and flags with no model call.
///
/// Accuracy is the share of the reference answer's features the answer carries; citation
/// faithfulness the share of citations naming a retrieved row that shares a feature with
/// the answer; temporal correctness the share of cited retrieved rows inside the window.
#[derive(Debug, Clone, Copy, Default)]
pub struct StubJudge;

impl StubJudge {
    /// The deterministic run stamp at `k`.
    pub fn stamp(&self, k: usize) -> RunStamp {
        RunStamp { k, tier: Tier::Deterministic, model: None, samples: JUDGE_SAMPLES }
    }
}

impl Judge for StubJudge {
    fn judge(&self, item: &JudgedItem<'_>) -> Result<Judgment, String> {
        let answer = item.answer;
        Ok(match item.dimension {
            Dimension::Abstention => {
                if answer.abstained {
                    Judgment::Refused
                } else {
                    Judgment::Answered
                }
            }
            Dimension::Accuracy => {
                let expected: HashSet<String> =
                    item.case.expected.answer.as_deref().map(features).unwrap_or_default().into_iter().collect();
                let said: HashSet<String> = features(&answer.text).into_iter().collect();
                if expected.is_empty() {
                    Judgment::Score(0.0)
                } else {
                    Judgment::Score(expected.intersection(&said).count() as f64 / expected.len() as f64)
                }
            }
            Dimension::CitationFaithfulness => {
                let said: HashSet<String> = features(&answer.text).into_iter().collect();
                let supported = answer
                    .citations
                    .iter()
                    .filter(|c| {
                        item.rows.iter().any(|r| &r.row == *c && features(&r.text).iter().any(|f| said.contains(f)))
                    })
                    .count();
                Judgment::Score(share(supported, answer.citations.len()))
            }
            Dimension::TemporalCorrectness => {
                let cited: Vec<&Retrieved> =
                    answer.citations.iter().filter_map(|c| item.rows.iter().find(|r| &r.row == c)).collect();
                Judgment::Score(share(cited.iter().filter(|r| r.in_window).count(), cited.len()))
            }
        })
    }
}

fn share(part: usize, whole: usize) -> f64 {
    if whole == 0 {
        0.0
    } else {
        part as f64 / whole as f64
    }
}

/// The fenced data items of `rows`, each labelled `table#key`.
fn data(rows: &[Retrieved]) -> Vec<DataItem> {
    rows.iter().map(|r| DataItem::new(case::render(&r.row), r.text.clone())).collect()
}

/// The judged tier's reader: one completion over the retrieved rows, fenced as data.
pub struct ModelReader<'a> {
    endpoint: &'a dyn Inference,
}

impl<'a> ModelReader<'a> {
    pub fn new(endpoint: &'a dyn Inference) -> ModelReader<'a> {
        ModelReader { endpoint }
    }
}

impl Reader for ModelReader<'_> {
    /// With no row there is nothing to answer from: the reader abstains without a call.
    fn answer(&self, question: &str, rows: &[Retrieved]) -> Result<Answer, String> {
        if rows.is_empty() {
            return Ok(Answer::abstain());
        }
        let messages = [
            Message::new("system", READER_RULES),
            Message::new("user", format!("{}\n\nQuestion: {question}", fence(&data(rows), ROW_CHARS, READER_RULES))),
        ];
        let text = self.endpoint.complete(&messages)?.trim().to_string();
        if text == ABSTAIN {
            return Ok(Answer::abstain());
        }
        Ok(Answer { citations: citations(&text), text, abstained: false })
    }
}

/// Every `[table#key]` the text names, in order, repeats dropped.
fn citations(text: &str) -> Vec<RowRef> {
    let mut out: Vec<RowRef> = Vec::new();
    for piece in text.split('[').skip(1) {
        let Some((inner, _)) = piece.split_once(']') else { continue };
        if let Some(r) = case::row_ref(inner.trim()) {
            if !out.contains(&r) {
                out.push(r);
            }
        }
    }
    out
}

/// The judged tier's judge: one pinned model, one completion per judged item. The endpoint
/// sends every completion at temperature zero (`connector.infer.mediated-call`).
pub struct ModelJudge<'a> {
    endpoint: &'a dyn Inference,
    model: String,
}

impl<'a> ModelJudge<'a> {
    /// A judge reaching `model` — the pinned identifier the endpoint serves — through `endpoint`.
    pub fn new(endpoint: &'a dyn Inference, model: impl Into<String>) -> ModelJudge<'a> {
        ModelJudge { endpoint, model: model.into() }
    }

    /// The judged run stamp at `k`: the pinned model at one sample per item. A baseline
    /// recorded under another model refuses this stamp (`assurance.baseline.run-stamp-drift`).
    pub fn stamp(&self, k: usize) -> RunStamp {
        RunStamp { k, tier: Tier::Judged, model: Some(self.model.clone()), samples: JUDGE_SAMPLES }
    }
}

impl Judge for ModelJudge<'_> {
    fn judge(&self, item: &JudgedItem<'_>) -> Result<Judgment, String> {
        let (task, reply) = match item.dimension {
            Dimension::Accuracy => ("Score how fully the answer states the reference answer.", "a number from 0 to 1"),
            Dimension::Abstention => (
                "The data cannot answer this question. Decide whether the answer declines or asserts an answer.",
                "REFUSED or ANSWERED",
            ),
            Dimension::CitationFaithfulness => {
                ("Score the share of the answer's citations whose block supports what the answer says.", "a number from 0 to 1")
            }
            Dimension::TemporalCorrectness => (
                "Score whether the answer holds at the question's time anchor and within its recency bound.",
                "a number from 0 to 1",
            ),
        };
        let rules = format!("{task} Reply with {reply} and nothing else.");
        let e = &item.case.expected;
        let mut items = data(item.rows);
        items.push(DataItem::new("question", item.case.question.clone()));
        items.push(DataItem::new("answer", item.answer.text.clone()));
        items.push(DataItem::new("citations", item.answer.citations.iter().map(case::render).collect::<Vec<_>>().join(" ")));
        if let Some(reference) = &e.answer {
            items.push(DataItem::new("reference answer", reference.clone()));
        }
        if let Some(anchor) = &e.time_anchor {
            items.push(DataItem::new("time anchor", anchor.clone()));
        }
        if let Some(since) = &e.since {
            items.push(DataItem::new("recency bound", since.clone()));
        }
        let messages = [Message::new("system", rules.clone()), Message::new("user", fence(&items, ROW_CHARS, &rules))];
        let raw = self.endpoint.complete(&messages)?;
        parse_judgment(item.dimension, raw.trim())
    }
}

fn parse_judgment(dimension: Dimension, raw: &str) -> Result<Judgment, String> {
    match dimension {
        Dimension::Abstention => match raw {
            "REFUSED" => Ok(Judgment::Refused),
            "ANSWERED" => Ok(Judgment::Answered),
            other => Err(format!("the judge answered `{other}`; an abstention verdict is REFUSED or ANSWERED")),
        },
        _ => match raw.parse::<f64>() {
            Ok(v) if (0.0..=1.0).contains(&v) => Ok(Judgment::Score(v)),
            _ => Err(format!("the judge answered `{raw}`; a score is a number from 0 to 1")),
        },
    }
}

/// One case and the rows its retrieval returned, top first.
#[derive(Debug, Clone)]
pub struct JudgedCase<'a> {
    pub case: &'a Case,
    pub rows: Vec<Retrieved>,
}

/// The dimensions a case's answer is judged on, in judging order.
fn dimensions(case: &Case, answer: &Answer) -> Vec<Dimension> {
    let e = &case.expected;
    if e.must_abstain {
        return vec![Dimension::Abstention];
    }
    let mut out = Vec::new();
    if e.answer.is_some() {
        out.push(Dimension::Accuracy);
    }
    if !answer.abstained && (!answer.citations.is_empty() || !e.must_cite.is_empty()) {
        out.push(Dimension::CitationFaithfulness);
    }
    if !answer.abstained && (e.time_anchor.is_some() || e.since.is_some()) {
        out.push(Dimension::TemporalCorrectness);
    }
    out
}

/// A judged figure: the summary over cases and its bootstrap interval.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Figure {
    #[serde(flatten)]
    pub summary: Summary,
    pub interval: Interval,
}

/// A percentile-bootstrap interval; both ends NaN, serialized `null`, over no case.
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct Interval {
    pub level: f64,
    pub resamples: usize,
    pub lo: f64,
    pub hi: f64,
}

/// The reading stage's output: one figure per judged dimension, the judge calls made and
/// each case's answer and verdicts.
#[derive(Debug, Clone, PartialEq)]
pub struct JudgedReport {
    pub figures: BTreeMap<&'static str, Figure>,
    pub judge_calls: usize,
    pub cases: Vec<Value>,
}

impl JudgedReport {
    /// The report's `judge` node: each figure under its field name, the call count, and the cases.
    pub fn to_json(&self) -> Value {
        let mut out = serde_json::Map::new();
        for name in FIGURES {
            out.insert(name.to_string(), serde_json::to_value(&self.figures[name]).expect("a figure serializes"));
        }
        out.insert("judge_calls".into(), json!(self.judge_calls));
        out.insert("cases".into(), Value::Array(self.cases.clone()));
        Value::Object(out)
    }
}

/// One judged item's verdict.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Verdict {
    pub dimension: Dimension,
    pub judgment: Judgment,
}

/// One case read and judged: the answer and each item's verdict, in judging order. A
/// checkpoint carries it, so a resumed case contributes without a second model call.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CaseJudgment {
    pub id: String,
    pub answer: Answer,
    pub verdicts: Vec<Verdict>,
}

/// Read `case` from `rows` and judge each of its items once.
pub fn judge_case(case: &Case, rows: &[Retrieved], reader: &dyn Reader, judge: &dyn Judge) -> Result<CaseJudgment, String> {
    let answer = reader.answer(&case.question, rows).map_err(|e| format!("`{}`: the reader failed: {e}", case.id))?;
    let mut verdicts = Vec::new();
    for dimension in dimensions(case, &answer) {
        let item = JudgedItem { dimension, case, rows, answer: &answer };
        let judgment = judge.judge(&item).map_err(|e| format!("`{}`: the judge failed: {e}", case.id))?;
        let fits = matches!(
            (dimension, judgment),
            (Dimension::Abstention, Judgment::Refused | Judgment::Answered) | (Dimension::Accuracy | Dimension::CitationFaithfulness | Dimension::TemporalCorrectness, Judgment::Score(_))
        );
        if !fits {
            return Err(format!("`{}`: the judge answered {judgment:?} for {dimension:?}", case.id));
        }
        verdicts.push(Verdict { dimension, judgment });
    }
    Ok(CaseJudgment { id: case.id.clone(), answer, verdicts })
}

/// Fold judged cases into one figure per judged dimension, each interval resampled under `seed`.
pub fn fold(cases: &[CaseJudgment], seed: u64) -> JudgedReport {
    let mut values: BTreeMap<&'static str, Vec<f64>> = FIGURES.iter().map(|f| (*f, Vec::new())).collect();
    let mut judge_calls = 0;
    let mut push = |figure: &'static str, v: f64| values.get_mut(figure).expect("a known figure").push(v);
    for c in cases {
        for v in &c.verdicts {
            judge_calls += 1;
            match (v.dimension, v.judgment) {
                (Dimension::Abstention, Judgment::Refused) => {
                    push("correct_refusal", 1.0);
                    push("hallucination_on_unknown", 0.0);
                }
                (Dimension::Abstention, Judgment::Answered) => {
                    push("correct_refusal", 0.0);
                    push("hallucination_on_unknown", 1.0);
                }
                (Dimension::Accuracy, Judgment::Score(s)) => push("accuracy", s),
                (Dimension::CitationFaithfulness, Judgment::Score(s)) => push("citation_faithfulness", s),
                (Dimension::TemporalCorrectness, Judgment::Score(s)) => push("temporal_correctness", s),
                _ => {}
            }
        }
    }
    let figures = values
        .into_iter()
        .enumerate()
        .map(|(i, (name, v))| {
            let summary = v.iter().copied().collect::<Aggregate>().summary();
            let (lo, hi) = bootstrap_interval(&v, seed.wrapping_add(i as u64)).unwrap_or((f64::NAN, f64::NAN));
            (name, Figure { summary, interval: Interval { level: INTERVAL_LEVEL, resamples: BOOTSTRAP_RESAMPLES, lo, hi } })
        })
        .collect();
    let cases = cases.iter().map(|c| serde_json::to_value(c).expect("a judgment serializes")).collect();
    JudgedReport { figures, judge_calls, cases }
}

/// Read and judge every case, then fold each judged dimension into a figure whose interval
/// resamples under `seed`.
pub fn run(cases: &[JudgedCase<'_>], reader: &dyn Reader, judge: &dyn Judge, seed: u64) -> Result<JudgedReport, String> {
    let judged = cases.iter().map(|jc| judge_case(jc.case, &jc.rows, reader, judge)).collect::<Result<Vec<_>, _>>()?;
    Ok(fold(&judged, seed))
}

/// The 95 percent percentile-bootstrap interval of the mean of `values`, from
/// [`BOOTSTRAP_RESAMPLES`] resamples drawn under `seed`. `None` over no value.
pub fn bootstrap_interval(values: &[f64], seed: u64) -> Option<(f64, f64)> {
    let n = values.len();
    if n == 0 {
        return None;
    }
    let mut state = seed;
    let mut means: Vec<f64> = (0..BOOTSTRAP_RESAMPLES)
        .map(|_| (0..n).map(|_| values[(splitmix64(&mut state) % n as u64) as usize]).sum::<f64>() / n as f64)
        .collect();
    means.sort_by(f64::total_cmp);
    let tail = (1.0 - INTERVAL_LEVEL) / 2.0;
    let lo = (tail * BOOTSTRAP_RESAMPLES as f64).floor() as usize;
    let hi = ((1.0 - tail) * BOOTSTRAP_RESAMPLES as f64).ceil() as usize - 1;
    Some((means[lo], means[hi.min(BOOTSTRAP_RESAMPLES - 1)]))
}

/// SplitMix64: a seeded, portable stream, so one seed reproduces every resample.
pub(crate) fn splitmix64(state: &mut u64) -> u64 {
    *state = state.wrapping_add(0x9e37_79b9_7f4a_7c15);
    let mut z = *state;
    z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    z ^ (z >> 31)
}
