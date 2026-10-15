//! `assurance.backtest.leak-score`: the case's judge scores each retrieved document 0 to 4
//! against a reference drawn from rows available before the horizon, and the report gives
//! the Brier score per leak tier.

use std::collections::{BTreeMap, BTreeSet};

use contextful_core::time::Instant;
use serde::Serialize;

/// The top of the leak scale.
pub const MAX_LEAK: u8 = 4;

/// One document's leak score at one case horizon.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct LeakRow {
    pub case: String,
    /// The SHA-256 of the document's text.
    pub document: String,
    pub horizon: String,
    pub score: u8,
}

/// The case's judge: how far a document says more than the reference, which holds only rows
/// available before `horizon`, on a 0 to 4 scale.
pub trait LeakJudge {
    fn score(&self, document: &str, reference: &str, horizon: Instant) -> Result<u8, String>;
}

/// The deterministic tier's leak judge: the share of the document's terms the reference
/// lacks, scaled to 0 to 4 and rounded.
#[derive(Debug, Clone, Copy, Default)]
pub struct StubLeakJudge;

fn terms(text: &str) -> Vec<String> {
    text.split(|c: char| !c.is_alphanumeric()).filter(|t| !t.is_empty()).map(str::to_lowercase).collect()
}

impl LeakJudge for StubLeakJudge {
    fn score(&self, document: &str, reference: &str, _horizon: Instant) -> Result<u8, String> {
        let known: BTreeSet<String> = terms(reference).into_iter().collect();
        let said = terms(document);
        if said.is_empty() {
            return Ok(0);
        }
        let novel = said.iter().filter(|t| !known.contains(*t)).count();
        Ok(((novel as f64 / said.len() as f64) * f64::from(MAX_LEAK)).round() as u8)
    }
}

/// One case's leak rows: each distinct document judged once against the case's other rows,
/// every one of which its bounded reads returned at or before the horizon.
pub fn score_case(judge: &dyn LeakJudge, case: &str, horizon: Instant, documents: &[String], trajectory: &[String]) -> Result<Vec<LeakRow>, String> {
    let mut seen = BTreeSet::new();
    let mut rows = Vec::new();
    for document in documents {
        if !seen.insert(document.clone()) {
            continue;
        }
        let reference: Vec<&str> = trajectory.iter().filter(|t| *t != document).map(String::as_str).collect();
        let score = judge.score(document, &reference.join("\n"), horizon)?;
        if score > MAX_LEAK {
            return Err(format!("case `{case}`: the leak judge scored {score}, beyond the 0 to {MAX_LEAK} scale"));
        }
        rows.push(LeakRow {
            case: case.to_string(),
            document: contextful_core::run::journal::sha256_hex(document.as_bytes()),
            horizon: horizon.to_rfc3339_nanos(),
            score,
        });
    }
    Ok(rows)
}

/// The Brier score per leak tier over `(tier, probability, outcome)` points: each tier's case
/// count and mean squared error.
pub fn brier_by_tier(points: &[(u8, f64, f64)]) -> BTreeMap<u8, (usize, f64)> {
    let mut sums: BTreeMap<u8, (usize, f64)> = BTreeMap::new();
    for (tier, p, o) in points {
        let entry = sums.entry(*tier).or_default();
        entry.0 += 1;
        entry.1 += (p - o).powi(2);
    }
    sums.into_iter().map(|(tier, (n, sum))| (tier, (n, sum / n as f64))).collect()
}
