//! Trend-tier comparison: a figure against its baseline on a matching runner
//! (`assurance.measure.trend-band`, `assurance.measure.runner-stamp`). A trend verdict
//! annotates the run report and fails nothing.

use serde::{Deserialize, Serialize};

use crate::baseline::Direction;
use crate::record::Runner;

/// Worsening past which a trend figure is annotated (`assurance-trend-band`: 25 percent).
pub const TREND_BAND: f64 = 0.25;

/// Warm repeats per timed batch (`assurance-timing-iterations`: 200 repeats).
pub const TIMING_ITERATIONS: usize = 200;

/// Batches behind a timed figure (`assurance-timing-batches`: 5 repeats).
pub const TIMING_BATCHES: usize = 5;

/// A figure and the runner it was measured on.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Figure {
    pub value: f64,
    pub runner: Runner,
}

/// A trend figure against its baseline.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "snake_case", tag = "verdict")]
pub enum Comparison {
    /// Within the band; `worse_percent` is negative for an improvement.
    Within { worse_percent: f64 },
    /// Past the band: the run report carries this annotation.
    Annotated { worse_percent: f64 },
    /// The runner stamps differ, so the figures do not compare.
    Incomparable,
}

impl Comparison {
    /// The run report's annotation, when the figure moved past the band.
    pub fn annotation(&self) -> Option<String> {
        match self {
            Comparison::Annotated { worse_percent } => Some(format!("{worse_percent:+.0} percent against the baseline")),
            _ => None,
        }
    }
}

/// Compare `current` against `baseline` in `direction`. A zero baseline compares by sign
/// alone: any worsening annotates.
pub fn compare(current: &Figure, baseline: &Figure, direction: Direction) -> Comparison {
    if current.runner != baseline.runner {
        return Comparison::Incomparable;
    }
    let delta = match direction {
        Direction::LowerIsBetter => current.value - baseline.value,
        Direction::HigherIsBetter => baseline.value - current.value,
    };
    let worse = if baseline.value == 0.0 {
        if delta > 0.0 {
            f64::INFINITY
        } else {
            0.0
        }
    } else {
        delta / baseline.value.abs()
    };
    let worse_percent = worse * 100.0;
    if worse > TREND_BAND + 1e-9 {
        Comparison::Annotated { worse_percent }
    } else {
        Comparison::Within { worse_percent }
    }
}

/// The median batch of a timed figure: each batch's p50 and p95, and the batch whose p95
/// is the median across batches.
pub fn median_batch(batches: &[Vec<f64>]) -> Option<(f64, f64)> {
    let mut summaries: Vec<(f64, f64)> = batches.iter().filter(|b| !b.is_empty()).map(|b| (percentile(b, 50.0), percentile(b, 95.0))).collect();
    if summaries.is_empty() {
        return None;
    }
    summaries.sort_by(|a, b| a.1.total_cmp(&b.1));
    Some(summaries[summaries.len() / 2])
}

/// The nearest-rank percentile of `samples`.
pub fn percentile(samples: &[f64], p: f64) -> f64 {
    let mut s = samples.to_vec();
    s.sort_by(f64::total_cmp);
    let rank = ((p / 100.0) * s.len() as f64).ceil().max(1.0) as usize;
    s[rank.min(s.len()) - 1]
}
