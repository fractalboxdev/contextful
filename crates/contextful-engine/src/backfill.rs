//! The backfill chunk scheduler (`run.backfill`): it writes a table's chunk plan into the
//! catalog, hands out the first claimable chunk in plan order, opens each chunk's execution
//! under the chunk's own scope, and rewinds a window of chunks back to pending.
//!
//! A chunk's execution settles its row: a commit marks it done in the transaction that
//! retires its owner (`run.own.retirement`), and a stop returns it to pending
//! (`run.cancel.resumable-remains`).
//!
//! A worker landing files instead leases the chunk by node and attempt, writes under its
//! attempt path, and commits through one conditional rewrite of the chunk row: the row,
//! never a directory, says which attempt's files count (`run.backfill.commit-signal`).

use crate::execution::Execution;
use crate::fsutil::storage;
use crate::runner::{Engine, EngineError};
use contextful_core::run::advance::CursorKind;
use contextful_core::run::backfill::{chunk_path, path_segment, BackfillBlock, ChunkLease, ChunkPlan, ChunkRow, ChunkStatus, Window};
use contextful_core::run::own::OwnerScope;
use contextful_core::run::ports::{BlobStore, JournalStore, OpenExecution};
use contextful_core::run::RunError;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

/// One worker's lease on one chunk of a plan, and the run its files land under.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChunkHold {
    pub pipeline_id: String,
    pub table: String,
    pub chunk: String,
    pub run_id: String,
    pub lease: ChunkLease,
}

impl ChunkHold {
    /// The path, relative to the landing root, this attempt writes under.
    pub fn path(&self) -> PathBuf {
        chunk_path(&self.run_id, &self.lease.node, &self.chunk, self.lease.attempt)
    }

    fn scope(&self) -> OwnerScope {
        OwnerScope::chunk(&self.pipeline_id, &self.table, &self.chunk)
    }
}

/// The chunk scheduler over one engine's catalog and journal.
pub struct Backfill<'e, J: JournalStore, B: BlobStore> {
    engine: &'e Engine<J, B>,
}

impl<J: JournalStore, B: BlobStore> Engine<J, B> {
    /// The backfill chunk scheduler over this engine.
    pub fn backfill(&self) -> Backfill<'_, J, B> {
        Backfill { engine: self }
    }
}

impl<'e, J: JournalStore, B: BlobStore> Backfill<'e, J, B> {
    /// Write the chunk plan of `pipeline_id`'s `table`, one pending row per named window in
    /// plan order. A chunk the catalog already holds keeps its row, so an interrupted plan
    /// resumes where it stood. Answers every row of the plan.
    pub fn plan(&self, pipeline_id: &str, table: &str, windows: &[(String, Window)]) -> Result<Vec<ChunkRow>, EngineError> {
        self.write_plan(pipeline_id, table, windows, false)
    }

    /// Write the chunk plan `block` shapes over `[start_from, upto)` for `table`'s cursor
    /// `kind` (`run.backfill.chunk-plan`), keeping every row the catalog already holds.
    pub fn plan_range(&self, pipeline_id: &str, table: &str, kind: CursorKind, block: &BackfillBlock, upto: i64) -> Result<Vec<ChunkRow>, EngineError> {
        let plan = ChunkPlan::of(kind, block, upto)?;
        self.write_plan(pipeline_id, table, &plan.chunks, plan.sequential)
    }

    fn write_plan(&self, pipeline_id: &str, table: &str, windows: &[(String, Window)], sequential: bool) -> Result<Vec<ChunkRow>, EngineError> {
        let catalog = &self.engine.catalog;
        for (ordinal, (chunk, window)) in windows.iter().enumerate() {
            let planned = ChunkRow { sequential, ..ChunkRow::planned(pipeline_id, table, chunk, ordinal as u32, window.clone()) };
            if catalog.chunk_at(&planned.scope())?.is_none() {
                catalog.put_chunk(&planned)?;
            }
        }
        Ok(catalog.chunks(pipeline_id, table)?)
    }

    /// The first chunk in plan order a worker may claim; `None` once every chunk runs or is done.
    pub fn next(&self, pipeline_id: &str, table: &str) -> Result<Option<ChunkRow>, EngineError> {
        Ok(self.engine.catalog.chunks(pipeline_id, table)?.into_iter().find(ChunkRow::claimable))
    }

    /// Lease the first chunk in plan order `node` may take, for `run_id`'s files: a pending
    /// or failed chunk, or one whose lease expired. A sequential plan offers only its first
    /// chunk not done. `None` when no chunk is free.
    pub fn lease(&self, pipeline_id: &str, table: &str, run_id: &str, node: &str) -> Result<Option<ChunkHold>, EngineError> {
        let catalog = &self.engine.catalog;
        let now = catalog.now()?;
        let plan = catalog.chunks(pipeline_id, table)?;
        let open: Vec<&ChunkRow> = if plan.iter().any(|c| c.sequential) {
            plan.iter().find(|c| c.status != ChunkStatus::Done).into_iter().collect()
        } else {
            plan.iter().collect()
        };
        for chunk in open.into_iter().filter(|c| c.leasable(now)) {
            let mut taken = None;
            catalog.update_chunk(&chunk.scope(), None, &mut |row| taken = row.lease(node, now))?;
            if let Some(lease) = taken {
                return Ok(Some(ChunkHold { pipeline_id: pipeline_id.into(), table: table.into(), chunk: chunk.chunk.clone(), run_id: run_id.into(), lease }));
            }
        }
        Ok(None)
    }

