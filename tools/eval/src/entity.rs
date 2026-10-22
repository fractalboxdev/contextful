//! Deterministic entity, claim, relationship and evidence grades for native fixtures.

use crate::metrics::Aggregate;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};

pub const METRICS: [&str; 5] = [
    "entity_accuracy",
    "eligible_claim_recall",
    "path_correctness",
    "citation_resolution",
    "appropriate_abstention",
];

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Claim {
    pub id: String,
    pub subject: String,
    pub predicate: String,
    pub object: String,
    pub citations: Vec<String>,
}

impl Claim {
    /// The canonical identity and relationship tuple, independent of citations.
    pub fn same_tuple(&self, other: &Claim) -> bool {
        self.id == other.id
            && self.subject == other.subject
            && self.predicate == other.predicate
            && self.object == other.object
    }

    /// The citation set; a claim's grade never depends on citation order.
    pub fn citation_set(&self) -> BTreeSet<&str> {
        self.citations.iter().map(String::as_str).collect()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Expected {
    pub entity: Option<String>,
    pub claims: Vec<String>,
    pub path: Vec<String>,
    pub citations: Vec<String>,
    pub abstain: bool,
    /// Adjudicated tuples, independent of either arm's return.
    pub facts: Vec<Claim>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Case {
    pub id: String,
    pub mention: String,
    pub question: String,
    pub relations: Vec<String>,
    pub anchor: String,
    pub tags: Vec<String>,
    pub expected: Expected,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Manifest {
    pub version: String,
    pub license: String,
    pub adjudication: String,
    pub corpus: String,
    pub corpus_sha256: String,
    pub entities: String,
    pub claims: String,
    pub evidence: String,
    pub k: usize,
    pub cases: Vec<Case>,
}

impl Manifest {
    pub fn parse(text: &str) -> Result<Self, String> {
        let manifest: Self = serde_json::from_str(text).map_err(|e| e.to_string())?;
        if manifest.version.is_empty()
            || manifest.license.is_empty()
            || manifest.adjudication.is_empty()
            || manifest.cases.is_empty()
            || manifest.k == 0
        {
            return Err(
                "an entity fixture carries a version, license, adjudication, cases and positive k"
                    .into(),
            );
        }
        let mut ids = BTreeSet::new();
        for c in &manifest.cases {
            if c.id.is_empty()
                || !ids.insert(&c.id)
                || c.mention.is_empty()
                || c.relations.is_empty()
            {
                return Err(format!(
                    "case `{}` has an empty/repeated id, mention or relation path",
                    c.id
                ));
            }
            contextful_core::time::Instant::parse(&c.anchor).map_err(|e| e.to_string())?;
            let e = &c.expected;
            let facts: BTreeSet<&str> = e.facts.iter().map(|f| f.id.as_str()).collect();
            if facts.len() != e.facts.len()
                || e.claims.iter().any(|id| !facts.contains(id.as_str()))
                || e.path != e.claims
                || e.abstain != e.claims.is_empty()
            {
                return Err(format!(
                    "case `{}` has inconsistent adjudicated truth",
                    c.id
                ));
            }
        }
        Ok(manifest)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Answer {
    pub entity: Option<String>,
    pub claims: Vec<Claim>,
    pub reason: Option<String>,
}

/// Follow requested predicates over returned tuples, retaining no partial answer.
pub fn follow(entity: Option<String>, relations: &[String], claims: &[Claim]) -> Answer {
    let mut answer = Answer {
        entity: entity.clone(),
        claims: Vec::new(),
        reason: None,
    };
    let Some(mut subject) = entity else {
        answer.reason = Some("unresolved_entity".into());
        return answer;
    };
    for relation in relations {
        let mut matches: Vec<&Claim> = claims
            .iter()
            .filter(|c| c.subject == subject && &c.predicate == relation)
            .collect();
        matches.sort_by(|a, b| a.id.cmp(&b.id));
        let objects: BTreeSet<&str> = matches.iter().map(|c| c.object.as_str()).collect();
        if objects.len() != 1 {
            answer.reason = Some(
                if objects.is_empty() {
                    "missing_evidence"
                } else {
                    "contradiction"
                }
                .into(),
            );
            answer.claims.clear();
            return answer;
        }
        let next = matches[0];
        subject = next.object.clone();
        answer.claims.push(next.clone());
    }
    answer
}

#[derive(Debug, Clone, Serialize)]
pub struct Scores {
    pub entity_accuracy: f64,
    pub eligible_claim_recall: Option<f64>,
    pub path_correctness: f64,
    pub citation_resolution: Option<f64>,
    pub appropriate_abstention: f64,
}

fn bit(value: bool) -> f64 {
    if value {
        1.0
    } else {
        0.0
    }
}

pub fn grade(expected: &Expected, answer: &Answer, eligible: &BTreeSet<String>) -> Scores {
    // A valid claim matches an adjudicated tuple and cites its complete citation set,
    // in any order; a proper subset of that set earns no claim or path credit.
    let valid = |c: &Claim| {
        expected
            .facts
            .iter()
            .any(|f| f.same_tuple(c) && f.citation_set() == c.citation_set())
            && !c.citations.is_empty()
            && c.citations.iter().all(|r| eligible.contains(r))
    };
    let matched: BTreeSet<&str> = answer
        .claims
        .iter()
        .filter(|c| valid(c))
        .map(|c| c.id.as_str())
        .collect();
    let recall = (!expected.claims.is_empty()).then(|| {
        expected
            .claims
            .iter()
            .filter(|id| matched.contains(id.as_str()))
            .count() as f64
            / expected.claims.len() as f64
    });
    let refs: Vec<(&Claim, &String)> = answer
        .claims
        .iter()
        .flat_map(|c| c.citations.iter().map(move |r| (c, r)))
        .collect();
    let citation_resolution = (!refs.is_empty()).then(|| {
        refs.iter()
            .filter(|(c, r)| {
                eligible.contains(*r)
                    && expected
                        .facts
                        .iter()
                        .any(|f| f.id == c.id && f.citations.contains(r))
            })
            .count() as f64
            / refs.len() as f64
    });
    let ids: Vec<&str> = answer.claims.iter().map(|c| c.id.as_str()).collect();
    let path: Vec<&str> = expected.path.iter().map(String::as_str).collect();
    let continuity = answer
        .claims
        .windows(2)
        .all(|pair| pair[0].object == pair[1].subject);
    Scores {
        entity_accuracy: bit(answer.entity == expected.entity),
        eligible_claim_recall: recall,
        path_correctness: bit(ids == path
            && continuity
            && answer.entity == expected.entity
            && answer
                .claims
                .first()
                .is_none_or(|c| Some(&c.subject) == answer.entity.as_ref())
            && answer.claims.iter().all(valid)
            && answer.claims.is_empty() == expected.abstain),
        citation_resolution,
        appropriate_abstention: bit(if expected.abstain {
            answer.claims.is_empty() && answer.reason.is_some()
        } else {
            !answer.claims.is_empty() && answer.reason.is_none()
        }),
    }
}

pub fn digest(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

/// Independent aggregates preserve missing denominators as empty summaries.
pub fn summarize(scores: &[Scores]) -> Value {
    let mut fields = BTreeMap::new();
    for (index, name) in METRICS.iter().enumerate() {
        let values = scores.iter().filter_map(|s| match index {
            0 => Some(s.entity_accuracy),
            1 => s.eligible_claim_recall,
            2 => Some(s.path_correctness),
            3 => s.citation_resolution,
            _ => Some(s.appropriate_abstention),
        });
        let aggregate: Aggregate = values.collect();
        fields.insert(*name, aggregate.summary());
    }
    json!(fields)
}
