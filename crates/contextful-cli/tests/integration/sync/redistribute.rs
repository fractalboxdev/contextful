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

/// A declaration clearing `policy.redistribution.replicate` keeps the table's landed part off
/// the bucket: the push refuses on the node's root-level run state and uploads nothing.
#[test]
fn a_withheld_table_lands_nothing_in_the_bucket() {
    let bucket = tempfile::tempdir().unwrap();
    let a = project("ingest-a", &file_sync(bucket.path(), ""));
    std::fs::write(a.path().join("contextful.toml"), WITHHELD).unwrap();
    std::fs::write(a.path().join("batch.jsonl"), "{\"id\":1}\n").unwrap();
    ok(&cf(
        a.path(),
        &["context", "land", "filings", "--project", "research", "--rows", "batch.jsonl", "--run-id", "run-1", "--site-id", "site", "--now", "2030-01-01T00:00:00Z"],
        &[],
    ));
    refused(&cf(a.path(), &["sync", "push", "--project", "research"], &[]), "EnforceRootObjectNotAllowlisted");
    let keys = bucket_keys(bucket.path());
    let objects: Vec<&String> = keys.iter().filter(|k| !k.ends_with(".bucket.lock")).collect();
    assert!(objects.is_empty(), "{keys:?}");
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
