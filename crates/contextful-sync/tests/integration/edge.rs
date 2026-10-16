//! `authority.bound-redistribution.edge-filter`: a serving edge in front of the bucket hands a
//! pulling credential a replica holding only the tables its grants cover.

use crate::support::{at, bucket, node, Node};
use contextful_core::grant::{Action, AggregateGrant, Grant, TablePattern, TenantScope};
use contextful_core::store::object::{Condition, ObjectStore};
use contextful_core::store::sync::BucketManifest;
use contextful_sync::{PullScope, ServingEdge, SyncError};
use contextful_core::store::StoreError;
use serde_json::json;
use std::sync::Arc;

const NOW: &str = "2030-01-01T01:00:00Z";
const LANDED: &str = "2030-01-01T00:00:00Z";

fn read(patterns: &[&str]) -> Grant {
    Grant {
        actions: vec![Action::Read],
        tables: patterns.iter().map(|p| TablePattern::parse(p).unwrap()).collect(),
        tenant: None,
        aggregate: None,
        templates: None,
        max_rows: None,
        max_duration_ms: None,
        max_response_bytes: None,
        horizon: None,
    }
}

fn write(n: &Node, rel: &str, bytes: &[u8]) {
    let path = n.root().join(rel);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, bytes).unwrap();
}

fn files(n: &Node) -> Vec<String> {
    let root = n.root();
    let mut out = Vec::new();
    let mut stack = vec![root.clone()];
    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(&dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                stack.push(path);
            } else {
                out.push(path.strip_prefix(&root).unwrap().to_string_lossy().replace('\\', "/"));
            }
        }
    }
    out.sort();
    out
}

const RUN_STATE: &str = "team/research/nodes/ingest-a/run-state.json";
const CURSOR: &str = "team/research/cursors/filings-feed/ingest-a/00000000000000000001.json";

