//! The durable run record (`run.record.reserved-table`): the engine's reserved `_runs`
//! table, holding append-only run rows, one at plan and one at commit. The latest row per
//! run and phase wins, and the machine catalog is a cache in front of the table: a
//! [`RecordedCatalog`] appends as it caches, and [`restore`] refills an empty catalog
//! (`run.record.rebuild-restores-history`). The table reads under the store's bounds
//! like any other (`run.record.time-travel`).

use crate::error::{ContextError, Result};
use crate::land::{land_engine_owned, Batch, RunContext};
use crate::rows::batch_rows;
use crate::scan::scan;
use crate::store::Store;
use contextful_core::coordinate::{Cas, Catalog, CursorRow, Lease, LeaseKey, LeaseRow};
use contextful_core::run::backfill::ChunkRow;
use contextful_core::run::failure::FailureTag;
use contextful_core::run::own::{ExecutionOwner, OwnerScope};
use contextful_core::run::record::{Phase, RunRow};
use contextful_core::run::{Failure, RunError};
use contextful_core::store::bound_time::Bounds;
use contextful_core::store::declare::TableDecl;
use contextful_core::store::lay_out::{NodeId, RunManifest};
use contextful_core::store::reconcile::ColumnType;
use contextful_core::store::reserve::{Injection, COMMIT_SEQ, RUN_RECORD_TABLE};
use contextful_core::time::Instant;
use serde_json::{Map, Value};
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

/// The columns each appended row carries; `record` holds the whole run row as JSON.
const COLUMNS: [&str; 8] = ["run_id", "pipeline_id", "site_id", "status", "phase", "started_at", "ended_at", "record"];

/// The run record's declaration: unkeyed, so every appended row stays readable.
pub fn decl() -> TableDecl {
    TableDecl::named(RUN_RECORD_TABLE)
}

fn phase_name(phase: Phase) -> &'static str {
    match phase {
        Phase::Plan => "plan",
        Phase::Commit => "commit",
    }
}

/// Append `row` to the run record at `at`, as one store run of its own on `node`.
pub fn append(store: &Store, node: &NodeId, row: &RunRow, at: Instant) -> Result<RunManifest> {
    let record = serde_json::to_string(row).map_err(|e| ContextError::Invalid(format!("run `{}`: {e}", row.run_id)))?;
    let mut fields = Map::new();
    fields.insert("run_id".into(), Value::String(row.run_id.clone()));
    fields.insert("pipeline_id".into(), Value::String(row.pipeline_id.clone()));
    fields.insert("site_id".into(), Value::String(row.site_id.clone()));
    fields.insert("status".into(), Value::String(row.status.name().into()));
    fields.insert("phase".into(), Value::String(phase_name(row.phase).into()));
    fields.insert("started_at".into(), Value::String(row.started_at.to_rfc3339_nanos()));
    fields.insert("ended_at".into(), row.ended_at.map_or(Value::Null, |t| Value::String(t.to_rfc3339_nanos())));
    fields.insert("record".into(), Value::String(record));
    let batch = Batch { rows: vec![fields], types: COLUMNS.iter().map(|c| (c.to_string(), ColumnType::Utf8)).collect() };
    let mut nonce = [0u8; 4];
    getrandom::fill(&mut nonce).map_err(|e| ContextError::Invalid(format!("run record nonce: {e}")))?;
    let store_run = format!("{}-{}-{}", phase_name(row.phase), at.unix_nanos(), crate::store::hex(&nonce));
    let injection = Injection { run_id: store_run, site_id: row.site_id.clone(), batch_seq: None, authored_by: None, taint: None };
    land_engine_owned(store, &decl(), &batch, &RunContext { node: node.clone(), injection, committed_at: at })
}

