//! `read.synthesize.confidence-calibration` and `read.settle.calibration-report`: a
//! per-shape-and-predicate isotonic map from emitted confidence to settled frequency,
//! validated on held-out settled predictions, and the report that names its figures.

use super::declare::Shape;
use std::collections::BTreeMap;

/// Equal-width bins the expected calibration error averages over
/// (`read.settle.calibration-report`).
pub const ECE_BINS: usize = 10;

/// One settled prediction in every this-many, by settlement order within its group, is
/// held out of fitting and scores the map (`read.settle.calibration-report`).
pub const HOLD_OUT_EVERY: usize = 5;

/// One settled prediction: the shape and predicate of the claim that emitted it, its
/// emitted confidence, and whether the settled outcome held.
#[derive(Debug, Clone, PartialEq)]
pub struct Settled {
    pub shape: Shape,
    pub predicate: String,
    pub confidence: f64,
    pub held: bool,
}

/// A confidence as reported: the emitted number labelled `uncalibrated`, or the validated
/// map's value labelled `calibrated`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Confidence {
    Uncalibrated(f64),
    Calibrated(f64),
}

impl Confidence {
    pub fn value(self) -> f64 {
        match self {
            Confidence::Uncalibrated(v) | Confidence::Calibrated(v) => v,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Confidence::Uncalibrated(_) => "uncalibrated",
            Confidence::Calibrated(_) => "calibrated",
        }
    }
}

/// One shape-and-predicate group's figures.
#[derive(Debug, Clone, PartialEq)]
pub struct GroupReport {
    pub shape: Shape,
    pub predicate: String,
    /// Settled predictions in the group.
    pub samples: usize,
    /// Mean squared error of the emitted confidence over every sample.
    pub brier: f64,
    /// Expected calibration error of the emitted confidence over [`ECE_BINS`] bins.
    pub ece: f64,
    /// Held-out samples, every [`HOLD_OUT_EVERY`]th.
    pub held_out: usize,
    /// Brier score of the emitted confidence on the held-out samples; `None` with none.
    pub held_out_raw_brier: Option<f64>,
    /// Brier score of the fitted map on the held-out samples; `None` with no held-out or
    /// no fitting sample.
    pub held_out_brier: Option<f64>,
    /// Whether the map lowers held-out Brier score and so supplies reported confidence.
    pub calibrated: bool,
}

/// A monotone step function fitted by pool-adjacent-violators: each block's lowest
/// confidence and its pooled outcome frequency, ascending.
#[derive(Debug, Clone, PartialEq)]
struct Isotonic {
    blocks: Vec<(f64, f64)>,
}

impl Isotonic {
    fn fit(samples: &[(f64, f64)]) -> Option<Isotonic> {
        let mut sorted = samples.to_vec();
        sorted.sort_by(|a, b| a.0.total_cmp(&b.0));
        // Equal confidences pool first: one step answers one confidence.
        let mut points: Vec<(f64, f64, f64)> = Vec::new();
        for (x, y) in sorted {
            match points.last_mut() {
                Some(p) if p.0 == x => {
                    p.1 += y;
                    p.2 += 1.0;
                }
                _ => points.push((x, y, 1.0)),
            }
        }
        // (lowest x, sum of y, count)
        let mut pooled: Vec<(f64, f64, f64)> = Vec::new();
        for point in points {
            pooled.push(point);
            while pooled.len() > 1 {
                let (_, sb, nb) = pooled[pooled.len() - 1];
                let (_, sa, na) = pooled[pooled.len() - 2];
                if sa / na <= sb / nb {
                    break;
                }
                pooled.pop();
                let last = pooled.last_mut().expect("two blocks were present");
                last.1 += sb;
                last.2 += nb;
            }
        }
        (!pooled.is_empty()).then(|| Isotonic { blocks: pooled.into_iter().map(|(x, s, n)| (x, s / n)).collect() })
    }

    fn apply(&self, x: f64) -> f64 {
        self.blocks.iter().take_while(|(lo, _)| *lo <= x).last().unwrap_or(&self.blocks[0]).1
    }
}

fn outcome(held: bool) -> f64 {
    if held {
        1.0
    } else {
        0.0
    }
}

