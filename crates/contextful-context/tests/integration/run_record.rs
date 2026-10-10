//! `run.record`: the durable run record in the engine's reserved `_runs` table.

use crate::support::{at, decl, Fixture};
use contextful_context::run_record::{append, history, RecordedCatalog};
use contextful_core::coordinate::{Cas, Catalog, CursorRow, Lease, LeaseKey, LeaseRow};
use contextful_core::run::backfill::{Bound as Position, ChunkRow, ChunkStatus, Window};
use contextful_core::run::own::{ExecutionOwner, OwnerScope};
use contextful_core::run::record::{Phase, RunRow, RunStatus};
use contextful_core::run::{Failure, RunError};
use contextful_core::time::Instant;
use std::sync::{Arc, Mutex};
use contextful_core::store::bound_time::{Bound, Bounds};
use contextful_core::store::lay_out::NodeId;
use contextful_core::store::reserve::RUN_RECORD_TABLE;
use contextful_core::store::StoreError;
use serde_json::json;

fn row(run_id: &str, status: RunStatus, phase: Phase, started_at: &str) -> RunRow {
    serde_json::from_value(json!({
        "run_id": run_id, "pipeline_id": "feed", "table": "filings", "site_id": "site-a",
        "status": status, "started_at": started_at, "rows": 0, "bytes": 0, "batches": 0,
        "connector_id": "vendor", "connector_version": "1", "connector_hash": "h",
        "phase": phase, "execution_id": format!("x-{run_id}"),
    }))
    .unwrap()
}

fn as_of(v: &str) -> Bounds {
    Bounds { as_of: Some(Bound::parse(v).unwrap()), valid_as_of: None }
}

fn statuses(rows: &[RunRow]) -> Vec<(String, RunStatus)> {
    rows.iter().map(|r| (r.run_id.clone(), r.status)).collect()
}

/// Run history inherits the store's transaction-time bound, so one as-of selector rewinds it with every other table.
// spec: run.record.time-travel@4793298a
#[test]
fn an_as_of_bound_rewinds_run_history_with_every_other_table() {
    let f = Fixture::new();
    let node = NodeId::parse("ingest-a").unwrap();
    let filings = decl("name = \"filings\"");
    append(&f.store, &node, &row("r1", RunStatus::Running, Phase::Plan, "2030-01-01T00:00:00Z"), at("2030-01-01T00:00:00Z")).unwrap();
    f.land(&filings, "r1", json!([{"doc": "a"}]), "2030-01-01T00:30:00Z").unwrap();
    append(&f.store, &node, &row("r1", RunStatus::Success, Phase::Commit, "2030-01-01T00:00:00Z"), at("2030-01-01T01:00:00Z")).unwrap();
    append(&f.store, &node, &row("r2", RunStatus::Running, Phase::Plan, "2030-01-01T02:00:00Z"), at("2030-01-01T02:00:00Z")).unwrap();
    f.land(&filings, "r2", json!([{"doc": "b"}]), "2030-01-01T02:30:00Z").unwrap();

    let now = history(&f.store, Bounds::default()).unwrap();
    assert_eq!(statuses(&now), [("r2".into(), RunStatus::Running), ("r1".into(), RunStatus::Success)]);
    // One selector: the run record and the landed table rewind to the same instant.
    let bound = as_of("2030-01-01T00:45:00Z");
    assert_eq!(statuses(&history(&f.store, bound).unwrap()), [("r1".into(), RunStatus::Running)]);
    assert_eq!(f.scan(&filings, bound).unwrap().files.len(), 1);
    let bound = as_of("2030-01-01T01:30:00Z");
    assert_eq!(statuses(&history(&f.store, bound).unwrap()), [("r1".into(), RunStatus::Success)]);
    assert!(history(&f.store, as_of("2029-12-31T00:00:00Z")).unwrap().is_empty());
}

/// The latest row per run and phase wins, and a commit row answers for its run over the plan row.
#[test]
fn the_latest_row_per_run_and_phase_wins() {
    let f = Fixture::new();
    let node = NodeId::parse("ingest-a").unwrap();
    let start = "2030-01-01T00:00:00Z";
    append(&f.store, &node, &row("r1", RunStatus::Running, Phase::Plan, start), at(start)).unwrap();
    append(&f.store, &node, &row("r1", RunStatus::PartialFailure, Phase::Plan, start), at(start)).unwrap();
    assert_eq!(statuses(&history(&f.store, Bounds::default()).unwrap()), [("r1".into(), RunStatus::PartialFailure)]);
    append(&f.store, &node, &row("r1", RunStatus::Failed, Phase::Commit, start), at("2030-01-01T00:01:00Z")).unwrap();
    append(&f.store, &node, &row("r1", RunStatus::Running, Phase::Plan, start), at("2030-01-01T00:02:00Z")).unwrap();
    assert_eq!(statuses(&history(&f.store, Bounds::default()).unwrap()), [("r1".into(), RunStatus::Failed)]);
}

/// An inner catalog that keeps chunk rows in memory and answers nothing else.
#[derive(Default)]
struct Chunks(Mutex<Vec<ChunkRow>>);

