//! Request-ledger durability without the read engine.

use crate::support::{at, Fixture};
use contextful_core::store::lay_out::NodeId;
use contextful_core::store::ledger::RequestRecord;

#[test]
fn a_ledger_append_syncs_its_directory() {
    let r = Fixture::new();
    let node = NodeId::parse("ingest-a").unwrap();
    let record = RequestRecord {
        request_id: "durable".into(),
        vendor_request_id: Some("vendor-durable".into()),
        connector: "http".into(),
        method: "GET".into(),
        url_host: "api.example.org".into(),
        status_code: None,
        started_at: at("2030-01-10T00:00:00Z"),
        duration_ms: 12,
        batch_seq: None,
    };
    contextful_context::ledger::append(&r.store, "research/vendor", "run-0001", &node, &[record]).unwrap();
    let dir = r.store.root().join("tables/research/vendor/requests");
    contextful_fs::open_dir_for_sync(&dir).unwrap().sync_all().unwrap();
    let path = dir.join("run-0001.ingest-a.parquet");
    assert_eq!(contextful_context::ledger::read(&path).unwrap().len(), 1);
}
