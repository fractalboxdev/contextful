//! Forecast rounds (`assurance.forecast`): cases frozen before they resolve, imputed
//! missing answers, the coverage floor, lead-time and evidence tracks, the market edge and
//! the no-evidence baseline. Every function is a pure computation over cases and answers.

use std::collections::BTreeMap;

use contextful_core::time::Instant;
use serde::Serialize;

use crate::backtest::BoundedCase;
use crate::error::EvalError;

/// Share of a round's cases a forecaster answers to be ranked, in percent
/// (`assurance-forecast-coverage`).
pub const COVERAGE_FLOOR_PERCENT: u32 = 95;

/// Where a case's question comes from.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Source {
    /// A curated dataset: a missing answer imputes 0.5.
    Dataset,
    /// A market, with its price at the freeze instant as an available row.
    Market { freeze_price: f64 },
}

/// A forecast case (`assurance.forecast.forecast-case`).
#[derive(Debug, Clone, PartialEq)]
pub struct ForecastCase {
    pub id: String,
    pub freeze: Instant,
    /// The resolution statement, naming the vintage it resolves against.
    pub resolution: String,
    pub resolved_at: Instant,
    /// The outcome, 1.0 when the statement held.
    pub outcome: f64,
    pub source: Source,
    /// The least lead before resolution an answer scores on the main track with.
    pub min_lead_secs: u64,
}

impl ForecastCase {
    /// The backtest form: a bounded case whose horizon is the freeze instant. A case
    /// resolved before its freeze refuses (`assurance.forecast.resolved-before-freeze`).
    pub fn backtest(&self) -> Result<BoundedCase, EvalError> {
        if self.resolved_at < self.freeze {
            return Err(EvalError::EvalCaseResolvedBeforeFreeze {
                case: self.id.clone(),
                resolved: self.resolved_at.to_rfc3339(),
                frozen: self.freeze.to_rfc3339(),
            });
        }
        Ok(BoundedCase { id: self.id.clone(), horizon: self.freeze, label: None, trajectory: Vec::new() })
    }

    /// The probability a missing answer scores as (`assurance.forecast.imputed`).
    pub fn imputed(&self) -> f64 {
        match self.source {
            Source::Dataset => 0.5,
            Source::Market { freeze_price } => freeze_price,
        }
    }
}

/// The track a score belongs to; tracks never pool (`assurance.forecast.tracks-separate`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Track {
    Backtest,
    Prospective,
    ToolFree,
    ToolUsing,
    /// Answers recorded within the case's minimum lead of resolution
    /// (`assurance.forecast.lead-time`).
    LateAnswer,
}

/// One forecaster's answer to one case.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Answer {
    pub probability: f64,
    pub recorded: Instant,
}

/// The track an answer scores on: [`Track::LateAnswer`] when recorded within the case's
/// minimum lead, else `track` (`assurance.forecast.lead-time`).
pub fn track_of(case: &ForecastCase, answer: &Answer, track: Track) -> Track {
    let lead = case.resolved_at.unix_secs() - answer.recorded.unix_secs();
    if lead < case.min_lead_secs as i64 {
        Track::LateAnswer
    } else {
        track
    }
}

/// A forecaster's figures on one track.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Standing {
    pub brier: f64,
    pub answered: usize,
    pub cases: usize,
    /// Below the coverage floor, the forecaster is reported unranked
    /// (`assurance.forecast.coverage-floor`).
    pub ranked: bool,
    /// The no-evidence forecaster's Brier score minus this one's
    /// (`assurance.forecast.no-evidence-baseline`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub over_no_evidence: Option<f64>,
    /// The freeze-price Brier score minus this one's, over market cases
    /// (`assurance.forecast.market-context`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub market_edge: Option<f64>,
}

fn brier(p: f64, outcome: f64) -> f64 {
    (p - outcome).powi(2)
}

/// Score `answers` (by case id) over `cases` on `track`. Missing answers impute; answers
/// recorded inside a case's lead window leave the track (`assurance.forecast.lead-time`).
pub fn standing(cases: &[ForecastCase], answers: &BTreeMap<String, Answer>, track: Track, no_evidence: Option<&BTreeMap<String, Answer>>) -> Standing {
    let mut scores = Vec::new();
    let mut answered = 0;
    let mut market = Vec::new();
    for case in cases {
        let given = answers.get(&case.id).filter(|a| track_of(case, a, track) == track);
        if given.is_some() {
            answered += 1;
        }
        let p = given.map_or_else(|| case.imputed(), |a| a.probability);
        scores.push(brier(p, case.outcome));
        if let Source::Market { freeze_price } = case.source {
            market.push(brier(freeze_price, case.outcome) - brier(p, case.outcome));
        }
    }
    let mean = |v: &[f64]| if v.is_empty() { f64::NAN } else { v.iter().sum::<f64>() / v.len() as f64 };
    let brier_mean = mean(&scores);
    let ranked = cases.is_empty() || answered * 100 >= cases.len() * COVERAGE_FLOOR_PERCENT as usize;
    let over_no_evidence = no_evidence.map(|baseline| standing(cases, baseline, track, None).brier - brier_mean);
    Standing { brier: brier_mean, answered, cases: cases.len(), ranked, over_no_evidence, market_edge: (!market.is_empty()).then(|| mean(&market)) }
}

/// A round's report: one standing per forecaster per track, and no figure pooling two
/// tracks (`assurance.forecast.tracks-separate`).
#[derive(Debug, Clone, PartialEq, Default, Serialize)]
pub struct RoundReport {
    pub tracks: BTreeMap<Track, BTreeMap<String, Standing>>,
}

impl RoundReport {
    /// Record `forecaster`'s standing on `track`.
    pub fn insert(&mut self, track: Track, forecaster: &str, standing: Standing) {
        self.tracks.entry(track).or_default().insert(forecaster.to_string(), standing);
    }
}
