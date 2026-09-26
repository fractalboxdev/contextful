//! The absolute floors. Each gates a run report independently of every committed
//! baseline value, on every leg of every retrieval surface the report carries.
//!
//! The precision floor holds the leg's mean; the three rate floors hold every case, so
//! they read the per-case extreme a [`crate::metrics::Summary`] carries.

use serde::Serialize;
use serde_json::Value;

use crate::metrics::LEGS;

/// Precision at rank min(k, R) a ranked run holds (`assurance-precision-floor`: 60 percent).
pub const PRECISION_FLOOR: f64 = 0.60;

/// Returned rows named in a must-not-retrieve set (`assurance-forbidden-row-rate`: 0 percent).
pub const FORBIDDEN_ROW_RATE_CEILING: f64 = 0.0;

/// Ranking entries repeating a table-and-row-key pair (`assurance-duplicate-row-rate`: 0 percent).
pub const DUPLICATE_ROW_RATE_CEILING: f64 = 0.0;

/// In-window rate on a case declaring a recency bound (`assurance-in-window-rate`: 95 percent).
pub const IN_WINDOW_RATE_FLOOR: f64 = 0.95;

/// The retrieval surfaces a report carries legs under.
pub const SURFACES: [&str; 2] = ["retrieval", "edge_retrieval"];

/// Which side of its bound a figure must stay on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Side {
    AtLeast,
    AtMost,
}

/// One floor held against one leg.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct FloorCheck {
    /// The report path of the figure the floor reads, e.g. `retrieval.hybrid.in_window_rate.min`.
    pub path: String,
    pub bound: f64,
    pub side: Side,
    pub measured: f64,
    pub holds: bool,
}

/// Every floor the report reached, and whether all of them hold.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct FloorVerdict {
    pub checks: Vec<FloorCheck>,
    pub passed: bool,
}

impl FloorVerdict {
    pub fn breaches(&self) -> impl Iterator<Item = &FloorCheck> {
        self.checks.iter().filter(|c| !c.holds)
    }
}

/// `(metric, summary field, bound, side)` for each floor.
const FLOORS: [(&str, &str, f64, Side); 4] = [
    ("r_precision", "mean", PRECISION_FLOOR, Side::AtLeast),
    ("forbidden_row_rate", "max", FORBIDDEN_ROW_RATE_CEILING, Side::AtMost),
    ("duplicate_row_rate", "max", DUPLICATE_ROW_RATE_CEILING, Side::AtMost),
    ("in_window_rate", "min", IN_WINDOW_RATE_FLOOR, Side::AtLeast),
];

/// Hold `report` to every floor. A leg whose summary has no finite sample (`n == 0`) has
/// nothing the floor could hold and is not checked.
pub fn check(report: &Value) -> FloorVerdict {
    let mut checks = Vec::new();
    for surface in SURFACES {
        for leg in LEGS {
            let Some(node) = report.get(surface).and_then(|s| s.get(leg)) else {
                continue;
            };
            for (metric, field, bound, side) in FLOORS {
                let Some(summary) = node.get(metric) else {
                    continue;
                };
                if summary.get("n").and_then(Value::as_u64).unwrap_or(0) == 0 {
                    continue;
                }
                let measured = summary.get(field).and_then(Value::as_f64).unwrap_or(f64::NAN);
                let holds = match side {
                    Side::AtLeast => measured >= bound,
                    Side::AtMost => measured <= bound,
                };
                checks.push(FloorCheck { path: format!("{surface}.{leg}.{metric}.{field}"), bound, side, measured, holds });
            }
        }
    }
    let passed = checks.iter().all(|c| c.holds);
    FloorVerdict { checks, passed }
}
