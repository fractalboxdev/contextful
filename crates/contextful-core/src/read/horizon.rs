//! `read.horizon`: the availability bound a credential's horizon imposes on every read.

use super::error::ReadError;
use crate::grant::Grant;
use crate::store::bound_time::{Bound, Bounds};
use crate::time::Instant;

/// The credential's horizon: the earliest any of its grants carries, or `None` when no
/// grant carries one (`authority.grant.horizon`).
pub fn horizon(grants: &[Grant]) -> Option<Instant> {
    grants.iter().filter_map(|g| g.horizon).min()
}

/// Refuse an argument bounding availability later than `horizon`, naming it
/// (`read.horizon.exceeds-horizon`). A bound at or before the horizon passes.
pub fn within(horizon: Option<Instant>, argument: &str, at: Option<Instant>) -> Result<(), ReadError> {
    match (horizon, at) {
        (Some(h), Some(t)) if t > h => Err(ReadError::ReadBoundExceedsHorizon(format!(
            "`{argument}` {} falls later than the credential's horizon {}",
            t.to_rfc3339_nanos(),
            h.to_rfc3339_nanos()
        ))),
        _ => Ok(()),
    }
}

/// `bounds` under `horizon`: a supplied `available_as_of` holds within it, and an absent
/// one reads at the horizon, inclusive (`read.horizon.inherited-bound`). `as_of` and
/// `valid_as_of` pass unbounded by it.
pub fn inherit(horizon: Option<Instant>, bounds: Bounds) -> Result<Bounds, ReadError> {
    let Some(h) = horizon else { return Ok(bounds) };
    match bounds.available_as_of {
        // An exclusive bound admits instants strictly before it, so it holds at the horizon's successor.
        Some(b) => {
            let last = if b.inclusive { b.at } else { Instant::from_unix_nanos(b.at.unix_nanos() - 1).unwrap_or(b.at) };
            within(Some(h), "available_as_of", Some(last))?
        }
        None => return Ok(Bounds { available_as_of: Some(Bound { at: h, inclusive: true }), ..bounds }),
    }
    Ok(bounds)
}