impl Catalog for Chunks {
    fn chunk_at(&self, scope: &OwnerScope) -> Result<Option<ChunkRow>, Failure> {
        Ok(self.0.lock().unwrap().iter().find(|r| &OwnerScope::chunk(&r.pipeline_id, &r.table, &r.chunk) == scope).cloned())
    }
    fn chunks(&self, pipeline_id: &str, table: &str) -> Result<Vec<ChunkRow>, Failure> {
        Ok(self.0.lock().unwrap().iter().filter(|r| r.pipeline_id == pipeline_id && r.table == table).cloned().collect())
    }
    fn put_chunk(&self, row: &ChunkRow) -> Result<(), Failure> {
        self.0.lock().unwrap().push(row.clone());
        Ok(())
    }
    fn update_chunk(&self, scope: &OwnerScope, _retire: Option<&str>, f: &mut dyn FnMut(&mut ChunkRow)) -> Result<Option<ChunkRow>, Failure> {
        let mut rows = self.0.lock().unwrap();
        let Some(row) = rows.iter_mut().find(|r| &OwnerScope::chunk(&r.pipeline_id, &r.table, &r.chunk) == scope) else { return Ok(None) };
        f(row);
        Ok(Some(row.clone()))
    }
    fn retired_at(&self, scope: &OwnerScope) -> Result<Option<String>, Failure> {
        Ok(Some(format!("retired under {scope}")))
    }
    fn now(&self) -> Result<Instant, Failure> {
        unimplemented!()
    }
    fn acquire(&self, _: &LeaseKey, _: &str, _: u64) -> Result<Option<Lease>, Failure> {
        unimplemented!()
    }
    fn release(&self, _: &Lease) -> Result<(), Failure> {
        unimplemented!()
    }
    fn renew(&self, _: &Lease, _: u64) -> Result<Option<Lease>, Failure> {
        unimplemented!()
    }
    fn lease_holds(&self, _: &Lease) -> Result<bool, Failure> {
        unimplemented!()
    }
    fn lease_row(&self, _: &LeaseKey) -> Result<LeaseRow, Failure> {
        unimplemented!()
    }
    fn cursor_at(&self, _: &OwnerScope) -> Result<CursorRow, Failure> {
        unimplemented!()
    }
    fn cursor_cas_at(&self, _: &OwnerScope, _: u64, _: CursorRow, _: Option<&Lease>) -> Result<Cas, Failure> {
        unimplemented!()
    }
    fn owner_at(&self, _: &OwnerScope) -> Result<Option<ExecutionOwner>, Failure> {
        unimplemented!()
    }
    fn put_owner(&self, _: &ExecutionOwner) -> Result<(), Failure> {
        unimplemented!()
    }
    fn retire_at(&self, _: &OwnerScope, _: &str, _: Option<(CursorRow, u64)>, _: Option<&Lease>) -> Result<Cas, Failure> {
        unimplemented!()
    }
    fn put_run(&self, _: &RunRow) -> Result<(), Failure> {
        unimplemented!()
    }
    fn run(&self, _: &str) -> Result<Option<RunRow>, Failure> {
        unimplemented!()
    }
    fn runs(&self, _: Option<&str>) -> Result<Vec<RunRow>, Failure> {
        unimplemented!()
    }
    fn update_run(&self, _: &str, _: &mut dyn FnMut(&mut RunRow) -> Result<(), RunError>) -> Result<Option<Result<RunRow, RunError>>, Failure> {
        unimplemented!()
    }
}

/// The recording catalog hands every chunk-plan read and write to the catalog it wraps.
#[test]
fn the_recorded_catalog_keeps_the_wrapped_catalogs_chunk_plan() {
    let f = Fixture::new();
    let inner = Arc::new(Chunks::default());
    let recorded = RecordedCatalog::new(inner.clone(), f.store.clone(), Box::new(|_| Ok(NodeId::parse("ingest-a").unwrap())));
    let window = Window::new(Position::Int(0), Position::Int(10)).unwrap();
    recorded.put_chunk(&ChunkRow::planned("feed", "filings", "c0", 0, window)).unwrap();
    assert_eq!(inner.0.lock().unwrap().len(), 1);
    let scope = OwnerScope::chunk("feed", "filings", "c0");
    assert_eq!(recorded.chunks("feed", "filings").unwrap().len(), 1);
    assert_eq!(recorded.chunk_at(&scope).unwrap().map(|r| r.chunk), Some("c0".to_string()));
    let claimed = recorded.update_chunk(&scope, None, &mut |r| r.status = ChunkStatus::Running).unwrap();
    assert_eq!(claimed.map(|r| r.status), Some(ChunkStatus::Running));
    assert_eq!(inner.0.lock().unwrap()[0].status, ChunkStatus::Running);
}

/// The recording catalog reads a scope's last retirement from the catalog it wraps.
#[test]
fn the_recorded_catalog_reads_the_wrapped_catalogs_last_retirement() {
    let f = Fixture::new();
    let recorded = RecordedCatalog::new(Arc::new(Chunks::default()), f.store.clone(), Box::new(|_| Ok(NodeId::parse("ingest-a").unwrap())));
    let scope = OwnerScope::table("feed", "filings");
    assert_eq!(recorded.retired_at(&scope).unwrap(), Some(format!("retired under {scope}")));
}

/// A producer landing into the run record still refuses; only the engine appends to it.
#[test]
fn a_producer_cannot_land_into_the_run_record() {
    let f = Fixture::new();
    append(&f.store, &NodeId::parse("ingest-a").unwrap(), &row("r1", RunStatus::Running, Phase::Plan, "2030-01-01T00:00:00Z"), at("2030-01-01T00:00:00Z")).unwrap();
    let err = f.land(&decl(&format!("name = \"{RUN_RECORD_TABLE}\"")), "forged", json!([{"run_id": "r9"}]), "2030-01-01T00:01:00Z").unwrap_err();
    assert!(matches!(err.store(), Some(StoreError::StoreReservedTableName(_))), "{err}");
    assert_eq!(history(&f.store, Bounds::default()).unwrap().len(), 1);
}
