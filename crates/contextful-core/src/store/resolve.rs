//! A table's FROM-source: the snapshot and committed runs a read covers, and the
//! explicit sorted file list they resolve to (`store.reconcile.explicit-file-list`).

use super::bound_time::Bound;
use super::declare::WriteMode;
use super::lay_out::{RunManifest, SnapshotId, SnapshotManifest};
use super::StoreError;
use std::collections::{BTreeMap, BTreeSet};

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
    /// Chain snapshots a collection reduced to their `_manifest.json`, by id: the chain still
    /// names their folded runs, and no read resolves to them (`store.fold.manifest-kept`).
    pub collected: BTreeSet<String>,
    /// The rows of the valid folded-runs index the walk stopped at, when it found one: the
    /// chain then ends there or at the read's bound (`store.lay-out.folded-index`).
    pub index: Option<FoldedSet>,
}

/// Each run a valid folded-runs index lists, with the snapshot folding it, read at
/// `chain[at]` (`store.lay-out.folded-index`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FoldedSet {
    pub at: usize,
    pub rows: BTreeMap<String, SnapshotId>,
    /// The same rows by folding snapshot.
    by_snapshot: BTreeMap<SnapshotId, Vec<String>>,
}

impl FoldedSet {
    /// The index's `rows`, read at `chain[at]`.
    pub fn new(at: usize, rows: BTreeMap<String, SnapshotId>) -> Self {
        let mut by_snapshot: BTreeMap<SnapshotId, Vec<String>> = BTreeMap::new();
        for (run, snapshot) in &rows {
            by_snapshot.entry(snapshot.clone()).or_default().push(run.clone());
        }
        FoldedSet { at, rows, by_snapshot }
    }
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
            // A part carried by reference resolves to its origin's directory
            // (`store.fold.carry-by-reference`).
            files.extend(s.part_files());
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
    /// [`RunManifest::key`]: the `includes_runs` walked down to the index, then the index's
    /// rows folded at or before `chain[i]`, snapshot ids sorting in commit order.
    pub fn folded(&self, i: usize) -> BTreeSet<&str> {
        match &self.index {
            None => self.chain[i..].iter().flat_map(|s| s.includes_runs.iter().map(String::as_str)).collect(),
            Some(ix) => {
                let id = &self.chain[i].snapshot_id;
                let walked = self.chain[i..ix.at.max(i)].iter().flat_map(|s| s.includes_runs.iter().map(String::as_str));
                walked.chain(ix.rows.iter().filter(|(_, s)| *s <= id).map(|(k, _)| k.as_str())).collect()
            }
        }
    }

    /// The runs `chain[i]` itself folds.
    fn own(&self, i: usize) -> Vec<&str> {
        match &self.index {
            Some(ix) if i >= ix.at => ix.by_snapshot.get(&self.chain[i].snapshot_id).map_or_else(Vec::new, |runs| runs.iter().map(String::as_str).collect()),
            _ => self.chain[i].includes_runs.iter().map(String::as_str).collect(),
        }
    }

    /// Resolve the read's FROM-source. Unbounded, it is the current snapshot plus the
    /// committed runs it omits (`store.bound-time.unbounded-latest`); under `as_of`, the
    /// newest reachable snapshot created at or before the bound plus the committed runs
    /// at or before it that snapshot omits (`store.bound-time.as-of`).
    pub fn resolve(&self, as_of: Option<Bound>) -> Result<Resolution<'_>, StoreError> {
        let admits = |t| as_of.is_none_or(|b| b.admits(t));
        let at = self.chain.iter().position(|s| admits(s.created_at));
        // Collected snapshots sit behind every retained one, superseded longest ago.
        let collected = self.chain.iter().take_while(|s| !self.collected.contains(&s.snapshot_id.to_string())).count();
        // A snapshot whose successors fold a run absent here answers no bound before them: the
        // run may have committed inside the bound (`store.bound-time.as-of-unretained`).
        let present: BTreeSet<String> = self.runs.iter().map(RunManifest::key).collect();
        let gapped = (0..self.chain.len()).find(|i| self.own(*i).iter().any(|r| !present.contains(*r))).map_or(self.chain.len(), |j| j + 1);
        let retained = collected.min(gapped);
        let unretained = match at {
            Some(i) => i >= retained,
            None => self.history_collected,
        };
        if unretained {
            if let (Some(b), Some(oldest)) = (as_of, self.chain[..retained.max(1).min(self.chain.len())].last()) {
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
