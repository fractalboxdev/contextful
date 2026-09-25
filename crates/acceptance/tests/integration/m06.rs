//! Milestone 6 — sync and replicas.
//!
//! Reach: two nodes share one bucket and converge without a coordinator.

use contextful_acceptance::{bin, GitRepo};
use std::path::Path;
use std::process::Output;

const STORE: &str = ".contextful/context/research";

fn ok(out: &Output) -> String {
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

fn refused(out: &Output, error: &str) {
    assert!(!out.status.success(), "expected {error}, got success: {}", String::from_utf8_lossy(&out.stdout));
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains(error), "expected {error}, got: {stderr}");
}

/// A node: its own project directory, its own node id, the shared bucket.
fn node(id: &str, bucket: &Path) -> GitRepo {
    let p = GitRepo::init();
    p.write(
        &format!("{STORE}/config.toml"),
        &format!("[node]\nid = \"{id}\"\n\n[sync]\nendpoint = \"file://{}\"\nbucket = \"context-team\"\nprefix = \"team\"\ncoordination = \"cas\"\n", bucket.display()),
    );
    p.write("contextful.toml", "[[pipeline.tables]]\nname = \"filings\"\nprimary_key = [\"document_id\"]\norder_by = \"revised_at\"\n");
    p
}

fn land(p: &GitRepo, cf: &Path, run: &str, rows: &str, now: &str) {
    p.write("batch.jsonl", rows);
    ok(&p.run(cf, &["context", "land", "filings", "--project", "research", "--rows", "batch.jsonl", "--run-id", run, "--site-id", "site", "--now", now]));
}

fn files(p: &GitRepo, cf: &Path) -> Vec<String> {
    ok(&p.run(cf, &["context", "files", "filings", "--project", "research"])).lines().map(str::to_string).collect()
}

fn sync(p: &GitRepo, cf: &Path, args: &[&str]) -> Output {
    let mut all = vec!["sync"];
    all.extend_from_slice(args);
    all.extend_from_slice(&["--project", "research"]);
    p.run(cf, &all)
}

#[test]
#[ignore = "milestone 6 is open"]
fn m06_sync() {
    let cf = bin("contextful");
    let bucket = tempfile::tempdir().unwrap();
    let a = node("ingest-a", bucket.path());
    let b = node("ingest-b", bucket.path());

    // Each node lands its own runs.
    land(&a, &cf, "run-1", "{\"document_id\":\"d1\",\"revised_at\":1,\"title\":\"draft\"}\n{\"document_id\":\"d2\",\"revised_at\":1,\"title\":\"memo\"}\n", "2030-01-01T00:00:00Z");
    land(&b, &cf, "run-1", "{\"document_id\":\"d3\",\"revised_at\":1,\"title\":\"brief\"}\n", "2030-01-01T00:00:01Z");

    // The backend demonstrates the conditional write the declared `cas` coordination rests on.
    assert!(ok(&sync(&a, &cf, &["probe"])).contains("cas"));

    // Both push at once: the bucket manifest commits by compare-and-set, and neither loses the other's entries.
    let (pa, pb) = std::thread::scope(|s| {
        let ha = s.spawn(|| sync(&a, &cf, &["push"]));
        let hb = s.spawn(|| sync(&b, &cf, &["push"]));
        (ha.join().unwrap(), hb.join().unwrap())
    });
    ok(&pa);
    ok(&pb);

    // Each pulls the other's runs; one run id on two nodes stays two runs.
    ok(&sync(&a, &cf, &["pull"]));
    ok(&sync(&b, &cf, &["pull"]));
    let (fa, fb) = (files(&a, &cf), files(&b, &cf));
    assert_eq!(fa, fb);
    assert_eq!(
        fa,
        ["tables/filings/data/runs/run-1/ingest-a/part-00000.parquet", "tables/filings/data/runs/run-1/ingest-b/part-00000.parquet"]
    );

    // Compaction takes the table's bucket lease; a second node finding it held is skipped.
    ok(&sync(&a, &cf, &["lease", "acquire", "filings", "--now", "2030-01-01T01:00:00Z"]));
    refused(&sync(&b, &cf, &["compact", "filings", "--now", "2030-01-01T01:01:00Z"]), "LeaseHeld");
    ok(&sync(&a, &cf, &["lease", "release", "filings", "--now", "2030-01-01T01:02:00Z"]));
    let folded = ok(&sync(&b, &cf, &["compact", "filings", "--now", "2030-01-01T01:03:00Z"]));
    assert!(folded.contains("folded"), "{folded}");

    // A pulls the snapshot B published under its fence: both nodes read the one snapshot.
    ok(&sync(&a, &cf, &["pull"]));
    let (fa, fb) = (files(&a, &cf), files(&b, &cf));
    assert_eq!(fa, fb, "the two nodes converge");
    assert_eq!(fa.len(), 1, "{fa:?}");
    assert!(fa[0].starts_with("tables/filings/data/snapshots/snapshot-"), "{fa:?}");
    let pointer_a = std::fs::read_to_string(a.root.join(STORE).join("tables/filings/_pointer.json")).unwrap();
    let pointer_b = std::fs::read_to_string(b.root.join(STORE).join("tables/filings/_pointer.json")).unwrap();
    assert_eq!(pointer_a, pointer_b);

    // A holder whose lease passed to a later fence publishes nothing.
    ok(&sync(&a, &cf, &["lease", "acquire", "filings", "--now", "2030-01-01T02:00:00Z"]));
    ok(&sync(&b, &cf, &["lease", "acquire", "filings", "--now", "2030-01-01T02:11:00Z"]));
    land(&a, &cf, "run-2", "{\"document_id\":\"d4\",\"revised_at\":2,\"title\":\"order\"}\n", "2030-01-01T02:12:00Z");
    refused(&sync(&a, &cf, &["compact", "filings", "--held", "--now", "2030-01-01T02:12:30Z"]), "LeaseFenced");
    ok(&sync(&b, &cf, &["pull"]));
    assert_eq!(files(&b, &cf), fb, "the fenced snapshot stays unreadable");
}
