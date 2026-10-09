//! The judged tier: the grounded reader, the judge, the four judged dimensions and the
//! bootstrap interval behind each judged figure.

use std::cell::RefCell;

use contextful_core::memory::synthesize::{Inference, Message};
use contextful_eval::baseline::{gate, Baselines, RunStamp, Tier};
use contextful_eval::case::{self, Case};
use contextful_eval::judge::{
    bootstrap_interval, run, Answer, Dimension, Judge, JudgedCase, JudgedItem, Judgment, ModelJudge, ModelReader, Reader,
    Retrieved, StubJudge, StubReader, ABSTAIN, BOOTSTRAP_RESAMPLES, JUDGE_SAMPLES,
};
use contextful_eval::metrics::RowRef;
use serde_json::json;

/// An inference endpoint answering from a script and recording every conversation.
struct Recording {
    replies: RefCell<Vec<String>>,
    calls: RefCell<Vec<Vec<Message>>>,
}

impl Recording {
    fn new(replies: &[&str]) -> Recording {
        Recording { replies: RefCell::new(replies.iter().rev().map(|r| r.to_string()).collect()), calls: RefCell::new(Vec::new()) }
    }

    fn calls(&self) -> usize {
        self.calls.borrow().len()
    }
}

impl Inference for Recording {
    fn complete(&self, messages: &[Message]) -> Result<String, String> {
        self.calls.borrow_mut().push(messages.to_vec());
        self.replies.borrow_mut().pop().ok_or_else(|| "the script ran out".to_string())
    }
}

fn case(line: serde_json::Value) -> Case {
    case::load(&line.to_string()).unwrap().remove(0)
}

fn row(key: &str, text: &str, in_window: bool) -> Retrieved {
    Retrieved { row: RowRef::new("notes", key), text: text.to_string(), in_window }
}

/// A case set exercising every judged dimension.
fn cases() -> Vec<Case> {
    vec![
        case(json!({ "id": "c1", "corpus": "c", "question": "Which city hosts the archive?",
            "expected": { "answer": "The archive sits in Lisbon", "artifacts": ["notes#1"], "must_cite": ["notes#1"] } })),
        case(json!({ "id": "c2", "corpus": "c", "question": "Who founded the archive?",
            "expected": { "answer": "Ada Varga founded it", "artifacts": ["notes#2"], "since": "2024-01-01T00:00:00Z" } })),
        case(json!({ "id": "c3", "corpus": "c", "question": "What is the archive's phone number?",
            "expected": { "must_abstain": true } })),
        case(json!({ "id": "c4", "corpus": "c", "question": "Who audits the vault?",
            "expected": { "must_abstain": true } })),
    ]
}

fn retrieved() -> Vec<Vec<Retrieved>> {
    vec![
        vec![row("1", "The archive sits in Lisbon beside the river", true)],
        vec![row("2", "Ada Varga founded the archive in 1990", false), row("9", "Unrelated minutes", true)],
        vec![],
        vec![row("7", "The vault opened in spring", true)],
    ]
}

fn judged<'a>(cases: &'a [Case], rows: &[Vec<Retrieved>]) -> Vec<JudgedCase<'a>> {
    cases.iter().zip(rows).map(|(case, rows)| JudgedCase { case, rows: rows.clone() }).collect()
}

/// The reading stage adds four judged dimensions: answer accuracy; abstention, as correct-refusal and
/// hallucination-on-unknown rates; citation faithfulness; and temporal correctness against the bitemporal columns.
// spec: assurance.evaluate.judged-dimensions@47a873b0
#[test]
fn the_reading_stage_reports_four_judged_dimensions() {
    let cases = cases();
    let report = run(&judged(&cases, &retrieved()), &StubReader, &StubJudge, 7).unwrap();
    let v = report.to_json();
    for dim in ["accuracy", "correct_refusal", "hallucination_on_unknown", "citation_faithfulness", "temporal_correctness"] {
        assert!(v[dim]["n"].as_u64().unwrap() > 0, "{dim}: {v}");
        assert!(v[dim]["interval"]["lo"].is_number(), "{dim}: {v}");
    }
    // Two answerable cases carry a reference answer; two must-abstain cases score abstention.
    assert_eq!(v["accuracy"]["n"], 2);
    assert_eq!(v["correct_refusal"]["n"], 2);
    assert_eq!(v["hallucination_on_unknown"]["n"], 2);
    // c3 retrieved nothing and refused; c4 answered from an unrelated row: one of each.
    assert_eq!(v["correct_refusal"]["mean"], 0.5);
    assert_eq!(v["hallucination_on_unknown"]["mean"], 0.5);
    // c2 cites a row outside its publication window: the temporal figure reads it.
    assert_eq!(v["temporal_correctness"]["n"], 1);
    assert_eq!(v["temporal_correctness"]["mean"], 0.0);
    // Each judged figure sits at its baseline path: its sample count resolves, and its mean
    // answers to the gated-mean floor rather than to a missing field.
    let r = json!({ "run": { "k": 10, "tier": "deterministic", "model": null, "samples": 1 }, "judge": v });
    assert_eq!(contextful_eval::baseline::resolve(&r, "judge.correct_refusal.n").unwrap(), 2.0);
    let e = contextful_eval::baseline::resolve(&r, "judge.accuracy").unwrap_err();
    assert!(e.to_string().contains("below 30"), "{e}");
}

