//! Growing and renewing the native golden set: three generators draft candidate cases from
//! the deployment's store (`assurance.baseline.generators`), a reviewer approves each, an
//! adjudicated outcome label promotes (`assurance.baseline.promotion-source`), every golden
//! passes write-time redaction before commit (`assurance.baseline.generated-redaction`),
//! and the set rotates on a cadence with a reviewed sample (`assurance.baseline.rotation`).
//!
//! A committed golden reaches the gate only as a change to `evals/cases/`
//! (`assurance.baseline.golden-custody`); nothing here writes the tree.

use std::collections::{BTreeMap, BTreeSet};

use contextful_core::enforce::EnforceError;
use contextful_core::redaction::{CompiledRule, Matcher, Operation, Rule};
use serde_json::Value;

use crate::baseline::{RunStamp, REPORT_RUN_KEY};
use crate::case::{self, Case, Expected};
use crate::error::EvalError;
use crate::judge::splitmix64;
use crate::metrics::RowRef;

/// The tag every generated case carries, beside its generator's name.
pub const GENERATED_TAG: &str = "generated";

/// A named thing in the store.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entity {
    pub id: String,
    pub name: String,
}

/// One attribute of an entity and the row stating it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Fact {
    pub entity: String,
    pub attribute: String,
    pub value: String,
    pub row: RowRef,
}

/// A directed relation between two entities and the row stating it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Edge {
    pub from: String,
    pub relation: String,
    pub to: String,
    pub row: RowRef,
}

/// The entities, facts and edges a deployment's store holds, as the generators read them.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct StoreView {
    pub entities: Vec<Entity>,
    pub facts: Vec<Fact>,
    pub edges: Vec<Edge>,
}

impl StoreView {
    fn name(&self, id: &str) -> Option<&str> {
        self.entities.iter().find(|e| e.id == id).map(|e| e.name.as_str())
    }
}

/// The generator that drafted a candidate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Generator {
    /// Entity to fact to edge.
    Walk,
    /// Two composed edges.
    Chain,
    /// A question about an entity the store does not hold.
    Absent,
}

impl Generator {
    pub fn as_str(self) -> &'static str {
        match self {
            Generator::Walk => "walk",
            Generator::Chain => "chain",
            Generator::Absent => "absent",
        }
    }
}

/// A drafted case awaiting a reviewer.
#[derive(Debug, Clone, PartialEq)]
pub struct Candidate {
    pub generator: Generator,
    pub case: Case,
}

fn draft(generator: Generator, id: String, corpus: &str, question: String, expected: Expected) -> Candidate {
    let case = Case {
        id: format!("gen-{}-{id}", generator.as_str()),
        corpus: corpus.to_string(),
        question,
        tags: vec![GENERATED_TAG.to_string(), generator.as_str().to_string()],
        prefix: None,
        query_embedding: None,
        expected,
    };
    Candidate { generator, case }
}

/// Entity-to-fact-to-edge walks: for each fact and each edge leaving its entity, a question
/// whose truth is the fact's row and the edge's row.
pub fn walks(store: &StoreView, corpus: &str) -> Vec<Candidate> {
    let mut out = Vec::new();
    for fact in &store.facts {
        let Some(name) = store.name(&fact.entity) else { continue };
        for edge in store.edges.iter().filter(|e| e.from == fact.entity) {
            let Some(target) = store.name(&edge.to) else { continue };
            let expected = Expected {
                answer: Some(format!("{name}'s {} is {}, and {name} {} {target}", fact.attribute, fact.value, edge.relation)),
                artifacts: vec![case::render(&fact.row)],
                edges: vec![case::render(&edge.row)],
                entities: vec![name.to_string(), target.to_string()],
                ..Expected::default()
            };
            let question = format!("What is the {} of {name}, and what does {name} {}?", fact.attribute, edge.relation);
            out.push(draft(Generator::Walk, format!("{}-{}", case::render(&fact.row), case::render(&edge.row)), corpus, question, expected));
        }
    }
    out
}

