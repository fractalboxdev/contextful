//! A table's FROM-source: the snapshot and committed runs a read covers, and the
//! explicit sorted file list they resolve to (`store.reconcile.explicit-file-list`).

use super::bound_time::Bound;
use super::declare::WriteMode;
use super::lay_out::{RunManifest, SnapshotManifest};
use super::StoreError;
use std::collections::BTreeSet;

/// What the tree holds for one table, read from its pointer and manifests.
#[derive(Debug, Clone, Default)]
pub struct TableState {
    pub table: String,
    pub write_mode: WriteMode,
    /// Every run whose `_manifest.json` exists.
    pub runs: Vec<RunManifest>,
    /// The snapshot the pointer names, then its `parent` chain, newest first, as far as
    /// the manifests are on disk.
    pub chain: Vec<SnapshotManifest>,
    /// Retention has collected history: the chain ends at a parent no longer on disk,
    /// or a run the chain folded is gone.
    pub history_collected: bool,
}

/// The snapshot and runs one read covers.
#[derive(Debug, Clone, PartialEq)]
pub struct Resolution<'a> {
    pub snapshot: Option<&'a SnapshotManifest>,
    pub runs: Vec<&'a RunManifest>,
}

impl Resolution<'_> {
    /// Data files relative to the table directory, sorted.
    pub fn files(&self) -> Vec<String> {
        let mut files: Vec<String> = Vec::new();
        if let Some(s) = self.snapshot {
            files.extend(s.parts.iter().map(|p| format!("data/snapshots/{}/{}", s.snapshot_id, p.name)));
        }
        for r in &self.runs {
            files.extend(r.parts.iter().map(|p| format!("data/runs/{}/{}/{}", r.run_id, r.node_id, p.name)));
        }
        files.sort();
        files
    }
}

impl TableState {
    /// Runs folded into the snapshot at `chain[i]` or any ancestor of it, by
    /// [`RunManifest::key`].
    pub fn folded(&self, i: usize) -> BTreeSet<&str> {
        self.chain[i..].iter().flat_map(|s| s.includes_runs.iter().map(String::as_str)).collect()
    }

    /// Resolve the read's FROM-source. Unbounded, it is the current snapshot plus the
    /// committed runs it omits (`store.bound-time.unbounded-latest`); under `as_of`, the
    /// newest reachable snapshot created at or before the bound plus the committed runs
    /// at or before it that snapshot omits (`store.bound-time.as-of`).
    pub fn resolve(&self, as_of: Option<Bound>) -> Result<Resolution<'_>, StoreError> {
        let admits = |t| as_of.is_none_or(|b| b.admits(t));
        let at = self.chain.iter().position(|s| admits(s.created_at));
        if at.is_none() && self.history_collected {
            if let (Some(b), Some(oldest)) = (as_of, self.chain.last()) {
                return Err(StoreError::StoreAsOfUnretained(format!(
                    "table `{}`: as_of {} precedes the oldest retained snapshot; the oldest answerable instant is {}",
                    self.table,
                    b.at.to_rfc3339_nanos(),
                    oldest.created_at.to_rfc3339_nanos()
                )));
            }
        }
        let folded = at.map(|i| self.folded(i)).unwrap_or_default();
        let mut runs: Vec<&RunManifest> =
            self.runs.iter().filter(|r| admits(r.committed_at) && !folded.contains(r.key().as_str())).collect();
        runs.sort_by(|a, b| (a.committed_at, &a.run_id).cmp(&(b.committed_at, &b.run_id)));
        let mut snapshot = at.map(|i| &self.chain[i]);

        // Under `replace`, a read covers the newest run carrying the source's complete
        // state plus every run after it; a marked zero-row run replaces it with empty.
        if self.write_mode == WriteMode::Replace {
            if let Some(frontier) = runs.iter().rposition(|r| !r.parts.is_empty() || r.replace_frontier) {
                snapshot = None;
                runs.drain(..frontier);
            }
        }
        Ok(Resolution { snapshot, runs })
    }

    /// Every committed run the current snapshot omits: what the next fold folds.
    pub fn unfolded_runs(&self) -> Vec<&RunManifest> {
        let folded = if self.chain.is_empty() { BTreeSet::new() } else { self.folded(0) };
        let mut runs: Vec<&RunManifest> = self.runs.iter().filter(|r| !folded.contains(r.key().as_str())).collect();
        runs.sort_by(|a, b| (a.committed_at, &a.run_id).cmp(&(b.committed_at, &b.run_id)));
        runs
    }
}
