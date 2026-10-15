//! `run.sweep`: a store-driven job run once per origin, each origin reading at
//! `available_as_of` equal to itself under one frozen `as_of`.

use super::RunError;
use crate::surface::arm::Schedule;
use crate::time::Instant;
use serde::{Deserialize, Serialize};

/// Whether an origin reads the job's output tables as the preceding origin left them.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SweepState {
    /// Every table resolves at the sweep's `as_of` (`run.sweep.fresh-state`).
    #[default]
    Fresh,
    /// Output tables resolve at the preceding origin's commit (`run.sweep.carried-state`).
    Carried,
}

/// A `sweep = { start, end, step, state }` declaration (`run.sweep.origins`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Sweep {
    pub start: String,
    pub end: String,
    /// A `<n><unit>` interval in the schedule grammar's units.
    pub step: String,
    #[serde(default)]
    pub state: SweepState,
}

fn invalid(why: String) -> RunError {
    RunError::Invalid(format!("sweep: {why}"))
}

impl Sweep {
    /// Every origin from `start` through `end` at each `step`, ascending.
    pub fn origins(&self) -> Result<Vec<Instant>, RunError> {
        let start = Instant::parse(&self.start).map_err(|e| invalid(format!("`start`: {e}")))?;
        let end = Instant::parse(&self.end).map_err(|e| invalid(format!("`end`: {e}")))?;
        let step = match Schedule::parse(&format!("every {}", self.step)) {
            Ok(Schedule::Every(secs)) => secs,
            Ok(_) => unreachable!("an `every` spelling parses to an interval"),
            Err(e) => return Err(invalid(format!("`step`: {e}"))),
        };
        if end < start {
            return Err(invalid(format!("`end` {} precedes `start` {}", self.end, self.start)));
        }
        let mut origins = vec![start];
        loop {
            let next = origins[origins.len() - 1].plus_secs(step);
            if next > end {
                return Ok(origins);
            }
            origins.push(next);
        }
    }

    /// The origins under the sweep's frozen `as_of`; an `end` later than it raises
    /// `RunSweepOriginAfterOpen` and opens no origin (`run.sweep.origin-after-open`).
    pub fn origins_at(&self, as_of: Instant) -> Result<Vec<Instant>, RunError> {
        let origins = self.origins()?;
        let end = Instant::parse(&self.end).map_err(|e| invalid(format!("`end`: {e}")))?;
        if end > as_of {
            return Err(RunError::RunSweepOriginAfterOpen(format!(
                "the sweep ends at {}, after its `as_of` {}; an origin past the frozen corpus reads nothing newer than it",
                end.to_rfc3339_nanos(),
                as_of.to_rfc3339_nanos()
            )));
        }
        Ok(origins)
    }

    /// The declaration's canonical text, which an origin's plan reference hashes
    /// (`run.sweep.origin-pin`).
    pub fn canonical(&self) -> String {
        serde_json::to_string(self).expect("a sweep declaration serializes")
    }
}

/// What an origin's run pins and records beside its input: the sweep declaration it
/// belongs to, the sweep's id, and its origin (`run.sweep.sweep-id`, `run.sweep.origin-pin`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OriginPin {
    pub declaration: String,
    pub sweep_id: String,
    pub origin: String,
}

/// Stamp each row an origin emits into `decl`'s table with the origin as its availability
/// instant, where the table declares `available_time`, so a later origin reads it and an
/// earlier one never does (`run.sweep.simulated-write`).
pub fn stamp_origin(decl: &crate::store::declare::TableDecl, rows: &mut [super::ports::Row], origin: &str) {
    let Some(available) = &decl.available_time else { return };
    for row in rows {
        row.insert(available.column.clone(), serde_json::Value::String(origin.to_string()));
    }
}
