//! The harness core's one integration binary.

mod baseline;
mod floors;
mod metrics;

use std::collections::HashSet;

use contextful_eval::metrics::{Aggregate, Returned, RowRef};
use serde_json::{json, Value};

pub const EPS: f64 = 1e-9;

/// Rows of one table, `notes`, keyed by the given strings.
pub fn rows(keys: &[&str]) -> Vec<RowRef> {
    keys.iter().map(|k| RowRef::new("notes", *k)).collect()
}

pub fn set(keys: &[&str]) -> HashSet<RowRef> {
    rows(keys).into_iter().collect()
}

/// Returned rows of `notes`, each with the engine's in-window flag.
pub fn returned(rows: &[(&str, bool)]) -> Vec<Returned> {
    rows.iter().map(|(k, w)| Returned { row: RowRef::new("notes", *k), in_window: *w }).collect()
}

/// The report summary of `values`, as the harness serializes it.
pub fn summary(values: &[f64]) -> Value {
    serde_json::to_value(values.iter().copied().collect::<Aggregate>().summary()).unwrap()
}

/// A summary node over `n` cases with the given mean and extremes.
pub fn node(n: u64, mean: f64) -> Value {
    json!({ "n": n, "mean": mean, "min": 0.0, "max": 1.0 })
}

/// The deterministic run block at k = 10.
pub fn run(k: usize) -> Value {
    json!({ "k": k, "tier": "deterministic", "model": null, "samples": 1 })
}
