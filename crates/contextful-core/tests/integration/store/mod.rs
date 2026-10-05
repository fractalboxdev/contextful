//! The `store` contract's pure domain, one module per operation.

mod bound_time;
mod declare;
mod fold;
mod index;
mod lay_out;
mod reconcile;
mod reserve;
mod shapes;
mod sync;

use contextful_core::store::lay_out::{PartEntry, RunManifest, SnapshotId, SnapshotManifest};
use contextful_core::time::Instant;

pub fn at(s: &str) -> Instant {
    Instant::parse(s).unwrap_or_else(|e| panic!("{s}: {e}"))
}

pub fn run(id: &str, committed: &str, parts: usize) -> RunManifest {
    RunManifest {
        run_id: id.into(),
        table: "filings".into(),
        node_id: "ingest-a".into(),
        parts: (0..parts).map(|i| PartEntry { name: format!("part-{i:05}.parquet"), key_version: 0 }).collect(),
        committed_at: at(committed),
        replace_frontier: false,
        pipeline_id: None,
        cursor: None,
        fence: None,
        logged: false,
        commit_seq: None,
    }
}

pub fn snapshot(created: &str, parent: Option<&SnapshotId>, includes: &[&str]) -> SnapshotManifest {
    SnapshotManifest {
        snapshot_id: SnapshotId::next(at(created), parent),
        parent: parent.cloned(),
        ancestors: None,
        table: "filings".into(),
        created_at: at(created),
        includes_runs: includes.iter().map(|s| format!("{s}/ingest-a")).collect(),
        primary_key: vec![],
        order_by: None,
        row_count: 0,
        valid_time: None,
        parts: vec![PartEntry { name: "part-00000.parquet".into(), key_version: 0 }],
        indexes: vec![],
        fence: None,
        commit_seq: None,
        publish: None,
    }
}
