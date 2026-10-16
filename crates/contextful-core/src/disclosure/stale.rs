//! `disclosure.bound-staleness` at read and at diagnose: the refusal of a read past a
//! table's budget, the budget a source's sweep cadence cannot meet, and the clock offset a
//! staleness check reads now through.

use super::declare::{Binding, OnStale};
use super::reach::public_resources;
use super::sweep::{Freshness, SourceState, SweepBlock};
use super::VisibilityError;
use crate::time::Instant;
use std::collections::BTreeMap;

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

/// How a read serves a bound table at one instant: whole, or narrowed to its public rows
/// (`disclosure.bound-staleness.public-only`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Posture {
    Fresh,
    Narrowed,
}

/// A table's public-status watermark at `now`: the oldest observation among its source's
/// resources holding a read-conferring, unexpired `public` grant under `levels`, or `now`
/// when none does (`disclosure.bound-staleness.public-watermark`).
pub fn public_watermark(state: &SourceState, levels: Option<&BTreeMap<String, String>>, now: Instant) -> Instant {
    public_resources(state, levels, now).into_values().min().unwrap_or(now)
}

/// The posture a read serves `binding`'s table under at `now`. Inside its budget the table
/// reads whole. Past it, a `public_only` table whose source has a completed full run and a
/// public-status watermark within the budget narrows (`disclosure.bound-staleness.public-only`);
/// any other table refuses (`disclosure.bound-staleness.access-stale`).
pub fn posture(binding: &Binding, state: &SourceState, levels: Option<&BTreeMap<String, String>>, now: Instant) -> Result<Posture, VisibilityError> {
    let refused = match check_fresh(binding, state.freshness.as_ref(), now) {
        Ok(()) => return Ok(Posture::Fresh),
        Err(e) => e,
    };
    let (OnStale::PublicOnly, Some(budget), Some(f)) = (binding.on_stale, binding.max_acl_staleness_secs, state.freshness.as_ref()) else { return Err(refused) };
    if f.last_full_sweep_at.is_none() {
        return Err(refused);
    }
    let watermark = public_watermark(state, levels, now);
    let lag = watermark.secs_until(now);
    if lag > budget {
        return Err(VisibilityError::AccessStale {
            table: binding.table.clone(),
            origin: binding.source.clone(),
            reason: "has a public-status watermark past its budget",
            lag: format!("{lag}s"),
            budget,
            last_observed: watermark.to_rfc3339(),
        });
    }
    Ok(Posture::Narrowed)
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