/// The run rows the record holds under `bounds`: per run, its latest commit row, or its
/// latest plan row where none committed, newest start first.
pub fn history(store: &Store, bounds: Bounds) -> Result<Vec<RunRow>> {
    if store.try_schema(RUN_RECORD_TABLE)?.is_none() {
        return Ok(Vec::new());
    }
    let s = scan(store, &decl(), bounds)?;
    // (run, phase) -> (commit seq, row): a later commit sequence wins.
    let mut latest: BTreeMap<(String, &'static str), (i64, RunRow)> = BTreeMap::new();
    for f in &s.files {
        for batch in store.read_parquet(&store.logical_path(f)?)? {
            for cells in batch_rows(&batch, &["record", COMMIT_SEQ])? {
                let seq = cells.get(COMMIT_SEQ).and_then(Value::as_i64).unwrap_or(i64::MIN);
                let Some(text) = cells.get("record").and_then(Value::as_str) else { continue };
                let row: RunRow = serde_json::from_str(text).map_err(|e| ContextError::Invalid(format!("`{RUN_RECORD_TABLE}` row: {e}")))?;
                let key = (row.run_id.clone(), phase_name(row.phase));
                if latest.get(&key).is_none_or(|(held, _)| *held < seq) {
                    latest.insert(key, (seq, row));
                }
            }
        }
    }
    let mut runs: BTreeMap<String, RunRow> = BTreeMap::new();
    for ((run_id, phase), (_, row)) in latest {
        if phase == "commit" || !runs.contains_key(&run_id) {
            runs.insert(run_id, row);
        }
    }
    let mut out: Vec<RunRow> = runs.into_values().collect();
    out.sort_by(|a, b| b.started_at.cmp(&a.started_at).then_with(|| b.run_id.cmp(&a.run_id)));
    Ok(out)
}

/// Fill a catalog holding no run row with every run the record holds, returning the
/// restored run ids. A catalog holding any run row is a live cache and stays untouched
/// (`store.lay-out.machine-catalog`).
pub fn restore(store: &Store, catalog: &dyn Catalog) -> Result<Vec<String>> {
    let mut restored = Vec::new();
    if !catalog.runs(None).map_err(ContextError::Catalog)?.is_empty() {
        return Ok(restored);
    }
    for row in history(store, Bounds::default())? {
        if catalog.run(&row.run_id).map_err(ContextError::Catalog)?.is_none() {
            catalog.put_run(&row).map_err(ContextError::Catalog)?;
            restored.push(row.run_id);
        }
    }
    Ok(restored)
}

/// Resolves the node the run record lands on, at the first append.
pub type NodeResolver = Box<dyn Fn(&Store) -> Result<NodeId> + Send + Sync>;

/// A catalog caching run rows in front of the run record: the row a run opens with is
/// appended before the cache holds it, and the row that moves it to commit as the cache
/// takes it. A restored row whose owner lease lapsed is reaped as any orphan is
/// (`run.record.orphan-reap`).
pub struct RecordedCatalog {
    inner: Arc<dyn Catalog + Send + Sync>,
    store: Store,
    resolve: NodeResolver,
    node: Mutex<Option<NodeId>>,
}

fn storage(e: ContextError) -> Failure {
    Failure::new(FailureTag::Storage, format!("run record: {e}"))
}

impl RecordedCatalog {
    pub fn new(inner: Arc<dyn Catalog + Send + Sync>, store: Store, resolve: NodeResolver) -> RecordedCatalog {
        RecordedCatalog { inner, store, resolve, node: Mutex::new(None) }
    }

    fn record(&self, row: &RunRow) -> std::result::Result<(), Failure> {
        let node = {
            let mut held = self.node.lock().unwrap_or_else(|p| p.into_inner());
            match &*held {
                Some(n) => n.clone(),
                None => {
                    let n = (self.resolve)(&self.store).map_err(storage)?;
                    *held = Some(n.clone());
                    n
                }
            }
        };
        let at = self.inner.now()?;
        append(&self.store, &node, row, at).map(|_| ()).map_err(storage)
    }
}

impl Catalog for RecordedCatalog {
    fn now(&self) -> std::result::Result<Instant, Failure> {
        self.inner.now()
    }
    fn acquire(&self, key: &LeaseKey, holder: &str, ttl_secs: u64) -> std::result::Result<Option<Lease>, Failure> {
        self.inner.acquire(key, holder, ttl_secs)
    }
    fn release(&self, lease: &Lease) -> std::result::Result<(), Failure> {
        self.inner.release(lease)
    }
    fn renew(&self, lease: &Lease, ttl_secs: u64) -> std::result::Result<Option<Lease>, Failure> {
        self.inner.renew(lease, ttl_secs)
    }
    fn lease_holds(&self, lease: &Lease) -> std::result::Result<bool, Failure> {
        self.inner.lease_holds(lease)
    }
    fn lease_row(&self, key: &LeaseKey) -> std::result::Result<LeaseRow, Failure> {
        self.inner.lease_row(key)
    }
    fn cursor_at(&self, scope: &OwnerScope) -> std::result::Result<CursorRow, Failure> {
        self.inner.cursor_at(scope)
    }
    fn cursor_cas_at(&self, scope: &OwnerScope, expected_version: u64, next: CursorRow, fence: Option<&Lease>) -> std::result::Result<Cas, Failure> {
        self.inner.cursor_cas_at(scope, expected_version, next, fence)
    }
    fn owner_at(&self, scope: &OwnerScope) -> std::result::Result<Option<ExecutionOwner>, Failure> {
        self.inner.owner_at(scope)
    }
    fn put_owner(&self, owner: &ExecutionOwner) -> std::result::Result<(), Failure> {
        self.inner.put_owner(owner)
    }
    fn retire_at(&self, scope: &OwnerScope, execution_id: &str, cursor: Option<(CursorRow, u64)>, fence: Option<&Lease>) -> std::result::Result<Cas, Failure> {
        self.inner.retire_at(scope, execution_id, cursor, fence)
    }
    fn retired_at(&self, scope: &OwnerScope) -> std::result::Result<Option<String>, Failure> {
        self.inner.retired_at(scope)
    }
    fn chunk_at(&self, scope: &OwnerScope) -> std::result::Result<Option<ChunkRow>, Failure> {
        self.inner.chunk_at(scope)
    }
    fn chunks(&self, pipeline_id: &str, table: &str) -> std::result::Result<Vec<ChunkRow>, Failure> {
        self.inner.chunks(pipeline_id, table)
    }
    fn put_chunk(&self, row: &ChunkRow) -> std::result::Result<(), Failure> {
        self.inner.put_chunk(row)
    }
    fn update_chunk(&self, scope: &OwnerScope, retire: Option<&str>, f: &mut dyn FnMut(&mut ChunkRow)) -> std::result::Result<Option<ChunkRow>, Failure> {
        self.inner.update_chunk(scope, retire, f)
    }
    fn put_run(&self, row: &RunRow) -> std::result::Result<(), Failure> {
        self.record(row)?;
        self.inner.put_run(row)
    }
    fn run(&self, run_id: &str) -> std::result::Result<Option<RunRow>, Failure> {
        self.inner.run(run_id)
    }
    fn runs(&self, pipeline_id: Option<&str>) -> std::result::Result<Vec<RunRow>, Failure> {
        self.inner.runs(pipeline_id)
    }
    fn last_run_start(&self, pipeline_id: &str) -> std::result::Result<Option<Instant>, Failure> {
        self.inner.last_run_start(pipeline_id)
    }
    fn update_run(
        &self,
        run_id: &str,
        f: &mut dyn FnMut(&mut RunRow) -> std::result::Result<(), RunError>,
    ) -> std::result::Result<Option<std::result::Result<RunRow, RunError>>, Failure> {
        let mut moved = false;
        let out = self.inner.update_run(run_id, &mut |r| {
            let before = r.phase;
            f(r)?;
            moved = before != r.phase;
            Ok(())
        })?;
        if let Some(Ok(row)) = &out {
            if moved {
                self.record(row)?;
            }
        }
        Ok(out)
    }
}
