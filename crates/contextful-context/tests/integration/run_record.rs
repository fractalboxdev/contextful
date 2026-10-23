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
use contextful_core::store::reserve::{ITEM_RECORD_TABLE, RUN_RECORD_TABLE};
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
    Bounds { as_of: Some(Bound::parse(v).unwrap()), valid_as_of: None, available_as_of: None, ..Bounds::default() }
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

/// An append runs a pass on the run record once its trigger fires: 25 unit runs, 50 store
/// runs, leave one snapshot folding all 50 and history reading every run. A pass that fails
/// leaves its append succeeding, and the next append runs it; 6 h after a pass, one more
/// append fires the next.
// spec: run.record.record-folds@29e1db69
#[test]
fn an_append_folds_the_run_record_once_its_trigger_fires() {
    let f = Fixture::new();
    let node = NodeId::parse("ingest-a").unwrap();
    let record = contextful_context::run_record::decl();
    let base = at("2030-01-01T00:00:00Z");
    let unit = |i: u64| {
        let started = base.plus_secs(i * 60);
        let id = format!("r{i:03}");
        append(&f.store, &node, &row(&id, RunStatus::Running, Phase::Plan, &started.to_rfc3339()), started).unwrap();
        append(&f.store, &node, &row(&id, RunStatus::Success, Phase::Commit, &started.to_rfc3339()), started.plus_secs(1)).unwrap();
    };
    for i in 0..25 {
        unit(i);
    }
    let state = f.store.state(&record).unwrap();
    assert_eq!(state.chain.len(), 1, "one pass folded the run record");
    assert_eq!(state.chain[0].includes_runs.len(), 50);
    assert!(state.unfolded_runs().is_empty());
    let read = history(&f.store, Bounds::default()).unwrap();
    assert_eq!(read.len(), 25);
    assert!(read.iter().all(|r| r.status == RunStatus::Success), "{:?}", statuses(&read));

    // A pass that cannot take the pointer lock fails beside a durable append. A directory
    // at the lock path refuses the open for every user, root included, where a read-only
    // mode bit binds only an unprivileged one.
    let lock = f.table_dir(RUN_RECORD_TABLE).join("_pointer.json.lock");
    std::fs::create_dir(&lock).unwrap();
    std::fs::write(lock.join("held"), b"").unwrap();
    for i in 25..50 {
        unit(i);
    }
    std::fs::remove_dir_all(&lock).unwrap();
    assert_eq!(f.store.state(&record).unwrap().chain.len(), 1, "the failed pass published nothing");
    assert_eq!(history(&f.store, Bounds::default()).unwrap().len(), 50);
    unit(50);
    let state = f.store.state(&record).unwrap();
    assert_eq!(state.chain.len(), 2, "the next append ran the pass");
    assert_eq!(state.unfolded_runs().len(), 1, "the plan row's pass folded 51 runs; the commit row follows it");

    // 6 h after the previous pass, one append fires the next.
    let late = base.plus_secs(7 * 3600);
    append(&f.store, &node, &row("r-late", RunStatus::Running, Phase::Plan, &late.to_rfc3339()), late).unwrap();
    let state = f.store.state(&record).unwrap();
    assert_eq!(state.chain.len(), 3);
    assert!(state.unfolded_runs().is_empty());
    assert_eq!(history(&f.store, Bounds::default()).unwrap().len(), 52);
}

/// An inner catalog that keeps chunk rows and the run rows put to it in memory and answers
/// nothing else.
#[derive(Default)]
struct Chunks(Mutex<Vec<ChunkRow>>, Arc<std::sync::atomic::AtomicUsize>, Mutex<Vec<RunRow>>);

struct GuardCount(Arc<std::sync::atomic::AtomicUsize>);
impl contextful_core::coordinate::ExecutionGuard for GuardCount {}
impl Drop for GuardCount { fn drop(&mut self) { self.0.fetch_sub(1,std::sync::atomic::Ordering::SeqCst); } }

