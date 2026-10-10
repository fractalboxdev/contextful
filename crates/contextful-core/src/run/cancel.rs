//! `run.cancel`: the stop mark a caller writes onto a run row, its two grains, and
//! which rows accept one.

use super::record::{RunRow, StopMark};
use super::RunError;
use crate::grant::{Action, Grant};
use crate::time::Instant;

/// Cadence of the catalog read feeding the cancellation token: 500 ms (`run.cancel.poll-interval`).
pub const POLL_INTERVAL_MS: u64 = 500;

/// A stop's grain (`run.cancel.two-grains`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scope {
    /// Halts one run; the fire continues.
    Run,
    /// Halts every in-flight run of the pipeline and every table or chunk the fire had left.
    Pipeline,
}

impl Scope {
    pub fn name(self) -> &'static str {
        match self {
            Scope::Run => "run",
            Scope::Pipeline => "pipeline",
        }
    }

    /// Read a stored scope. An unrecognized spelling reads as `run`.
    pub fn read(stored: &str) -> Scope {
        match stored {
            "pipeline" => Scope::Pipeline,
            _ => Scope::Run,
        }
    }
}

/// Write a stop onto `row`. Only an in-flight row accepts a mark; marking an already
/// marked row overwrites it with the newer request (`run.cancel.not-in-flight`,
/// `run.cancel.re-mark`).
pub fn mark(row: &mut RunRow, scope: Scope, reason: Option<String>, requested_at: Instant) -> Result<(), RunError> {
    if !row.status.is_in_flight() {
        return Err(RunError::CancelTargetNotInFlight(format!(
            "run `{}` is {}; only a pending, running or waiting run accepts a stop",
            row.run_id, row.status
        )));
    }
    row.stop = Some(StopMark { requested_at, scope: scope.name().to_string(), reason });
    Ok(())
}

/// Whether `row` shares `target`'s `pipeline` grain: the same pipeline for a table run,
/// the same host-declared scope for a host execution's run (`run.cancel.host-grain`).
pub fn same_grain(target: &RunRow, row: &RunRow) -> bool {
    match (&target.host_scope, &row.host_scope) {
        (Some(ours), Some(theirs)) => ours == theirs,
        (None, None) => row.pipeline_id == target.pipeline_id,
        _ => false,
    }
}

/// Whether `row` is stopped by `mark` on `target`: a run-scoped mark stops its own row;
/// a pipeline-scoped mark stops every in-flight run sharing the target's grain.
pub fn stops(target: &RunRow, row: &RunRow) -> bool {
    let Some(stop) = &target.stop else { return false };
    if !row.status.is_in_flight() {
        return false;
    }
    match Scope::read(&stop.scope) {
        Scope::Run => row.run_id == target.run_id,
        Scope::Pipeline => same_grain(target, row),
    }
}

/// Authorize a credentialed stop against the pipeline its run record holds: an `execute`
/// grant covering that pipeline admits it. An unrecorded run refuses exactly as an
/// uncovered one, and neither refusal names the pipeline (`run.cancel.stop-unauthorized`,
/// `run.cancel.authority-from-the-record`).
pub fn authorize_stop(grants: &[Grant], record: Option<&RunRow>) -> Result<(), RunError> {
    let covered = record.is_some_and(|r| {
        grants.iter().any(|g| g.actions.contains(&Action::Execute) && g.tables.iter().any(|p| p.covers_name(&r.pipeline_id)))
    });
    if covered {
        Ok(())
    } else {
        Err(RunError::CancelUnauthorized("no execute grant covers this run's pipeline".into()))
    }
}