/// A bucket holding two tables, a root-level verbatim record, a run state and a cursor commit
/// log naming `filings`, pushed with no bound.
fn pushed() -> (tempfile::TempDir, Arc<dyn ObjectStore>) {
    let dir = tempfile::tempdir().unwrap();
    let b = bucket(dir.path());
    let a = node("ingest-a", b.clone(), "");
    a.land_into("filings", "run-1", json!([{"id": 1}]), LANDED);
    a.land_into("public", "run-1", json!([{"id": 2}]), LANDED);
    write(&a, "records/run-1.json", b"verbatim pull record");
    let state = json!({
        "format": 1,
        "node_id": "ingest-a",
        "runs": {"filings-feed": {"run_id": "run-1", "table": "filings", "status": "succeeded", "started_at": LANDED}},
        "cursors": [{"pipeline_id": "filings-feed", "table": "filings", "position": 41, "run_id": "run-1", "committed_at": LANDED}]
    });
    write(&a, "nodes/ingest-a/run-state.json", state.to_string().as_bytes());
    write(&a, "cursors/filings-feed/ingest-a/00000000000000000001.json", br#"{"table":"filings","position":41}"#);
    a.syncer.push(at(NOW)).unwrap();
    assert!(b.get(RUN_STATE).unwrap().is_some() && b.get(CURSOR).unwrap().is_some());
    assert!(!b.list("team/research/tables/filings/").unwrap().is_empty());
    assert!(!b.list("team/research/records/").unwrap().is_empty());
    (dir, b)
}

/// A pull through the edge under a grant on `public` lands `public` alone: the edge's manifest
/// names no other table, an ungranted object reads as absent by its key and from a listing,
/// the root objects Shapes withholds stay behind, and the edge takes no write.
// spec: authority.bound-redistribution.edge-filter@1932ed20
#[test]
fn a_pull_through_the_edge_lands_only_granted_tables() {
    let (_dir, b) = pushed();
    let edge: Arc<dyn ObjectStore> = Arc::new(ServingEdge::new(b.clone(), "team", "research", &[read(&["public"])]));

    let served: BucketManifest = serde_json::from_slice(&edge.get("team/manifest.json").unwrap().unwrap().0).unwrap();
    assert!(!served.entries.is_empty());
    assert!(served.entries.keys().any(|k| k.starts_with("research/tables/public/")), "{:?}", served.entries.keys());
    assert!(!served.entries.keys().any(|k| k.contains("filings") || k.contains("/records/") || k.contains("/nodes/")), "{:?}", served.entries.keys());
    assert_eq!(edge.get(RUN_STATE).unwrap(), None);
    assert_eq!(edge.get(CURSOR).unwrap(), None);

    let filings = b.list("team/research/tables/filings/").unwrap();
    assert_eq!(edge.get(&filings[0]).unwrap(), None);
    assert_eq!(edge.get("team/research/records/run-1.json").unwrap(), None);
    assert_eq!(edge.list("team/research/tables/filings/").unwrap(), Vec::<String>::new());
    let listed = edge.list("team/research/").unwrap();
    assert!(!listed.is_empty());
    assert!(listed.iter().all(|k| !k.contains("filings") && !k.contains("/records/") && !k.contains("/nodes/")), "{listed:?}");
    assert!(edge.put("team/research/tables/public/x", b"rows", Condition::IfNoneMatch).is_err());
    assert!(edge.delete(&filings[0]).is_err());
    assert!(b.get(&filings[0]).unwrap().is_some());

    let cold = node("reader", edge, "");
    cold.syncer.pull(&PullScope::default()).unwrap();
    let landed = files(&cold);
    assert!(landed.iter().any(|f| f.starts_with("tables/public/data/")), "{landed:?}");
    assert!(!landed.iter().any(|f| f.contains("filings") || f.starts_with("records/") || f.starts_with("nodes/")), "{landed:?}");
}

/// A bucket manifest of a newer format reaches the puller as that format alone, so the pull
/// refuses it as a pull from the bucket does, and the edge leaks none of its entries.
#[test]
fn a_newer_manifest_through_the_edge_refuses_as_an_unsupported_format() {
    let (_dir, b) = pushed();
    let mut manifest: serde_json::Value = serde_json::from_slice(&b.get("team/manifest.json").unwrap().unwrap().0).unwrap();
    manifest["format"] = json!(2);
    manifest["entries"] = json!(["a newer shape"]);
    b.put("team/manifest.json", manifest.to_string().as_bytes(), Condition::None).unwrap();
    let edge: Arc<dyn ObjectStore> = Arc::new(ServingEdge::new(b.clone(), "team", "research", &[read(&["public"])]));

    let served: serde_json::Value = serde_json::from_slice(&edge.get("team/manifest.json").unwrap().unwrap().0).unwrap();
    assert_eq!(served, json!({"format": 2}));
    let cold = node("reader", edge, "");
    let refused = cold.syncer.pull(&PullScope::default()).unwrap_err();
    assert!(matches!(refused, SyncError::Store(StoreError::SyncManifestFormatUnsupported(_))), "{refused:?}");
}

/// A grant on `*` serves every table and the root objects; a tenant-scoped, aggregate or
/// horizon-bound grant serves none of its table's files, and a write-only grant serves nothing.
// spec: authority.bound-redistribution.edge-whole-table@febff83c
#[test]
fn only_a_whole_table_read_grant_opens_a_table_at_the_edge() {
    let (_dir, b) = pushed();
    let filings = b.list("team/research/tables/filings/").unwrap();

    let everything = ServingEdge::new(b.clone(), "team", "research", &[read(&["*"])]);
    assert!(everything.get(&filings[0]).unwrap().is_some());
    assert!(everything.get("team/research/records/run-1.json").unwrap().is_some());

    let prefix = ServingEdge::new(b.clone(), "team", "research", &[read(&["fil*"])]);
    assert!(prefix.get(&filings[0]).unwrap().is_some());
    assert_eq!(prefix.get("team/research/records/run-1.json").unwrap(), None);

    let mut scoped = read(&["filings"]);
    scoped.tenant = Some(TenantScope { table: "filings".into(), value: "acme".into() });
    let scoped = ServingEdge::new(b.clone(), "team", "research", &[scoped]);
    assert_eq!(scoped.get(&filings[0]).unwrap(), None);

    let mut aggregate = read(&["filings"]);
    aggregate.aggregate = Some(AggregateGrant { min_group_size: 5, max_contributor_share: 0.5, functions: vec!["count".into()], max_groups: 100, max_rows: None });
    let aggregate = ServingEdge::new(b.clone(), "team", "research", &[aggregate]);
    assert_eq!(aggregate.get(&filings[0]).unwrap(), None);

    let mut horizon = read(&["filings"]);
    horizon.horizon = Some(at(LANDED));
    let horizon = ServingEdge::new(b.clone(), "team", "research", &[horizon]);
    assert_eq!(horizon.get(&filings[0]).unwrap(), None);

    let mut writer = read(&["*"]);
    writer.actions = vec![Action::Write];
    let writer = ServingEdge::new(b.clone(), "team", "research", &[writer]);
    assert_eq!(writer.get(&filings[0]).unwrap(), None);
    assert_eq!(writer.get(RUN_STATE).unwrap(), None);
    assert_eq!(writer.get(CURSOR).unwrap(), None);
    assert!(everything.get(RUN_STATE).unwrap().is_some());
    assert!(everything.get(CURSOR).unwrap().is_some());
    assert_eq!(prefix.get(RUN_STATE).unwrap(), None);
    let served: BucketManifest = serde_json::from_slice(&writer.get("team/manifest.json").unwrap().unwrap().0).unwrap();
    assert_eq!(served.entries.keys().filter(|k| k.contains("/tables/")).count(), 0, "{:?}", served.entries.keys());
}
