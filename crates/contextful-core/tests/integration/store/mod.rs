//! The `store` contract's pure domain, one module per operation.

mod available_time;
mod bound_time;
mod declare;
mod endpoint;
mod fold;
mod index;
mod lay_out;
mod reconcile;
mod reserve;
mod shapes;
mod snapshot;
mod sync;

use contextful_core::store::lay_out::{PartEntry, RunManifest, SnapshotId, SnapshotManifest};
use contextful_core::time::Instant;

pub fn at(s: &str) -> Instant {
    Instant::parse(s).unwrap_or_else(|e| panic!("{s}: {e}"))
}

pub fn run(id: &str, committed: &str, parts: usize) -> RunManifest {
    RunManifest {
        format_version: Default::default(),
        run_id: id.into(),
        table: "filings".into(),
        node_id: "ingest-a".into(),
        parts: (0..parts).map(|i| PartEntry::new(format!("part-{i:05}.parquet"), 0)).collect(),
        committed_at: at(committed),
        replace_frontier: false,
        pipeline_id: None,
        cursor: None,
        fence: None,
        logged: false,
        commit_seq: None,
        group_root: None,
        schema_diffs: Vec::new(),
    }
}

pub fn snapshot(created: &str, parent: Option<&SnapshotId>, includes: &[&str]) -> SnapshotManifest {
    SnapshotManifest {
        format_version: Default::default(),
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
        parts: vec![PartEntry::new("part-00000.parquet", 0)],
        indexes: vec![],
        fence: None,
        commit_seq: None,
        publish: None,
    }
}
