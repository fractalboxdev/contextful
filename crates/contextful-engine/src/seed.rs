//! The seed loader (`run.seed`): it plans a seed's chunks under the `<table>#seed` scope,
//! runs each chunk through the one land path with the ceiling as the last shaping stage,
//! binds the live position on commit, and stamps and compares the seed source's fingerprint.
//!
//! A seed chunk's owner, cursor and chunk row key on `<table>#seed`, while its rows land in
//! the live table. The commit marker the live table reads therefore carries the live
//! position, never the seed's: a seed commit leaves an existing live position as it stood
//! and binds an unset one only through [`Ceiling::live_position`].

use crate::runner::{Engine, EngineError, RunSpec};
use contextful_core::coordinate::{Cas, Catalog, CursorRow};
use contextful_core::pipeline::seed::{probe, SeedCeiling, SeedProbe};
use contextful_core::run::advance::CursorKind;
use contextful_core::run::backfill::{ChunkRow, ChunkStatus, Window};
use contextful_core::run::effect::{EffectScope, EmissionSummary};
use contextful_core::run::own::{OwnerScope, SEED_SUFFIX};
use contextful_core::run::plan::Plan;
use contextful_core::run::ports::{BlobStore, Commit, Destination, JournalStore, Landed, Marker, Part, Pull, Row, Shape, Source, Stage, Types};
use contextful_core::run::record::{RunRow, RunStatus};
use contextful_core::run::{Failure, FailureTag, RunError};
use serde_json::{json, Value};
use std::collections::BTreeSet;
use std::path::Path;

/// The table segment a seed's chunks and cursor key on: `<table>#seed` (`run.seed.scope`).
pub fn seed_table(table: &str) -> String {
    format!("{table}{SEED_SUFFIX}")
}

/// One seed load's bounds: the ceiling every seeded stamp sorts below, and the live table's
/// cursor kind and field a commit binds.
#[derive(Debug, Clone, PartialEq)]
pub struct SeedLoad {
    pub ceiling: SeedCeiling,
    pub live_kind: CursorKind,
    pub live_field: Option<String>,
}

/// What one call of [`Seeding::run_next`] did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SeedOutcome {
    /// A seed chunk ran; the row records how it closed.
    Ran(Box<RunRow>),
    /// The committed seed's source answers its committed fingerprint; nothing ran.
    Unchanged,
    /// The committed seed's source answers no fingerprint, or committed none; nothing ran.
    Unavailable(String),
    /// No chunk is claimable and the seed is not committed: every remaining chunk runs elsewhere.
    Idle,
}

/// A seed's chunk plan and commit stamp.
#[derive(Debug, Clone, PartialEq)]
pub struct SeedState {
    pub chunks: Vec<ChunkRow>,
    /// `None` while uncommitted; `Some(None)` for a seed committed without a fingerprint.
    pub fingerprint: Option<Option<String>>,
}

impl SeedState {
    /// Whether the seed committed: every chunk done and the fingerprint stamped.
    pub fn committed(&self) -> bool {
        self.fingerprint.is_some()
    }
}

/// A cheap fingerprint of a seed file: its length and modification instant, read from one
/// stat that never opens the file (`run.seed.fingerprint`). `None` when the stat fails.
pub fn file_fingerprint(path: &Path) -> Option<String> {
    let meta = std::fs::metadata(path).ok()?;
    let modified = meta.modified().ok()?.duration_since(std::time::UNIX_EPOCH).ok()?;
    Some(format!("len={};mtime={}.{:09}", meta.len(), modified.as_secs(), modified.subsec_nanos()))
}

/// The seed loader over one engine's catalog and journal.
pub struct Seeding<'e, J: JournalStore, B: BlobStore> {
    engine: &'e Engine<J, B>,
}

impl<J: JournalStore, B: BlobStore> Engine<J, B> {
    /// The seed loader over this engine.
    pub fn seeding(&self) -> Seeding<'_, J, B> {
        Seeding { engine: self }
    }
}

