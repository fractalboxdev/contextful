//! `store.fold`: when a pass fires, and what it reports per table.

use crate::time::Instant;

/// Runs committed on a table since its previous pass that fire the next: 50
/// (`store.fold.triggers`).
pub const COMPACTION_RUN_COUNT: usize = 50;

/// Elapsed time after a table's previous pass that fires the next: 6 h (`store.fold.triggers`).
pub const COMPACTION_INTERVAL_SECS: u64 = 6 * 60 * 60;

/// Whether a scheduled pass fires for a table holding `unfolded` committed runs, whose
/// previous pass published at `previous` (none before its first). An explicit
/// `contextful context compact <table>` fires regardless.
pub fn due(unfolded: usize, previous: Option<Instant>, now: Instant) -> bool {
    if unfolded == 0 {
        return false;
    }
    if unfolded >= COMPACTION_RUN_COUNT {
        return true;
    }
    match previous {
        Some(p) => p.secs_until(now) >= COMPACTION_INTERVAL_SECS,
        None => false,
    }
}

/// A pass's outcome for one table (`store.fold.result`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FoldOutcome {
    /// A snapshot was published.
    Folded { snapshot_id: String, runs: usize, rows: u64 },
    /// No committed run was left to fold.
    NothingLanded,
    /// The pass failed for this table; the others continue.
    Failed(String),
}

impl std::fmt::Display for FoldOutcome {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            FoldOutcome::Folded { snapshot_id, runs, rows } => write!(f, "folded {snapshot_id} ({runs} runs, {rows} rows)"),
            FoldOutcome::NothingLanded => f.write_str("nothing-landed"),
            FoldOutcome::Failed(e) => write!(f, "failed: {e}"),
        }
    }
}