/// The harness reader answers from retrieved rows alone, and the deterministic tier substitutes a stub reader.
// spec: assurance.evaluate.grounded-reader@f3b52d0e
#[test]
fn the_reader_answers_from_retrieved_rows_alone() {
    let q = "Which city hosts the archive?";
    let rows = [row("1", "The archive sits in Lisbon", true)];
    // The stub reads its answer and citation off the rows it was handed.
    let a = StubReader.answer(q, &rows).unwrap();
    assert!(!a.abstained);
    assert_eq!(a.text, "The archive sits in Lisbon");
    assert_eq!(a.citations, vec![RowRef::new("notes", "1")]);
    assert!(StubReader.answer(q, &[]).unwrap().abstained);

    // The model reader sends only the rows, fenced as data, and makes no call with none.
    let endpoint = Recording::new(&["Lisbon [notes#1] [notes#5]"]);
    let reader = ModelReader::new(&endpoint);
    let none = reader.answer(q, &[]).unwrap();
    assert!(none.abstained);
    assert_eq!(endpoint.calls(), 0);
    let a = reader.answer(q, &rows).unwrap();
    assert_eq!(endpoint.calls(), 1);
    let sent: String = endpoint.calls.borrow()[0].iter().map(|m| m.content.clone()).collect();
    assert!(sent.contains("The archive sits in Lisbon"), "{sent}");
    assert!(sent.contains(q));
    // A citation naming a row the reader was not handed is kept, for the judge to score.
    assert_eq!(a.citations, vec![RowRef::new("notes", "1"), RowRef::new("notes", "5")]);
    let endpoint = Recording::new(&[ABSTAIN]);
    assert!(ModelReader::new(&endpoint).answer(q, &rows).unwrap().abstained);
}

/// The judge and the reader reach a model through {{connector.infer.model-endpoint}}, and the harness holds no
/// credential.
// spec: assurance.evaluate.model-endpoint@62693da4
#[test]
fn the_reader_and_judge_reach_the_model_through_the_inference_port() {
    let cases = cases();
    // Case by case: the reader's answer, then one judgment per judged item.
    let endpoint = Recording::new(&[
        "The archive sits in Lisbon [notes#1]",
        "1",
        "1",
        "Ada Varga [notes#2]",
        "1",
        "0.5",
        "0",
        "REFUSED",
        "The vault opened in spring [notes#7]",
        "ANSWERED",
    ]);
    let reader = ModelReader::new(&endpoint);
    let judge = ModelJudge::new(&endpoint, "open-instruct-7b@sha256:ab12");
    let report = run(&judged(&cases, &retrieved()), &reader, &judge, 7).unwrap();
    // c3 retrieved nothing, so the reader made three calls and the judge seven.
    assert_eq!(endpoint.calls(), 10);
    assert_eq!(report.judge_calls, 7);
    let v = report.to_json();
    assert_eq!(v["accuracy"]["mean"], 1.0);
    assert_eq!(v["correct_refusal"]["mean"], 0.5);
    // Every byte the harness sends is a role and a content: it carries no header or key.
    for call in endpoint.calls.borrow().iter() {
        for m in call {
            assert!(m.role == "system" || m.role == "user", "{}", m.role);
        }
    }
    // An unparseable judgment fails the run rather than scoring.
    let endpoint = Recording::new(&["Lisbon [notes#1]", "maybe"]);
    let one = &cases[..1];
    let e = run(&judged(one, &retrieved()[..1]), &ModelReader::new(&endpoint), &ModelJudge::new(&endpoint, "m"), 7);
    assert!(e.is_err());
}

