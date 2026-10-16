//! `disclosure.bound-staleness` at read and at diagnose: the refusal of a read past a
//! table's budget, the budget a source's sweep cadence cannot meet, and the clock offset a
//! staleness check reads now through.

use super::declare::Binding;
use super::sweep::{Freshness, SweepBlock};
use super::VisibilityError;
use crate::time::Instant;

/// The variable moving the instant a staleness check reads as now
/// (`disclosure.bound-staleness.clock-offset`).
pub const NOW_OFFSET_VAR: &str = "CONTEXTFUL_NOW_OFFSET_SECS";

/// A [`NOW_OFFSET_VAR`] value other than a non-negative integer
/// (`disclosure.bound-staleness.offset-malformed`).
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{NOW_OFFSET_VAR} = `{0}` is not a non-negative integer number of seconds")]
pub struct OffsetMalformed(pub String);

/// The offset [`NOW_OFFSET_VAR`] declares, `0` when unset. An offset only ages state, so
/// it can refuse a read and never extends a budget.
pub fn now_offset(value: Option<&str>) -> Result<u64, OffsetMalformed> {
    match value {
        None => Ok(0),
        Some(text) if !text.is_empty() && text.bytes().all(|b| b.is_ascii_digit()) => text.parse().map_err(|_| OffsetMalformed(text.to_string())),
        Some(text) => Err(OffsetMalformed(text.to_string())),
    }
}

/// Refuse a read touching `binding`'s table when its source has no completed full run, has
/// no covering watermark, or lags past the table's budget at `now`
/// (`disclosure.bound-staleness.access-stale`). A table at a level served from no store, or
/// declaring no budget, carries none to exceed.
pub fn check_fresh(binding: &Binding, freshness: Option<&Freshness>, now: Instant) -> Result<(), VisibilityError> {
    let Some(budget) = binding.max_acl_staleness_secs.filter(|_| binding.fidelity.is_servable()) else { return Ok(()) };
    let stale = |reason: &'static str, lag: Option<u64>, last: Option<Instant>| VisibilityError::AccessStale {
        table: binding.table.clone(),
        origin: binding.source.clone(),
        reason,
        lag: lag.map_or_else(|| "unbounded".to_string(), |l| format!("{l}s")),
        budget,
        last_observed: last.map_or_else(|| "none".to_string(), Instant::to_rfc3339),
    };
    let Some(f) = freshness else { return Err(stale("has no completed full run", None, None)) };
    if f.last_full_sweep_at.is_none() {
        return Err(stale("has no completed full run", None, f.watermark_at));
    }
    match f.lag_secs(now) {
        None => Err(stale("has no covering watermark", None, None)),
        Some(lag) if lag > budget => Err(stale("lags past its budget", Some(lag), f.watermark_at)),
        Some(_) => Ok(()),
    }
}

/// Refuse a servable binding whose budget is tighter than the cadence its source's sweep
/// sustains — the tighter of its full and incremental cadences — naming both figures
/// (`disclosure.bound-staleness.budget-below-cadence`).
pub fn check_cadence(bindings: &[Binding], sweeps: &[SweepBlock]) -> Result<(), VisibilityError> {
    for b in bindings.iter().filter(|b| b.fidelity.is_servable()) {
        let (Some(budget), Some(sweep)) = (b.max_acl_staleness_secs, sweeps.iter().find(|s| s.source == b.source)) else { continue };
        let cadence = sweep.full_every_secs.min(sweep.incremental_every_secs);
        if budget < cadence {
            return Err(VisibilityError::BudgetUnreachable { table: b.table.clone(), origin: b.source.clone(), budget, cadence });
        }
    }
    Ok(())
}
