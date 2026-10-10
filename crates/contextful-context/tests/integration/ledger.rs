//! Request-ledger durability without the read engine.

use crate::support::{at, decl, Fixture};
use contextful_context::fold::fold;
use contextful_core::store::lay_out::NodeId;
use contextful_core::store::ledger::RequestRecord;
use serde_json::json;

fn record(id: &str) -> RequestRecord {
    RequestRecord {
        request_id: id.into(),
        vendor_request_id: None,
        connector: "http".into(),
        method: "GET".into(),
        url_host: "api.example.org".into(),
        status_code: Some(200),
        started_at: at("2030-01-01T00:00:00Z"),
        duration_ms: 3,
        batch_seq: Some(0),
    }
}

/// The ledger file names under `requests/`, sorted.
fn names(f: &Fixture, table: &str) -> Vec<String> {
    contextful_context::ledger::files(&f.store, table).unwrap().iter().map(|p| p.file_name().unwrap().to_string_lossy().into_owned()).collect()
}

/// Every `(run_id, request_id)` the table's ledger files hold, sorted.
fn calls(f: &Fixture, table: &str) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = contextful_context::ledger::files(&f.store, table)
        .unwrap()
        .iter()
        .flat_map(|p| contextful_context::ledger::read_for_store(&f.store, p).unwrap())
        .map(|(run, r)| (run, r.request_id))
        .collect();
    out.sort();
    out
}

fn pair(run: &str, id: &str) -> (String, String) {
    (run.to_string(), id.to_string())
}

#[test]
fn a_ledger_append_syncs_its_directory() {
    let r = Fixture::new();
    let node = NodeId::parse("ingest-a").unwrap();
    let mut record = record("durable");
    record.vendor_request_id = Some("vendor-durable".into());
    contextful_context::ledger::append(&r.store, "research/vendor", "run-0001", &node, &[record]).unwrap();
    let dir = r.store.root().join("tables/research/vendor/requests");
    contextful_fs::open_dir_for_sync(&dir).unwrap().sync_all().unwrap();
    let path = dir.join("run-0001.ingest-a.parquet");
    assert_eq!(contextful_context::ledger::read(&path).unwrap().len(), 1);
}

/// A pass merges the ledger files of committed runs into `requests/folded-<snapshot-id>.parquet`, carries the
/// previous merge forward, and leaves an in-flight run's file in place.
#[test]
fn a_pass_merges_committed_ledgers_into_one_file_named_by_its_snapshot() {
    let f = Fixture::new();
    let d = decl("name = \"filings\"");
    let node = NodeId::parse("ingest-a").unwrap();
    f.land(&d, "run-1", json!([{"e": 1}]), "2030-01-01T00:00:00Z").unwrap();
    contextful_context::ledger::append(&f.store, "filings", "run-1", &node, &[record("c1"), record("c2")]).unwrap();
    // A run still in flight: its ledger has no committed manifest beside it.
    contextful_context::ledger::append(&f.store, "filings", "run-9", &node, &[record("c9")]).unwrap();
    fold(&f.store, &d, at("2030-01-01T01:00:00Z")).unwrap();
    let (first, _) = f.store.pointer("filings").unwrap().unwrap();
    assert_eq!(names(&f, "filings"), [format!("folded-{}.parquet", first.snapshot_id), "run-9.ingest-a.parquet".into()]);
    assert_eq!(calls(&f, "filings"), [pair("run-1", "c1"), pair("run-1", "c2"), pair("run-9", "c9")]);

    // The next pass folds the next committed run's ledger together with the earlier merge.
    f.land(&d, "run-2", json!([{"e": 2}]), "2030-01-01T02:00:00Z").unwrap();
    contextful_context::ledger::append(&f.store, "filings", "run-2", &node, &[record("c3")]).unwrap();
    fold(&f.store, &d, at("2030-01-01T03:00:00Z")).unwrap();
    let (second, _) = f.store.pointer("filings").unwrap().unwrap();
    assert_ne!(first.snapshot_id, second.snapshot_id);
    assert_eq!(names(&f, "filings"), [format!("folded-{}.parquet", second.snapshot_id), "run-9.ingest-a.parquet".into()]);
    assert_eq!(calls(&f, "filings"), [pair("run-1", "c1"), pair("run-1", "c2"), pair("run-2", "c3"), pair("run-9", "c9")]);
}

/// A ledger row is collected 365 d after its run committed.
// spec: store.reserve.ledger-retention@7271f9e0
#[test]
fn a_ledger_row_is_collected_365_days_after_its_run_committed() {
    assert_eq!(contextful_core::store::reserve::LEDGER_RETENTION_SECS, 365 * 24 * 60 * 60);
    let f = Fixture::new();
    let d = decl("name = \"filings\"");
    let node = NodeId::parse("ingest-a").unwrap();
    f.land(&d, "run-1", json!([{"e": 1}]), "2030-01-01T00:00:00Z").unwrap();
    contextful_context::ledger::append(&f.store, "filings", "run-1", &node, &[record("old")]).unwrap();
    fold(&f.store, &d, at("2030-01-01T01:00:00Z")).unwrap();
    f.land(&d, "run-2", json!([{"e": 2}]), "2030-07-01T00:00:00Z").unwrap();
    contextful_context::ledger::append(&f.store, "filings", "run-2", &node, &[record("new")]).unwrap();
    // One second short of 365 d after run-1 committed: both rows stay.
    fold(&f.store, &d, at("2030-12-31T23:59:59Z")).unwrap();
    assert_eq!(calls(&f, "filings"), [pair("run-1", "old"), pair("run-2", "new")]);
    // At 365 d, a pass with nothing to fold still collects run-1's row; run-2's stays.
    fold(&f.store, &d, at("2031-01-01T00:00:00Z")).unwrap();
    assert_eq!(calls(&f, "filings"), [pair("run-2", "new")]);
    assert_eq!(names(&f, "filings").len(), 1);
}
