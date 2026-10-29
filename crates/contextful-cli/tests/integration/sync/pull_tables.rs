//! `[sync] pull_tables`: the tables a pull without `--table` takes, and the pull a run takes
//! before it reads.

use super::{cf, file_sync, files, ok, project, refused, start, LANDED};
use std::path::Path;

const MEMO: &str = "tables/memos/data/runs/run-1/ingest-a/part-00000.parquet";

/// A project declaring `filings` and `memos`, its store declaring `sync`.
fn two_tables(id: &str, sync: &str) -> tempfile::TempDir {
    let dir = project(id, sync);
    std::fs::write(dir.path().join("contextful.toml"), "authoring_posture = \"per_request\"\n[[pipeline.tables]]\nname = \"filings\"\n[[pipeline.tables]]\nname = \"memos\"\n").unwrap();
    dir
}

fn memos(dir: &Path) -> std::process::Output {
    cf(dir, &["context", "files", "memos", "--project", "research"], &[])
}

/// Node `ingest-a` lands one run into each table and pushes both.
fn seed(bucket: &Path) {
    let a = two_tables("ingest-a", &file_sync(bucket, ""));
    std::fs::write(a.path().join("batch.jsonl"), "{\"id\":1}\n").unwrap();
    for table in ["filings", "memos"] {
        ok(&cf(
            a.path(),
            &["context", "land", table, "--project", "research", "--rows", "batch.jsonl", "--run-id", "run-1", "--site-id", "site", "--now", "2030-01-01T00:00:00Z"],
            &[],
        ));
    }
    ok(&cf(a.path(), &["sync", "push", "--project", "research"], &[]));
}

/// `[sync] pull_tables` scopes the pull a run takes before it reads and a `sync pull` without
/// `--table`; `--table` overrides it, and a misplaced star refuses.
// spec: store.pull.table-scope@5cf8c370
#[test]
fn pull_tables_scopes_the_pull_before_a_run_and_a_pull_without_table() {
    let bucket = tempfile::tempdir().unwrap();
    seed(bucket.path());

    // A run on a disposable disk pulls only the declared tables before it reads.
    let cold = two_tables("ingest-c", &file_sync(bucket.path(), "pull_before_run = true\npull_tables = [\"fil*\"]\n"));
    ok(&start(cold.path(), "c1"));
    assert_eq!(files(cold.path()), [LANDED]);
    refused(&memos(cold.path()), "StoreUnknownTable");

    // `sync pull` without `--table` takes the same tables, and `--table` widens them.
    let pulled = two_tables("ingest-d", &file_sync(bucket.path(), "pull_tables = [\"filings\"]\n"));
    ok(&cf(pulled.path(), &["sync", "pull", "--project", "research"], &[]));
    assert_eq!(files(pulled.path()), [LANDED]);
    refused(&memos(pulled.path()), "StoreUnknownTable");
    ok(&cf(pulled.path(), &["sync", "pull", "--project", "research", "--table", "*"], &[]));
    assert_eq!(ok(&memos(pulled.path())), MEMO);

    // A pattern with a misplaced star refuses before the run reads.
    let malformed = two_tables("ingest-e", &file_sync(bucket.path(), "pull_before_run = true\npull_tables = [\"fil*ings\"]\n"));
    refused(&start(malformed.path(), "e1"), "GrantPatternMalformed");
}