impl<J: JournalStore, B: BlobStore> Seeding<'_, J, B> {
    /// Write the seed chunk plan of `pipeline_id`'s `table` under `<table>#seed`, keeping every
    /// row the catalog already holds.
    pub fn plan(&self, pipeline_id: &str, table: &str, windows: &[(String, Window)]) -> Result<Vec<ChunkRow>, EngineError> {
        self.engine.backfill().plan(pipeline_id, &seed_table(table), windows)
    }

    /// The seed's chunk rows and commit stamp.
    pub fn state(&self, pipeline_id: &str, table: &str) -> Result<SeedState, EngineError> {
        let catalog = &self.engine.catalog;
        let chunks = catalog.chunks(pipeline_id, &seed_table(table))?;
        let stamp = catalog.cursor_at(&OwnerScope::seed(pipeline_id, table))?;
        let fingerprint = stamp.position.map(|p| p.get("fingerprint").and_then(Value::as_str).map(str::to_string));
        Ok(SeedState { chunks, fingerprint })
    }

    /// Probe, then run the seed's next claimable chunk as `spec.run_id`. A committed seed
    /// compares `observed` with its stamp and runs nothing (`run.seed.source-changed`). A chunk
    /// passes `shape` and then the ceiling on the root batch before the write
    /// (`run.seed.ceiling-point`); the chunk completing the plan stamps `observed`.
    pub fn run_next(&self, load: &SeedLoad, spec: &RunSpec, source: &mut dyn Source, shape: &dyn Shape, dest: &mut dyn Destination, observed: Option<&str>) -> Result<SeedOutcome, EngineError> {
        let (pipeline_id, table) = (spec.plan.spec.pipeline.as_str(), spec.plan.spec.table.as_str());
        let state = self.state(pipeline_id, table)?;
        match probe(table, state.fingerprint.as_ref().map(Option::as_deref), observed)? {
            SeedProbe::Unchanged => return Ok(SeedOutcome::Unchanged),
            SeedProbe::Unavailable(warning) => return Ok(SeedOutcome::Unavailable(warning)),
            SeedProbe::Load => {}
        }
        let Some(chunk) = self.engine.backfill().next(pipeline_id, &seed_table(table))? else {
            return Ok(SeedOutcome::Idle);
        };
        let shaped = Ceilinged { inner: shape, ceiling: &load.ceiling, table };
        let mut bound = LiveBound { inner: &mut *dest, catalog: self.engine.catalog.as_ref(), load, pipeline_id, table };
        let row = self.engine.run_scoped(spec, chunk.scope(), source, &shaped, &mut bound)?;
        if row.status == RunStatus::Success {
            // The live cursor caches the marker the seed commit wrote.
            self.engine.reconcile(pipeline_id, table, &*dest)?;
            let after = self.engine.catalog.chunks(pipeline_id, &seed_table(table))?;
            if !after.is_empty() && after.iter().all(|c| c.status == ChunkStatus::Done) {
                self.stamp(pipeline_id, table, observed)?;
            }
        }
        Ok(SeedOutcome::Ran(Box::new(row)))
    }

    /// Stamp the seed committed with the fingerprint its source answered at commit.
    fn stamp(&self, pipeline_id: &str, table: &str, fingerprint: Option<&str>) -> Result<(), EngineError> {
        let scope = OwnerScope::seed(pipeline_id, table);
        let catalog = &self.engine.catalog;
        let held = catalog.cursor_at(&scope)?;
        let next = CursorRow { position: Some(json!({ "fingerprint": fingerprint })), version: 0, marker_run_id: None, marker_committed_at: Some(catalog.now()?) };
        match catalog.cursor_cas_at(&scope, held.version, next, None)? {
            Cas::Applied => Ok(()),
            Cas::VersionMoved => Err(Failure::new(FailureTag::Transient, format!("the seed stamp of {scope} moved under its commit")).into()),
            Cas::Fenced(e) => Err(Failure::new(FailureTag::Storage, e.to_string()).into()),
        }
    }
}

/// The caller's shape followed by the seed ceiling: the gate reads the batch normalize and
/// the chain answer, and refuses it before any write (`run.seed.ceiling-point`).
struct Ceilinged<'a> {
    inner: &'a dyn Shape,
    ceiling: &'a SeedCeiling,
    table: &'a str,
}