/// Composed edge chains: for each pair of edges meeting at one entity, a question whose
/// answer is the far end and whose truth is both edges.
pub fn chains(store: &StoreView, corpus: &str) -> Vec<Candidate> {
    let mut out = Vec::new();
    for first in &store.edges {
        for second in store.edges.iter().filter(|e| e.from == first.to && e.to != first.from) {
            let (Some(a), Some(b), Some(c)) = (store.name(&first.from), store.name(&first.to), store.name(&second.to)) else {
                continue;
            };
            let expected = Expected {
                answer: Some(c.to_string()),
                edges: vec![case::render(&first.row), case::render(&second.row)],
                entities: vec![a.to_string(), b.to_string(), c.to_string()],
                ..Expected::default()
            };
            let question = format!("Which entity does {a} reach when it {} something that {}?", first.relation, second.relation);
            out.push(draft(Generator::Chain, format!("{}-{}", case::render(&first.row), case::render(&second.row)), corpus, question, expected));
        }
    }
    out
}

/// Questions about absent entities: each decoy the store names no entity after becomes a
/// must-abstain case.
pub fn absent(store: &StoreView, decoys: &[&str], corpus: &str) -> Vec<Candidate> {
    let held: BTreeSet<String> = store.entities.iter().map(|e| e.name.to_lowercase()).collect();
    decoys
        .iter()
        .filter(|d| !d.trim().is_empty() && !held.contains(&d.to_lowercase()))
        .map(|d| {
            let expected = Expected { must_abstain: true, ..Expected::default() };
            draft(Generator::Absent, d.to_lowercase().replace(' ', "-"), corpus, format!("What does the store record about {d}?"), expected)
        })
        .collect()
}

/// The deployment's write-time removal rules, applied to a golden before commit.
///
/// A golden's free text — its question, reference answer and entities — may quote any
/// column. A pattern rule rewrites its matched spans in that text with the writer's own
/// matcher. A whole-value rule on a table the golden cites has no span to locate in free
/// text, and a keyed substitute needs a pepper the harness never holds; either refuses the
/// golden, as does a question left blank.
#[derive(Debug, Clone, Default)]
pub struct GoldenRedaction {
    rules: Vec<CompiledRule>,
}

impl GoldenRedaction {
    /// Compile `rules` as the writer does; a rule the writer refuses refuses here.
    pub fn new(rules: Vec<Rule>) -> Result<GoldenRedaction, EvalError> {
        let rules = rules
            .into_iter()
            .map(|r| {
                let at = format!("{}.{}", r.table, r.column);
                CompiledRule::compile(r).map_err(|e| EvalError::redaction_failed(&at, e.to_string()))
            })
            .collect::<Result<_, _>>()?;
        Ok(GoldenRedaction { rules })
    }

    /// `case` with every rule applied, or the refusal naming it.
    pub fn redact(&self, mut case: Case) -> Result<Case, EvalError> {
        let e = &case.expected;
        let cited: BTreeSet<String> = e
            .artifacts
            .iter()
            .chain(&e.must_cite)
            .chain(&e.must_not_retrieve)
            .chain(&e.edges)
            .filter_map(|r| case::row_ref(r))
            .map(|r| r.table)
            .collect();
        let fail = |id: &str, reason: String| EvalError::redaction_failed(id, reason);
        for compiled in &self.rules {
            let rule = &compiled.rule;
            if matches!(rule.matcher, Matcher::Whole(_)) {
                if cited.contains(&rule.table) {
                    return Err(fail(
                        &case.id,
                        format!("the whole-value rule on `{}.{}` binds a cited table, and free text holds no span to remove", rule.table, rule.column),
                    ));
                }
                continue;
            }
            let id = case.id.clone();
            let mut texts: Vec<&mut String> = vec![&mut case.question];
            texts.extend(case.expected.answer.as_mut());
            texts.extend(case.expected.entities.iter_mut());
            for text in texts {
                let mut value = Value::String(std::mem::take(text));
                compiled.rewrite(&mut value, &substitute).map_err(|e| fail(&id, e.to_string()))?;
                *text = match value {
                    Value::String(s) => s,
                    _ => String::new(),
                };
            }
        }
        if case.question.trim().is_empty() {
            return Err(fail(&case.id, "redaction leaves the question blank".to_string()));
        }
        Ok(case)
    }
}

/// The substitute of one matched span: the class marker for `replace`, the span's prefix
/// for `truncate`. A keyed substitute refuses: the harness holds no pepper.
fn substitute(rule: &Rule, matched: &str) -> Result<Option<String>, EnforceError> {
    match rule.operation {
        Operation::Drop => Ok(None),
        Operation::Replace => {
            let class = rule.argument.as_ref().and_then(Value::as_str).unwrap_or_default();
            Ok(Some(format!("[REDACTED:{class}]")))
        }
        Operation::Truncate => {
            let n = rule.argument.as_ref().and_then(Value::as_u64).unwrap_or_default() as usize;
            Ok(Some(matched.chars().take(n).collect()))
        }
        Operation::Hash | Operation::Tokenize => Err(EnforceError::RedactionInvalid(format!(
            "`{}.{}` rewrites under a keyed substitute, and the harness holds no pepper",
            rule.table, rule.column
        ))),
    }
}

