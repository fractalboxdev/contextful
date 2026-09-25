//! `store.fold`: when a pass fires, and what it reports per table.

use super::resolve::TableState;
use crate::time::Instant;

/// Runs committed on a table since its previous pass that fire the next: 50
/// (`store.fold.triggers`).
pub const COMPACTION_RUN_COUNT: usize = 50;

/// Elapsed time after a table's previous pass that fires the next: 6 h (`store.fold.triggers`).
pub const COMPACTION_INTERVAL_SECS: u64 = 6 * 60 * 60;

/// Whether a table's trigger has fired: it holds committed runs its current snapshot
/// omits, and either as many as [`COMPACTION_RUN_COUNT`] or [`COMPACTION_INTERVAL_SECS`]
/// have passed since its previous pass published — or, before its first pass, since its
/// oldest unfolded run committed. An explicit `contextful context compact <table>` fires
/// regardless.
pub fn due(state: &TableState, now: Instant) -> bool {
    let unfolded = state.unfolded_runs();
    let Some(oldest) = unfolded.first() else { return false };
    if unfolded.len() >= COMPACTION_RUN_COUNT {
        return true;
    }
    let since = state.chain.first().map_or(oldest.committed_at, |s| s.created_at);
    since.secs_until(now) >= COMPACTION_INTERVAL_SECS
}

/// Whether a scheduled pass visits a table: its trigger fired, or it holds nothing to
/// fold, and the pass reports it nothing-landed and collects what retention allows, since
/// retention ages from the fold (`store.fold.retention`).
pub fn scheduled(state: &TableState, now: Instant) -> bool {
    state.unfolded_runs().is_empty() || due(state, now)
}

/// A pass's outcome for one table (`store.fold.result`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FoldOutcome {
    /// A snapshot was published; `collection` carries why the collection after it failed,
    /// which leaves the snapshot published (`store.fold.collection-failed`).
    Folded { snapshot_id: String, runs: usize, rows: u64, collection: Option<String> },
    /// No committed run was left to fold.
    NothingLanded,
    /// The pass failed for this table; the others continue.
    Failed(String),
}

impl FoldOutcome {
    /// Whether the command reporting this outcome exits non-zero: the pass failed, or it
    /// published and its collection failed (`store.fold.collection-failed`).
    pub fn is_failure(&self) -> bool {
        matches!(self, FoldOutcome::Failed(_) | FoldOutcome::Folded { collection: Some(_), .. })
    }
}

impl std::fmt::Display for FoldOutcome {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            FoldOutcome::Folded { snapshot_id, runs, rows, collection } => {
                write!(f, "folded {snapshot_id} ({runs} runs, {rows} rows)")?;
                match collection {
                    Some(e) => write!(f, "; collection failed: {e}"),
                    None => Ok(()),
                }
            }
            FoldOutcome::NothingLanded => f.write_str("nothing-landed"),
            FoldOutcome::Failed(e) => write!(f, "failed: {e}"),
        }
    }
}