impl Catalog for Chunks {
    fn supports_strict_execution_guards(&self) -> bool { true }
    fn execution_guard(&self, _: &contextful_core::coordinate::ExecutionAdmission) -> Result<Box<dyn contextful_core::coordinate::ExecutionGuard>,Failure> {
        self.1.fetch_add(1,std::sync::atomic::Ordering::SeqCst);
        Ok(Box::new(GuardCount(self.1.clone())))
    }
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
    fn delete_chunks(&self, pipeline_id: &str, table: &str) -> Result<Vec<ChunkRow>, Failure> {
        let mut rows = self.0.lock().unwrap();
        let (removed, kept): (Vec<ChunkRow>, Vec<ChunkRow>) = rows.drain(..).partition(|r| r.pipeline_id == pipeline_id && r.table == table);
        *rows = kept;
        Ok(removed)
    }
    fn retired_at(&self, scope: &OwnerScope) -> Result<Option<String>, Failure> {
        Ok(Some(format!("retired under {scope}")))
    }
    fn now(&self) -> Result<Instant, Failure> {
        Ok(at("2030-01-01T00:05:00Z"))
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
    /// A retirement by the execution `fenced` moves no row; any other applies.
    fn retire_at(&self, _: &OwnerScope, execution_id: &str, _: Option<(CursorRow, u64)>, _: Option<&Lease>) -> Result<Cas, Failure> {
        Ok(if execution_id == "fenced" { Cas::VersionMoved } else { Cas::Applied })
    }
    fn put_run(&self, row: &RunRow) -> Result<(), Failure> {
        self.2.lock().unwrap().push(row.clone());
        Ok(())
    }
    fn run(&self, run_id: &str) -> Result<Option<RunRow>, Failure> {
        Ok(Some(row(run_id, RunStatus::Running, Phase::Plan, "2030-01-01T00:00:00Z")))
    }
    fn runs(&self, _: Option<&str>) -> Result<Vec<RunRow>, Failure> {
        unimplemented!()
    }
    fn history(&self, pipeline_id: Option<&str>, window: &contextful_core::run::record::Window) -> Result<contextful_core::run::record::HistoryPage, Failure> {
        let run = row(&format!("windowed-{}", pipeline_id.unwrap_or("all")), RunStatus::Success, Phase::Commit, "2030-01-01T00:00:00Z");
        Ok(contextful_core::run::record::HistoryPage { window: window.clone(), runs: vec![run], truncated: true })
    }
    fn update_run(&self, run_id: &str, f: &mut dyn FnMut(&mut RunRow) -> Result<(), RunError>) -> Result<Option<Result<RunRow, RunError>>, Failure> {
        let mut rows = self.2.lock().unwrap();
        let Some(r) = rows.iter_mut().rev().find(|r| r.run_id == run_id) else { return Ok(None) };
        Ok(Some(f(r).map(|()| r.clone())))
    }
}

/// The recording catalog hands every chunk-plan read and write to the catalog it wraps.
#[test]
fn the_recorded_catalog_keeps_the_wrapped_catalogs_chunk_plan() {
    let f = Fixture::new();
    let inner = Arc::new(Chunks::default());
    let recorded = RecordedCatalog::new(inner.clone(), f.store.clone(), Box::new(|_| Ok(NodeId::parse("ingest-a").unwrap())));
    assert!(recorded.supports_strict_execution_guards());
    let token=recorded.execution_guard(&contextful_core::coordinate::ExecutionAdmission::new(Vec::new(),Vec::new(),false).unwrap()).unwrap();
    assert_eq!(inner.1.load(std::sync::atomic::Ordering::SeqCst),1);
    drop(token);
    assert_eq!(inner.1.load(std::sync::atomic::Ordering::SeqCst),0);
    let window = Window::new(Position::Int(0), Position::Int(10)).unwrap();
    recorded.put_chunk(&ChunkRow::planned("feed", "filings", "c0", 0, window)).unwrap();
    assert_eq!(inner.0.lock().unwrap().len(), 1);
    let scope = OwnerScope::chunk("feed", "filings", "c0");
    assert_eq!(recorded.chunks("feed", "filings").unwrap().len(), 1);
    assert_eq!(recorded.chunk_at(&scope).unwrap().map(|r| r.chunk), Some("c0".to_string()));
    let claimed = recorded.update_chunk(&scope, None, &mut |r| r.status = ChunkStatus::Running).unwrap();
    assert_eq!(claimed.map(|r| r.status), Some(ChunkStatus::Running));
    assert_eq!(inner.0.lock().unwrap()[0].status, ChunkStatus::Running);
    let removed = recorded.delete_chunks("feed", "filings").unwrap();
    assert_eq!(removed.iter().map(|r| r.chunk.as_str()).collect::<Vec<_>>(), ["c0"], "a plan delete reaches the wrapped catalog");
    assert!(inner.0.lock().unwrap().is_empty());
}

/// The recording catalog reads a scope's last retirement from the catalog it wraps.
#[test]
fn the_recorded_catalog_reads_the_wrapped_catalogs_last_retirement() {
    let f = Fixture::new();
    let recorded = RecordedCatalog::new(Arc::new(Chunks::default()), f.store.clone(), Box::new(|_| Ok(NodeId::parse("ingest-a").unwrap())));
    let scope = OwnerScope::table("feed", "filings");
    assert_eq!(recorded.retired_at(&scope).unwrap(), Some(format!("retired under {scope}")));
}

/// The recording catalog reads a history window through the catalog it wraps, so the window reaches that
/// catalog's `WHERE` rather than a filter over every run.
#[test]
fn the_recorded_catalog_reads_history_through_the_wrapped_catalogs_window() {
    let f = Fixture::new();
    let recorded = RecordedCatalog::new(Arc::new(Chunks::default()), f.store.clone(), Box::new(|_| Ok(NodeId::parse("ingest-a").unwrap())));
    let window = contextful_core::run::record::Window { since: Some(at("2030-01-01T00:00:00Z")), ceiling: 1 };
    let page = recorded.history(Some("feed"), &window).unwrap();
    assert_eq!((page.runs[0].run_id.as_str(), page.truncated, page.window), ("windowed-feed", true, window));
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

/// A terminal item receipt the wrapped catalog applies appends one item record row; a refused
/// retirement, a receipt naming no item and a scope outside `item:` append none.
#[cfg(feature = "read")]
#[test]
fn an_applied_item_receipt_appends_one_item_record_row() {
    use contextful_core::run::drive::{ItemIdentity, ItemReceipt, ItemTerminal};
    let f = Fixture::new();
    let recorded = RecordedCatalog::new(Arc::new(Chunks::default()), f.store.clone(), Box::new(|_| Ok(NodeId::parse("ingest-a").unwrap())));
    let receipt = |outcome: ItemTerminal, failure: Option<Failure>, named: bool| {
        let item = ItemIdentity { job: "score".into(), logical_key: json!("doc-1"), entity_namespace: "documents".into(), entity_key: json!(42), attempts: 3, outcome };
        let receipt = ItemReceipt { run_id: "fire-1:item:x1".into(), rows: 0, bytes: 0, failure, item: named.then_some(item) };
        (CursorRow { position: Some(serde_json::to_value(receipt).unwrap()), ..CursorRow::default() }, 0)
    };
    let exhausted = Failure::new(contextful_core::run::FailureTag::RateLimited, "token=sk-live-abcdef0123456789 throttled");
    let item = OwnerScope::host("item:x1");
    assert_eq!(recorded.retire_at(&item, "fenced", Some(receipt(ItemTerminal::Succeeded, None, true)), None).unwrap(), Cas::VersionMoved);
    recorded.retire_at(&item, "x1", Some(receipt(ItemTerminal::Succeeded, None, false)), None).unwrap();
    recorded.retire_at(&OwnerScope::host("job:x1"), "x1", Some(receipt(ItemTerminal::Succeeded, None, true)), None).unwrap();
    assert!(f.store.try_schema(ITEM_RECORD_TABLE).unwrap().is_none(), "no row lands before an applied, named item receipt");

    assert_eq!(recorded.retire_at(&item, "x1", Some(receipt(ItemTerminal::Exhausted, Some(exhausted), true)), None).unwrap(), Cas::Applied);
    let rows = f.query(
        &contextful_context::item_record::decl(),
        Bounds::default(),
        "SELECT job, logical_key, entity_namespace, entity_key, attempts, outcome, run_id, execution_id, _site_id FROM t",
    );
    assert_eq!(rows, [["score", "doc-1", "documents", "42", "3", "exhausted", "fire-1:item:x1", "x1", "site-a"].map(|c| Some(c.to_string()))]);
    let reason = f.query(&contextful_context::item_record::decl(), Bounds::default(), "SELECT reason FROM t");
    let reason = reason[0][0].clone().unwrap();
    assert!(reason.starts_with("RateLimited: ") && reason.contains("throttled") && !reason.contains("sk-live-abcdef0123456789"), "{reason}");

    // Only the engine appends to the item record.
    let err = f.land(&decl(&format!("name = \"{ITEM_RECORD_TABLE}\"")), "forged", json!([{"job": "score"}]), "2030-01-01T00:01:00Z").unwrap_err();
    assert!(matches!(err.store(), Some(StoreError::StoreReservedTableName(_))), "{err}");
}

/// A marking catalog writes a run row opening in flight with `unpushed` naming its node, before
/// any landing can commit, and leaves a terminal row and the store's run record unmarked.
#[test]
fn a_marking_catalog_marks_a_run_unpushed_at_its_open() {
    let f = Fixture::new();
    let inner = Arc::new(Chunks::default());
    let recorded = RecordedCatalog::new(inner.clone(), f.store.clone(), Box::new(|_| Ok(NodeId::parse("ingest-a").unwrap()))).marking_unpushed();
    recorded.put_run(&row("r1", RunStatus::Running, Phase::Plan, "2030-01-01T00:00:00Z")).unwrap();
    recorded.put_run(&row("r0", RunStatus::Success, Phase::Commit, "2030-01-01T00:00:00Z")).unwrap();
    let put = inner.2.lock().unwrap().clone();
    assert_eq!(put[0].unpushed.as_ref().map(|u| (u.node.as_str(), u.lost)), Some(("ingest-a", false)));
    assert!(put[1].unpushed.is_none(), "a terminal row carries no new mark");
    let recorded_rows = history(&f.store, Bounds::default()).unwrap();
    assert_eq!(recorded_rows.len(), 2, "the run record holds both rows");
    assert!(recorded_rows.iter().all(|r| r.unpushed.is_none()), "the run record stays unmarked");

    let plain = RecordedCatalog::new(inner.clone(), f.store.clone(), Box::new(|_| Ok(NodeId::parse("ingest-a").unwrap())));
    plain.put_run(&row("r2", RunStatus::Running, Phase::Plan, "2030-01-01T00:00:00Z")).unwrap();
    assert!(inner.2.lock().unwrap()[2].unpushed.is_none(), "an unmarking catalog marks nothing");
}

/// A marked run's move to its commit phase appends a run-record row without the mark the
/// execution database holds, so no push carries a mark into the bucket.
// spec: store.drain.record-unmarked@bd4810c6
#[test]
fn a_marked_runs_phase_move_records_its_row_unmarked() {
    let f = Fixture::new();
    let inner = Arc::new(Chunks::default());
    let recorded = RecordedCatalog::new(inner.clone(), f.store.clone(), Box::new(|_| Ok(NodeId::parse("ingest-a").unwrap()))).marking_unpushed();
    recorded.put_run(&row("r1", RunStatus::Running, Phase::Plan, "2030-01-01T00:00:00Z")).unwrap();
    let moved = recorded.update_run("r1", &mut |r| { r.phase = Phase::Commit; Ok(()) }).unwrap().unwrap().unwrap();
    assert!(moved.unpushed.is_some(), "the execution database keeps the mark");
    let recorded_rows = history(&f.store, Bounds::default()).unwrap();
    assert!(recorded_rows.iter().any(|r| r.phase == Phase::Commit), "the commit phase is recorded");
    assert!(recorded_rows.iter().all(|r| r.unpushed.is_none()), "the run record stays unmarked: {recorded_rows:?}");
}