/// Commit the candidates a reviewer approved — `approvals` maps a case id to a non-blank
/// reviewer — each through `redaction`. Every other candidate stays a draft.
pub fn commit(candidates: Vec<Candidate>, approvals: &BTreeMap<String, String>, redaction: &GoldenRedaction) -> Result<Vec<Case>, EvalError> {
    candidates
        .into_iter()
        .filter(|c| approvals.get(&c.case.id).is_some_and(|r| !r.trim().is_empty()))
        .map(|c| redaction.redact(c.case))
        .collect()
}

/// Who labeled an outcome.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LabelSource {
    /// A reviewer other than the system under test settled the label.
    Adjudicated { adjudicator: String },
    /// The system rated its own outcome.
    SelfRated,
}

/// A labeled outcome, carrying the case it would promote into.
#[derive(Debug, Clone, PartialEq)]
pub struct OutcomeLabel {
    pub case: Case,
    pub source: LabelSource,
}

/// Promote every label an adjudicator settled, each through `redaction`; a self-rated
/// label, or one with a blank adjudicator, promotes nothing.
pub fn promote(labels: Vec<OutcomeLabel>, redaction: &GoldenRedaction) -> Result<Vec<Case>, EvalError> {
    labels
        .into_iter()
        .filter(|l| matches!(&l.source, LabelSource::Adjudicated { adjudicator } if !adjudicator.trim().is_empty()))
        .map(|l| redaction.redact(l.case))
        .collect()
}

/// One generation of the native set.
#[derive(Debug, Clone, PartialEq)]
pub struct NativeSet {
    pub generation: u32,
    /// Unix seconds of the rotation that produced this generation.
    pub rotated_at: i64,
    pub cases: Vec<Case>,
}

/// The native set's rotation policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rotation {
    pub cadence_secs: i64,
    /// Cases of each new generation whose truth a reviewer re-reads.
    pub review_sample: usize,
}

/// A rotation's result: the next generation and the case ids sampled for review.
#[derive(Debug, Clone, PartialEq)]
pub struct Rotated {
    pub set: NativeSet,
    pub review: Vec<String>,
}

impl Rotation {
    /// Whether `set` has reached its cadence at `now`.
    pub fn due(&self, set: &NativeSet, now: i64) -> bool {
        now.saturating_sub(set.rotated_at) >= self.cadence_secs
    }

    /// The next generation of `set`, holding `fresh`, with a review sample drawn under `seed`.
    pub fn rotate(&self, set: &NativeSet, fresh: Vec<Case>, now: i64, seed: u64) -> Rotated {
        let mut ids: Vec<String> = fresh.iter().map(|c| c.id.clone()).collect();
        let mut state = seed;
        // A seeded Fisher-Yates prefix: the first `review_sample` positions are the sample.
        let take = self.review_sample.min(ids.len());
        for i in 0..take {
            let j = i + (splitmix64(&mut state) % (ids.len() - i) as u64) as usize;
            ids.swap(i, j);
        }
        ids.truncate(take);
        Rotated { set: NativeSet { generation: set.generation + 1, rotated_at: now, cases: fresh }, review: ids }
    }
}

/// Whether two run reports, from any two generations, compare: their run blocks must be one
/// pinned stamp, or the comparison raises `BaselineRunStampMismatch`.
pub fn compare_generations(earlier: &Value, later: &Value) -> Result<(), EvalError> {
    let stamp = |r: &Value| r.get(REPORT_RUN_KEY).and_then(|v| serde_json::from_value::<RunStamp>(v.clone()).ok());
    let show = |s: &Option<RunStamp>| s.as_ref().map(ToString::to_string).unwrap_or_else(|| "no run block".to_string());
    let (a, b) = (stamp(earlier), stamp(later));
    match (&a, &b) {
        (Some(x), Some(y)) if x == y => Ok(()),
        _ => Err(EvalError::BaselineRunStampMismatch { recorded: show(&a), run: show(&b) }),
    }
}