impl Shape for Ceilinged<'_> {
    fn recording_identity(&self) -> Result<Option<String>, RunError> {
        Ok(self.inner.recording_identity()?.map(|inner| format!("{inner}+seed-ceiling:{:?}", self.ceiling)))
    }

    fn recording_clock_columns(&self, field: &str) -> Result<Option<BTreeSet<String>>, RunError> {
        self.inner.recording_clock_columns(field)
    }

    fn shape(&self, rows: Vec<Row>) -> Result<Vec<Row>, RunError> {
        let rows = self.inner.shape(rows)?;
        self.ceiling.check(self.table, &rows)?;
        Ok(rows)
    }

    fn shape_types(&self, types: Types) -> Types {
        self.inner.shape_types(types)
    }
}

/// The live table's destination under a seed chunk: every call passes through, and a commit
/// carries the live position in place of the seed's (`run.seed.ceiling-binding`).
struct LiveBound<'a> {
    inner: &'a mut dyn Destination,
    catalog: &'a (dyn Catalog + Send + Sync),
    load: &'a SeedLoad,
    pipeline_id: &'a str,
    table: &'a str,
}

impl LiveBound<'_> {
    /// The live position the newest marker carries, else the catalog's; an unset one takes
    /// the ceiling where it binds.
    fn live_position(&self) -> Result<Option<Value>, Failure> {
        let held = match self.inner.newest_marker(self.pipeline_id, self.table)? {
            Some(marker) => marker.cursor,
            None => self.catalog.cursor(self.pipeline_id, self.table)?.position,
        };
        Ok(held.or_else(|| self.load.live_field.as_deref().and_then(|field| self.load.ceiling.below.live_position(self.load.live_kind, field))))
    }
}

impl Destination for LiveBound<'_> {
    fn recording_identity(&self, plan: &Plan) -> Result<Option<String>, Failure> {
        self.inner.recording_identity(plan)
    }
    fn validate_recorded_control(&self, plan: &Plan, pull: &Pull) -> Result<(), Failure> {
        self.inner.validate_recorded_control(plan, pull)
    }
    fn validate_recorded_clock(&self, plan: &Plan, columns: &BTreeSet<String>) -> Result<(), Failure> {
        self.inner.validate_recorded_clock(plan, columns)
    }
    fn prepare_recorded(&mut self, table: &str, rows: Vec<Row>, types: Types, load_id: &str) -> Result<Value, Failure> {
        self.inner.prepare_recorded(table, rows, types, load_id)
    }
    fn stage_recorded(&mut self, stage: Stage, prepared: &Value) -> Result<Part, Failure> {
        self.inner.stage_recorded(stage, prepared)
    }
    fn admit_effect_recorded(&self, table: &str, prepared: &Value, scope: &EffectScope) -> Result<EmissionSummary, Failure> {
        self.inner.admit_effect_recorded(table, prepared, scope)
    }
    fn stage_effect_recorded(&mut self, stage: Stage, prepared: &Value, scope: &EffectScope) -> Result<Part, Failure> {
        self.inner.stage_effect_recorded(stage, prepared, scope)
    }
    fn replaces(&self, table: &str) -> bool {
        self.inner.replaces(table)
    }
    fn stage_batch(&mut self, stage: Stage) -> Result<Part, Failure> {
        self.inner.stage_batch(stage)
    }
    fn commit(&mut self, commit: Commit, precommit: &dyn Fn() -> Result<(), Failure>) -> Result<Landed, Failure> {
        let cursor = self.live_position()?;
        self.inner.commit(Commit { cursor, cursor_kind: self.load.live_kind, replace_frontier: false, ..commit }, precommit)
    }
    fn discard(&mut self, table: &str, run_id: &str) -> Result<(), Failure> {
        self.inner.discard(table, run_id)
    }
    fn open_fence(&mut self, pipeline_id: &str, table: &str, fence: u64) -> Result<(), Failure> {
        self.inner.open_fence(pipeline_id, table, fence)
    }
    fn newest_marker(&self, pipeline_id: &str, table: &str) -> Result<Option<Marker>, Failure> {
        self.inner.newest_marker(pipeline_id, table)
    }
}
