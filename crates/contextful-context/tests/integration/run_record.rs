//! `run.record`: the durable run record in the engine's reserved `_runs` table.

use crate::support::{at, decl, Fixture};
use contextful_context::run_record::{append, history};
use contextful_core::run::record::{Phase, RunRow, RunStatus};
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

/// A producer landing into the run record still refuses; only the engine appends to it.
#[test]
fn a_producer_cannot_land_into_the_run_record() {
    let f = Fixture::new();
    append(&f.store, &NodeId::parse("ingest-a").unwrap(), &row("r1", RunStatus::Running, Phase::Plan, "2030-01-01T00:00:00Z"), at("2030-01-01T00:00:00Z")).unwrap();
    let err = f.land(&decl(&format!("name = \"{RUN_RECORD_TABLE}\"")), "forged", json!([{"run_id": "r9"}]), "2030-01-01T00:01:00Z").unwrap_err();
    assert!(matches!(err.store(), Some(StoreError::StoreReservedTableName(_))), "{err}");
    assert_eq!(history(&f.store, Bounds::default()).unwrap().len(), 1);
}
