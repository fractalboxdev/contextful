//! `store.fold`: when a pass fires, and what it reports per table.

use super::resolve::TableState;
use crate::time::Instant;

/// Row-age work completed by one table's pass.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RetentionReport {
    pub cutoff: Instant,
    pub rows_expired: u64,
    pub partitions_dropped: u64,
}

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
    /// `warnings` carries what the pass's partition plan warns about
    /// (`store.index.partition-warnings`); a warning fails nothing.
    Folded { snapshot_id: String, runs: usize, rows: u64, retention: Option<RetentionReport>, collected: Vec<String>, collection: Option<String>, warnings: Vec<String> },
    /// No committed run was left to fold.
    NothingLanded,
    /// No snapshot changed, while row-age retention and collection were checked.
    NothingLandedRetained { cutoff: Instant, collected: Vec<String> },
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
            FoldOutcome::Folded { snapshot_id, runs, rows, retention, collected, collection, .. } => {
                write!(f, "folded {snapshot_id} ({runs} runs, {rows} rows)")?;
                if let Some(report) = retention {
                    write!(f, "; cutoff {}, {} rows expired, {} partitions dropped", report.cutoff, report.rows_expired, report.partitions_dropped)?;
                }
                if !collected.is_empty() {
                    write!(f, "; collected {} directories: {}", collected.len(), collected.join(", "))?;
                }
                match collection {
                    Some(e) => write!(f, "; collection failed: {e}"),
                    None => Ok(()),
                }
            }
            FoldOutcome::NothingLanded => f.write_str("nothing-landed"),
            FoldOutcome::NothingLandedRetained { cutoff, collected } => {
                write!(f, "nothing-landed; cutoff {cutoff}, 0 rows expired, 0 partitions dropped")?;
                if !collected.is_empty() {
                    write!(f, "; collected {} directories: {}", collected.len(), collected.join(", "))?;
                }
                Ok(())
            }
            FoldOutcome::Failed(e) => write!(f, "failed: {e}"),
        }
    }
}
