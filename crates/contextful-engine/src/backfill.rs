//! The backfill chunk scheduler (`run.backfill`): it writes a table's chunk plan into the
//! catalog, hands out the first claimable chunk in plan order, opens each chunk's execution
//! under the chunk's own scope, and rewinds a window of chunks back to pending.
//!
//! A chunk's execution settles its row: a commit marks it done in the transaction that
//! retires its owner (`run.own.retirement`), and a stop returns it to pending
//! (`run.cancel.resumable-remains`).

use crate::execution::Execution;
use crate::runner::{Engine, EngineError};
use contextful_core::run::backfill::{ChunkRow, Window};
use contextful_core::run::ports::{BlobStore, JournalStore, OpenExecution};
use contextful_core::run::RunError;

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
        let catalog = &self.engine.catalog;
        for (ordinal, (chunk, window)) in windows.iter().enumerate() {
            let planned = ChunkRow::planned(pipeline_id, table, chunk, ordinal as u32, window.clone());
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