/// A judge that counts its calls.
struct Counting<'a> {
    inner: &'a dyn Judge,
    calls: RefCell<Vec<Dimension>>,
}

impl Judge for Counting<'_> {
    fn judge(&self, item: &JudgedItem<'_>) -> Result<Judgment, String> {
        self.calls.borrow_mut().push(item.dimension);
        self.inner.judge(item)
    }
}

/// The judge is one pinned open-weights instruct model, called once per judged item at temperature zero; a judge
/// swap is a full re-baseline.
// spec: assurance.evaluate.judge@d60b19b7
#[test]
fn the_judge_is_pinned_and_called_once_per_item() {
    let cases = cases();
    let counting = Counting { inner: &StubJudge, calls: RefCell::new(Vec::new()) };
    let report = run(&judged(&cases, &retrieved()), &StubReader, &counting, 7).unwrap();
    let calls = counting.calls.borrow();
    assert_eq!(calls.len(), report.judge_calls);
    // c1: accuracy, citation; c2: accuracy, citation, temporal; c3: abstention; c4: abstention.
    assert_eq!(calls.len(), 7);
    assert_eq!(calls.iter().filter(|d| **d == Dimension::Abstention).count(), 2);

    // The pinned model names the run stamp, at one sample per item.
    let endpoint = Recording::new(&[]);
    let judge = ModelJudge::new(&endpoint, "open-instruct-7b@sha256:ab12");
    let stamp = judge.stamp(10);
    assert_eq!(stamp, RunStamp { k: 10, tier: Tier::Judged, model: Some("open-instruct-7b@sha256:ab12".into()), samples: JUDGE_SAMPLES });
    assert_eq!(JUDGE_SAMPLES, 1);
    assert_eq!(StubJudge.stamp(10).tier, Tier::Deterministic);

    // A baseline recorded under one judge refuses a run under another.
    let baseline = Baselines::parse(
        &json!({ "_run": { "k": 10, "tier": "judged", "model": "open-instruct-7b@sha256:ab12", "samples": 1 } }).to_string(),
    )
    .unwrap();
    assert!(baseline.check_run(&stamp).is_ok());
    let swapped = ModelJudge::new(&endpoint, "open-instruct-8b@sha256:cd34").stamp(10);
    assert_eq!(baseline.check_run(&swapped).unwrap_err().code(), "BaselineRunStampMismatch");
    let report = json!({ "run": swapped, "n_cases": 0 });
    assert_eq!(gate(&report, &baseline).unwrap_err().code(), "BaselineRunStampMismatch");
}

/// Each judged figure reports a 95 percent percentile-bootstrap interval over cases from 1000 resamples.
// spec: assurance.baseline.interval@4c0ef2d3
#[test]
fn each_judged_figure_carries_a_seeded_bootstrap_interval() {
    assert_eq!(BOOTSTRAP_RESAMPLES, 1000);
    let values: Vec<f64> = (0..40).map(|i| if i % 4 == 0 { 0.0 } else { 1.0 }).collect();
    let (lo, hi) = bootstrap_interval(&values, 11).unwrap();
    assert!(lo < 0.75 && 0.75 < hi, "{lo}..{hi}");
    assert!(lo >= 0.5 && hi <= 0.95, "{lo}..{hi}");
    // One seed reproduces the interval; a constant figure has a degenerate one.
    assert_eq!(bootstrap_interval(&values, 11), Some((lo, hi)));
    assert_eq!(bootstrap_interval(&[1.0; 12], 3), Some((1.0, 1.0)));
    assert_eq!(bootstrap_interval(&[], 3), None);
    // A wider sample narrows the interval.
    let wide: Vec<f64> = (0..400).map(|i| if i % 4 == 0 { 0.0 } else { 1.0 }).collect();
    let (wlo, whi) = bootstrap_interval(&wide, 11).unwrap();
    assert!(whi - wlo < hi - lo, "{wlo}..{whi} vs {lo}..{hi}");
}

#[test]
fn the_deterministic_tier_reports_identical_figures_across_two_runs() {
    let cases = cases();
    let first = run(&judged(&cases, &retrieved()), &StubReader, &StubJudge, 7).unwrap().to_json();
    let second = run(&judged(&cases, &retrieved()), &StubReader, &StubJudge, 7).unwrap().to_json();
    assert_eq!(first, second);
    assert_eq!(serde_json::to_string(&first).unwrap(), serde_json::to_string(&second).unwrap());
}

#[test]
fn an_abstained_answer_cites_nothing() {
    let a = Answer::abstain();
    assert!(a.abstained && a.citations.is_empty());
}