fn brier(pairs: impl Iterator<Item = (f64, f64)>) -> Option<f64> {
    let (sum, n) = pairs.fold((0.0, 0usize), |(s, n), (p, y)| (s + (p - y) * (p - y), n + 1));
    (n > 0).then(|| sum / n as f64)
}

fn ece(pairs: &[(f64, f64)]) -> f64 {
    let mut bins = [(0.0f64, 0.0f64, 0usize); ECE_BINS];
    for &(p, y) in pairs {
        let i = ((p * ECE_BINS as f64) as usize).min(ECE_BINS - 1);
        bins[i].0 += p;
        bins[i].1 += y;
        bins[i].2 += 1;
    }
    let n = pairs.len() as f64;
    // Each bin weighs |mean confidence - frequency| by its share, which is |Σp - Σy| / n.
    bins.iter().filter(|b| b.2 > 0).map(|&(p, y, _)| (p - y).abs() / n).sum()
}

type Key = (Shape, String);

/// Group settled predictions by shape and predicate, in settlement order, keeping only a
/// finite confidence within `[0, 1]`.
fn groups(samples: &[Settled]) -> BTreeMap<Key, Vec<(f64, f64)>> {
    let mut groups: BTreeMap<Key, Vec<(f64, f64)>> = BTreeMap::new();
    for s in samples.iter().filter(|s| s.confidence.is_finite() && (0.0..=1.0).contains(&s.confidence)) {
        groups.entry((s.shape, s.predicate.clone())).or_default().push((s.confidence, outcome(s.held)));
    }
    groups
}

/// One group's map, its report, and whether the map validated.
fn assess(key: &Key, pairs: &[(f64, f64)]) -> (Option<Isotonic>, GroupReport) {
    let held = |i: usize| i % HOLD_OUT_EVERY == HOLD_OUT_EVERY - 1;
    let held_out: Vec<(f64, f64)> = pairs.iter().enumerate().filter(|(i, _)| held(*i)).map(|(_, p)| *p).collect();
    let train: Vec<(f64, f64)> = pairs.iter().enumerate().filter(|(i, _)| !held(*i)).map(|(_, p)| *p).collect();
    let map = Isotonic::fit(&train);
    let held_out_raw_brier = brier(held_out.iter().copied());
    let held_out_brier = map.as_ref().and_then(|m| brier(held_out.iter().map(|&(p, y)| (m.apply(p), y))));
    let calibrated = matches!((held_out_brier, held_out_raw_brier), (Some(fit), Some(raw)) if fit < raw);
    let report = GroupReport {
        shape: key.0,
        predicate: key.1.clone(),
        samples: pairs.len(),
        brier: brier(pairs.iter().copied()).unwrap_or(0.0),
        ece: ece(pairs),
        held_out: held_out.len(),
        held_out_raw_brier,
        held_out_brier,
        calibrated,
    };
    (map.filter(|_| calibrated), report)
}

/// The calibration report: one entry per shape and predicate with a settled prediction
/// (`read.settle.calibration-report`).
pub fn report(samples: &[Settled]) -> Vec<GroupReport> {
    groups(samples).iter().map(|(key, pairs)| assess(key, pairs).1).collect()
}

/// The validated maps over a set of settled predictions.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Calibration {
    maps: BTreeMap<Key, Isotonic>,
}

impl Calibration {
    /// Fit each group's isotonic map on its non-held-out predictions, keeping it only where
    /// it lowers the held-out Brier score (`read.synthesize.confidence-calibration`).
    pub fn fit(samples: &[Settled]) -> Calibration {
        let maps = groups(samples).iter().filter_map(|(key, pairs)| assess(key, pairs).0.map(|m| (key.clone(), m))).collect();
        Calibration { maps }
    }

    /// The reported confidence for a claim of `shape` and `predicate`: the validated map's
    /// value, else the emitted confidence labelled `uncalibrated`.
    pub fn confidence(&self, shape: Shape, predicate: &str, emitted: f64) -> Confidence {
        match self.maps.get(&(shape, predicate.to_string())) {
            Some(map) if emitted.is_finite() => Confidence::Calibrated(map.apply(emitted.clamp(0.0, 1.0))),
            _ => Confidence::Uncalibrated(emitted),
        }
    }
}
