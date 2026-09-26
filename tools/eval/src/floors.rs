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

/// `(metric, summary field, bound, side, holds over no case)` for each floor. A
/// forbidden-row rate over no case means no regression case named a must-not set, and an
/// in-window rate over no case means no case declared a recency bound; both hold. A
/// precision or duplicate rate over no case measured nothing, which holds no floor.
const FLOORS: [(&str, &str, f64, Side, bool); 4] = [
    ("r_precision", "mean", PRECISION_FLOOR, Side::AtLeast, false),
    ("forbidden_row_rate", "max", FORBIDDEN_ROW_RATE_CEILING, Side::AtMost, true),
    ("duplicate_row_rate", "max", DUPLICATE_ROW_RATE_CEILING, Side::AtMost, false),
    ("in_window_rate", "min", IN_WINDOW_RATE_FLOOR, Side::AtLeast, true),
];

/// Hold `report` to every floor on every leg of `retrieval`, and on each leg of
/// `edge_retrieval` the report carries. A leg or figure the report lacks breaches its floor,
/// read as NaN, so a report the runner truncated never passes vacuously.
pub fn check(report: &Value) -> FloorVerdict {
    let mut checks = Vec::new();
    for surface in SURFACES {
        let Some(node) = report.get(surface) else {
            if surface == "retrieval" {
                for leg in LEGS {
                    for (metric, field, bound, side, _) in FLOORS {
                        checks.push(FloorCheck { path: format!("{surface}.{leg}.{metric}.{field}"), bound, side, measured: f64::NAN, holds: false });
                    }
                }
            }
            continue;
        };
        for leg in LEGS {
            let leg_node = node.get(leg);
            // Every retrieval leg answers; an edge surface answers on the legs it configures.
            if leg_node.is_none() && surface != "retrieval" {
                continue;
            }
            for (metric, field, bound, side, empty_holds) in FLOORS {
                let path = format!("{surface}.{leg}.{metric}.{field}");
                let summary = leg_node.and_then(|l| l.get(metric));
                let n = summary.and_then(|s| s.get("n")).and_then(Value::as_u64).unwrap_or(0);
                if summary.is_some() && n == 0 && empty_holds {
                    continue;
                }
                let measured = summary.and_then(|s| s.get(field)).and_then(Value::as_f64).filter(|_| n > 0).unwrap_or(f64::NAN);
                let holds = match side {
                    Side::AtLeast => measured >= bound,
                    Side::AtMost => measured <= bound,
                };
                checks.push(FloorCheck { path, bound, side, measured, holds });
            }
        }
    }
    let passed = checks.iter().all(|c| c.holds);
    FloorVerdict { checks, passed }
}
