//! `contextful sync push` under a table's cleared redistribution flag.

use super::{cf, file_sync, ok, project, refused, seed, LANDED};
use std::path::Path;

const WITHHELD: &str = "authoring_posture = \"per_request\"\n[[pipeline.tables]]\nname = \"filings\"\n[pipeline.tables.policy.redistribution]\nreplicate = false\n";

fn bucket_keys(bucket: &Path) -> Vec<String> {
    fn walk(root: &Path, dir: &Path, out: &mut Vec<String>) {
        for entry in std::fs::read_dir(dir).into_iter().flatten().flatten() {
            let path = entry.path();
            if path.is_dir() {
                walk(root, &path, out);
            } else {
                out.push(path.strip_prefix(root).unwrap().to_string_lossy().replace('\\', "/"));
            }
        }
    }
    let mut out = Vec::new();
    walk(bucket, bucket, &mut out);
    out.sort();
    out
}

const TWO_TABLES: &str = "authoring_posture = \"per_request\"\n[[pipeline.tables]]\nname = \"filings\"\n[pipeline.tables.policy.redistribution]\nreplicate = false\n[[pipeline.tables]]\nname = \"public\"\n";

/// A project landing `filings` under a cleared flag beside an open `public`.
fn two_tables(bucket: &Path) -> tempfile::TempDir {
    let a = project("ingest-a", &file_sync(bucket, ""));
    std::fs::write(a.path().join("contextful.toml"), TWO_TABLES).unwrap();
    std::fs::write(a.path().join("batch.jsonl"), "{\"id\":1}\n").unwrap();
    for table in ["filings", "public"] {
        ok(&cf(
            a.path(),
            &["context", "land", table, "--project", "research", "--rows", "batch.jsonl", "--run-id", "run-1", "--site-id", "site", "--now", "2030-01-01T00:00:00Z"],
            &[],
        ));
    }
    a
}

/// A declaration clearing `policy.redistribution.replicate` keeps the table's landed part off
/// the bucket while the push ships the open table and the node's run state.
#[test]
fn a_withheld_table_lands_nothing_in_the_bucket() {
    let bucket = tempfile::tempdir().unwrap();
    let a = two_tables(bucket.path());
    ok(&cf(a.path(), &["sync", "push", "--project", "research"], &[]));
    let keys = bucket_keys(bucket.path());
    assert!(keys.iter().any(|k| k.contains("/research/tables/public/")), "{keys:?}");
    assert!(keys.iter().any(|k| k.contains("/research/nodes/ingest-a/")), "{keys:?}");
    assert!(!keys.iter().any(|k| k.contains("tables/filings")), "{keys:?}");
    let manifest = std::fs::read_to_string(bucket.path().join("context-team/team/manifest.json")).unwrap();
    assert!(!manifest.contains("filings"), "{manifest}");
}

/// `sync manifest --emit` prints the plan the push commits: no key of a withheld table.
#[test]
fn the_emitted_plan_names_no_withheld_key() {
    let bucket = tempfile::tempdir().unwrap();
    let a = two_tables(bucket.path());
    let plan = ok(&cf(a.path(), &["sync", "manifest", "--emit", "--project", "research"], &[]));
    assert!(plan.contains("research/tables/public/"), "{plan}");
    assert!(!plan.contains("filings"), "{plan}");
}

/// A replica's pull reads a flag cleared under `policy.redistribution` as it reads
/// `replicate = false`: a refresh naming the table refuses.
#[test]
fn a_policy_cleared_flag_keeps_the_table_off_a_replica_pull() {
    let bucket = tempfile::tempdir().unwrap();
    let _a = seed(&file_sync(bucket.path(), ""), &[]);
    let replica = project("replica-p", &file_sync(bucket.path(), "\n[replica]\nof = \"team/research\"\n"));
    std::fs::write(replica.path().join("contextful.toml"), WITHHELD).unwrap();
    refused(&cf(replica.path(), &["sync", "pull", "--project", "research", "--table", "filings"], &[]), "ReplicaSensitiveTable");
}

/// Clearing the flag over a table an earlier push uploaded refuses the next push with
/// `EnforceStaleRedistributedObjects`, naming the part, and leaves every object in place.
#[test]
fn clearing_the_flag_after_a_push_refuses_and_deletes_nothing() {
    let bucket = tempfile::tempdir().unwrap();
    let a = seed(&file_sync(bucket.path(), ""), &[]);
    let before = bucket_keys(bucket.path());
    assert!(before.iter().any(|k| k.ends_with(LANDED)), "{before:?}");
    std::fs::write(a.path().join("contextful.toml"), WITHHELD).unwrap();
    let out = cf(a.path(), &["sync", "push", "--project", "research"], &[]);
    refused(&out, "EnforceStaleRedistributedObjects");
    assert!(String::from_utf8_lossy(&out.stderr).contains(&format!("research/{LANDED}")));
    assert_eq!(bucket_keys(bucket.path()), before);
}
