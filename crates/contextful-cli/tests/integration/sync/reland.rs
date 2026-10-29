//! `contextful sync reland`: the runs a local snapshot off the bucket's chain alone holds, landed
//! again on the bucket's head, so the pull its refusal stopped proceeds.

use super::{cf, file_sync, ok, project, refused, STORE};
use std::path::Path;

fn land(dir: &Path, run: &str, id: u32, now: &str) {
    std::fs::write(dir.join("batch.jsonl"), format!("{{\"id\":{id}}}\n")).unwrap();
    ok(&cf(dir, &["context", "land", "filings", "--project", "research", "--rows", "batch.jsonl", "--run-id", run, "--site-id", "site", "--now", now], &[]));
}

fn compact(dir: &Path, now: &str) {
    ok(&cf(dir, &["context", "compact", "filings", "--project", "research", "--now", now], &[]));
}

fn sync(dir: &Path, verb: &str) -> std::process::Output {
    cf(dir, &["sync", verb, "--project", "research"], &[])
}

fn files(dir: &Path) -> Vec<String> {
    ok(&cf(dir, &["context", "files", "filings", "--project", "research"], &[])).lines().map(str::to_string).collect()
}

/// A pull refusing a local fold whose runs only its parts hold names `sync reland`; the command lands those runs
/// again, prints them as JSON, and the pull then proceeds on both nodes. Run again, it refuses: nothing is stranded.
// spec: store.pull.reland@abcaebd9
// spec: store.pull.reland-unneeded@7966ae26
// spec: store.pull.reland-named@61ab1134
#[test]
fn sync_reland_lands_a_stranded_fold_and_the_pull_proceeds() {
    let bucket = tempfile::tempdir().unwrap();
    let a = project("ingest-a", &file_sync(bucket.path(), ""));
    let c = project("ingest-c", &file_sync(bucket.path(), ""));
    land(a.path(), "run-1", 1, "2030-01-01T00:00:00Z");
    compact(a.path(), "2030-01-01T00:10:00Z");
    ok(&sync(a.path(), "push"));
    ok(&sync(c.path(), "pull"));
    // A folds run-2 into the bucket's head; its run directory goes, so only the head's parts hold it.
    land(a.path(), "run-2", 2, "2030-01-01T01:00:00Z");
    compact(a.path(), "2030-01-01T01:10:00Z");
    ok(&sync(a.path(), "push"));
    std::fs::remove_dir_all(a.path().join(STORE).join("tables/filings/data/runs/run-2")).unwrap();
    ok(&sync(a.path(), "push"));
    // C folds run-3 off the bucket's chain, and its run directory goes too.
    land(c.path(), "run-3", 3, "2030-01-02T00:00:00Z");
    compact(c.path(), "2030-01-02T00:10:00Z");
    std::fs::remove_dir_all(c.path().join(STORE).join("tables/filings/data/runs/run-3")).unwrap();
    let out = sync(c.path(), "pull");
    refused(&out, "SyncLocalFoldUnrecoverable");
    assert!(String::from_utf8_lossy(&out.stderr).contains("`contextful sync reland filings`"), "{}", String::from_utf8_lossy(&out.stderr));

    let report: serde_json::Value = serde_json::from_str(&ok(&cf(c.path(), &["sync", "reland", "filings", "--project", "research"], &[]))).unwrap();
    let runs = report["runs"].as_array().unwrap();
    assert_eq!(runs.len(), 1, "{report}");
    assert_eq!((runs[0]["from"].as_str(), runs[0]["rows"].as_u64()), (Some("run-3/ingest-c"), Some(1)), "{report}");
    let run = runs[0]["run_id"].as_str().unwrap().to_string();
    assert!(run.starts_with("reland-"), "{report}");
    ok(&sync(c.path(), "pull"));
    let part = format!("tables/filings/data/runs/{run}/ingest-c/part-00000.parquet");
    let listed = files(c.path());
    assert!(listed.contains(&part) && listed.iter().all(|f| !f.contains("/runs/run-")), "{listed:?}");
    ok(&sync(c.path(), "push"));
    ok(&sync(a.path(), "pull"));
    assert!(files(a.path()).contains(&part));

    refused(&cf(c.path(), &["sync", "reland", "filings", "--project", "research"], &[]), "SyncRelandUnneeded");
}
