//! `contextful sync generations`: the generations a bucket holds, to choose one to restore.

use super::{cf, file_sync, ok, project, STORE};

/// Each generation lists its commit time, its pushing node and the project's entry and tombstone
/// counts, so an operator finds the last generation before a push tombstoned the run history.
// spec: store.pull.generation-list@42baae85
// spec: store.push.commit-stamp@7b3fd2fe
#[test]
fn generations_list_commit_time_pushing_node_and_tombstones() {
    let bucket = tempfile::tempdir().unwrap();
    let a = project("ingest-a", &file_sync(bucket.path(), ""));
    std::fs::write(a.path().join("batch.jsonl"), "{\"id\":1}\n").unwrap();
    ok(&cf(a.path(), &["context", "land", "filings", "--project", "research", "--rows", "batch.jsonl", "--run-id", "run-1", "--site-id", "site", "--now", "2030-01-01T00:00:00Z"], &[]));
    ok(&cf(a.path(), &["sync", "push", "--project", "research", "--now", "2030-01-01T01:00:00Z"], &[]));
    std::fs::remove_dir_all(a.path().join(STORE).join("tables/filings/data/runs/run-1")).unwrap();
    ok(&cf(a.path(), &["sync", "push", "--project", "research", "--now", "2030-01-02T01:00:00Z"], &[]));

    let listed: serde_json::Value = serde_json::from_str(&ok(&cf(a.path(), &["sync", "generations", "--project", "research", "--json"], &[]))).unwrap();
    let rows = listed["generations"].as_array().unwrap();
    assert_eq!(rows.len(), 2, "{listed}");
    assert_eq!(rows[0]["generation"], 1);
    assert_eq!(rows[0]["committed_at"], "2030-01-01T01:00:00Z");
    assert_eq!(rows[0]["committed_by"], "ingest-a");
    assert_eq!(rows[0]["tombstones"], 0);
    assert_eq!(rows[1]["generation"], 2);
    assert_eq!(rows[1]["committed_at"], "2030-01-02T01:00:00Z");
    assert!(rows[1]["tombstones"].as_u64().unwrap() >= 2, "{listed}");
    assert!(rows[1]["entries"].as_u64().unwrap() < rows[0]["entries"].as_u64().unwrap(), "{listed}");

    let text = ok(&cf(a.path(), &["sync", "generations", "--project", "research"], &[]));
    let lines: Vec<&str> = text.lines().collect();
    assert!(lines.len() == 3 && lines[1].starts_with("1 ") && lines[1].contains("2030-01-01T01:00:00Z") && lines[1].contains("ingest-a"), "{text}");
}