    /// Renew `hold`'s lease from now; false once a later attempt superseded it or the chunk
    /// left running.
    pub fn renew(&self, hold: &mut ChunkHold) -> Result<bool, EngineError> {
        let now = self.engine.catalog.now()?;
        let mut renewed = None;
        self.engine.catalog.update_chunk(&hold.scope(), None, &mut |row| renewed = row.renew(&hold.lease, now))?;
        Ok(match renewed {
            Some(lease) => {
                hold.lease = lease;
                true
            }
            None => false,
        })
    }

    /// Commit `hold`'s attempt in one catalog update conditioned on `attempt = :mine AND
    /// status != 'done'`. False when the update matches no row: that attempt's files stay
    /// uncommitted (`run.backfill.fenced-commit`).
    pub fn commit(&self, hold: &ChunkHold) -> Result<bool, EngineError> {
        let now = self.engine.catalog.now()?;
        let mut applied = false;
        self.engine.catalog.update_chunk(&hold.scope(), None, &mut |row| applied = row.commit_fenced(hold.lease.attempt, now))?;
        Ok(applied)
    }

    /// Remove every attempt path of `run_id` under `root` for the plan's chunks that no done
    /// row commits and no live lease holds, last modified at or before `cutoff`, the edge of
    /// the retention window. Answers the removed paths in order (`run.backfill.commit-signal`).
    pub fn collect(&self, pipeline_id: &str, table: &str, run_id: &str, root: &Path, cutoff: SystemTime) -> Result<Vec<PathBuf>, EngineError> {
        let now = self.engine.catalog.now()?;
        let plan = self.engine.catalog.chunks(pipeline_id, table)?;
        let mut removed = Vec::new();
        for node_dir in dirs(&root.join(path_segment(run_id)))? {
            for chunk in &plan {
                let committed = chunk.committed_path(run_id).map(|c| root.join(c));
                for at in dirs(&node_dir.join(path_segment(&chunk.chunk)))? {
                    let Some(attempt) = at.file_name().and_then(|a| a.to_str()).and_then(|a| a.parse::<u32>().ok()) else { continue };
                    if committed.as_ref() == Some(&at) || chunk.live_attempt(attempt, now) {
                        continue;
                    }
                    let modified = std::fs::metadata(&at).and_then(|m| m.modified()).map_err(|e| storage(&at, e))?;
                    if modified <= cutoff {
                        std::fs::remove_dir_all(&at).map_err(|e| storage(&at, e))?;
                        removed.push(at);
                    }
                }
            }
        }
        removed.sort();
        Ok(removed)
    }

    /// Open an execution over `chunk` under the chunk's scope; the claim marks the chunk
    /// running and raises its attempt count.
    pub fn open(&self, chunk: &ChunkRow, open: &OpenExecution) -> Result<Execution<'e, J, B>, EngineError> {
        self.engine.open_execution(&OpenExecution { scope: chunk.scope(), ..open.clone() })
    }

    /// Rewind every chunk of the plan overlapping `window`: each returns to pending and
    /// retires the owner its scope holds, collecting that owner's journal, in the one catalog
    /// transaction that rewrites its row (`run.own.pin-recovery`). A window on another scale
    /// than the plan's raises `PipelineRewindWindowInvalid`; a window overlapping no chunk
    /// rewinds nothing. Answers the rewound rows.
    pub fn rewind(&self, pipeline_id: &str, table: &str, window: &Window) -> Result<Vec<ChunkRow>, EngineError> {
        let catalog = &self.engine.catalog;
        let plan = catalog.chunks(pipeline_id, table)?;
        if let Some(other) = plan.iter().find(|c| !window.comparable(&c.predicate)) {
            return Err(RunError::PipelineRewindWindowInvalid(format!(
                "the window [{}, {}) is on another scale than chunk `{}` of `{pipeline_id}`/`{table}`",
                window.start, window.end, other.chunk
            ))
            .into());
        }
        let mut rewound = Vec::new();
        for chunk in plan.iter().filter(|c| c.predicate.overlaps(window)) {
            let scope = chunk.scope();
            let owner = catalog.owner_at(&scope)?;
            let retire = owner.as_ref().map(|o| o.execution_id.as_str());
            if let Some(row) = catalog.update_chunk(&scope, retire, &mut ChunkRow::rewind)? {
                rewound.push(row);
            }
            if let Some(owner) = owner {
                self.engine.journal.collect(&owner.execution_id)?;
            }
        }
        Ok(rewound)
    }
}

/// The subdirectories of `dir`, in name order; none when it is absent.
fn dirs(dir: &Path) -> Result<Vec<PathBuf>, EngineError> {
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(storage(dir, e).into()),
    };
    let mut out = Vec::new();
    for entry in entries {
        let path = entry.map_err(|e| storage(dir, e))?.path();
        if path.is_dir() {
            out.push(path);
        }
    }
    out.sort();
    Ok(out)
}
