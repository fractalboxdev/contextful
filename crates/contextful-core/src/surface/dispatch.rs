//! Admission into the bounded fire pool (`surface.dispatch.pool-bound`,
//! `surface.dispatch.exclusion-key`).

use crate::time::Instant;
use std::collections::BTreeSet;

/// Units one deployment runs at once when `[control] pool` names no bound.
pub const DEFAULT_POOL: usize = 4;

/// Seconds a stopping serve process waits between `SIGTERM` and `SIGKILL` to its children
/// (`surface.dispatch.children-reaped`).
pub const CHILD_GRACE_SECS: u64 = 10;

/// One due unit: its exclusion key, the pipeline id, and the instant it fell due.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Due {
    pub key: String,
    pub at: Instant,
}

/// One beat's admission.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Admitted {
    /// Units to start now, in due order.
    pub start: Vec<Due>,
    /// Units past the pool's bound; they stay armed.
    pub pending: Vec<Due>,
    /// Units whose key is in flight; they start no second instance.
    pub held: Vec<Due>,
}

/// Admit `due` into a pool of `bound` slots holding `in_flight`: order by due instant then
/// key, one unit per key, a key in flight held, and slots past the bound pending.
pub fn admit(due: &[Due], in_flight: &BTreeSet<String>, bound: usize) -> Admitted {
    let mut ordered: Vec<&Due> = due.iter().collect();
    ordered.sort_by(|a, b| a.at.cmp(&b.at).then_with(|| a.key.cmp(&b.key)));
    let mut seen = BTreeSet::new();
    let mut out = Admitted::default();
    let mut free = bound.saturating_sub(in_flight.len());
    for unit in ordered {
        if !seen.insert(unit.key.clone()) {
            continue;
        }
        if in_flight.contains(&unit.key) {
            out.held.push(unit.clone());
        } else if free > 0 {
            free -= 1;
            out.start.push(unit.clone());
        } else {
            out.pending.push(unit.clone());
        }
    }
    out
}
