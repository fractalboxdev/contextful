//! `contextful sync collect`: deleting the bucket objects no retained state reaches.

use super::{cf, file_sync, ok, project, STORE};
use serde_json::Value;
use std::path::Path;

/// Every file under `dir`, at any depth.
fn files(dir: &Path) -> usize {
    let Ok(entries) = std::fs::read_dir(dir) else { return 0 };
    entries.map(|e| e.unwrap().path()).map(|p| if p.is_dir() { files(&p) } else { 1 }).sum()
}

/// `sync collect --dry-run` prints each class's dropped keys, deleted objects and bytes as JSON and deletes nothing;
/// `sync collect` then deletes what the dry run counted.
// spec: store.collect.command@4c00f075
#[test]
fn a_dry_run_counts_what_collect_then_deletes() {
    let bucket = tempfile::tempdir().unwrap();
    let a = project("ingest-a", &file_sync(bucket.path(), "retain_generations = 1\n"));
    std::fs::write(a.path().join("batch.jsonl"), "{\"id\":1}\n").unwrap();
    ok(&cf(a.path(), &["context", "land", "filings", "--project", "research", "--rows", "batch.jsonl", "--run-id", "run-1", "--site-id", "site", "--now", "2030-01-01T00:00:00Z"], &[]));
    ok(&cf(a.path(), &["sync", "push", "--project", "research", "--now", "2030-01-01T01:00:00Z"], &[]));
    std::fs::remove_dir_all(a.path().join(STORE).join("tables/filings/data/runs/run-1")).unwrap();
    ok(&cf(a.path(), &["sync", "push", "--project", "research", "--now", "2030-01-02T01:00:00Z"], &[]));
    let runs = bucket.path().join("context-team/team/research/tables/filings/data/runs/run-1");
    let held = files(&runs);
    assert!(held > 0, "the bucket holds the tombstoned run");

    let collect = |extra: &[&str]| -> Value {
        let args = [&["sync", "collect", "--project", "research", "--now", "2030-01-03T00:00:00Z"][..], extra].concat();
        serde_json::from_str(&ok(&cf(a.path(), &args, &[]))).unwrap()
    };
    let dry = collect(&["--dry-run"]);
    assert_eq!(dry["dry_run"], true, "{dry}");
    assert_eq!(dry["unlisted"]["deleted"].as_u64(), Some(held as u64), "{dry}");
    assert!(dry["unlisted"]["bytes"].as_u64().unwrap() > 0, "{dry}");
    assert!(dry["generations"]["deleted"].as_u64().unwrap() >= 1, "{dry}");
    assert_eq!(files(&runs), held, "the dry run deletes nothing");

    let real = collect(&[]);
    assert_eq!(real["dry_run"], false, "{real}");
    assert_eq!(real["unlisted"]["deleted"], dry["unlisted"]["deleted"], "{real}");
    assert_eq!(files(&runs), 0, "the collect deletes the tombstoned run");
}
