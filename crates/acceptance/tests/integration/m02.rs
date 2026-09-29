//! Milestone 2 — the store.
//!
//! Reach: a table lands, a scan resolves its file list, and a reader opens the Parquet
//! without the engine.

use contextful_acceptance::{bin, GitRepo};
use parquet::file::reader::{FileReader, SerializedFileReader};
use parquet::record::Field;
use std::process::{Command, Output};

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

#[test]
fn m02_store() {
    let cf = bin("contextful");
    let p = GitRepo::init();
    p.write(&format!("{STORE}/config.toml"), "[node]\nid = \"ingest-a\"\n");
    p.write(
        "contextful.toml",
        "authoring_posture = \"per_request\"\n[[pipeline.tables]]\nname = \"filings\"\nprimary_key = [\"document_id\"]\norder_by = \"revised_at\"\n",
    );
    p.write(
        "batch-1.jsonl",
        "{\"document_id\":\"d1\",\"revised_at\":1,\"title\":\"draft\"}\n{\"document_id\":\"d2\",\"revised_at\":1,\"title\":\"memo\"}\n",
    );
    p.write("batch-2.jsonl", "{\"document_id\":\"d1\",\"revised_at\":2,\"title\":\"final\",\"pages\":12}\n");
    p.write("batch-3.jsonl", "{\"document_id\":\"d3\",\"revised_at\":\"yesterday\"}\n");

    let land = |run: &str, rows: &str, now: &str| {
        p.run(
            &cf,
            &[
                "context", "land", "filings", "--project", "research", "--rows", rows, "--run-id", run, "--site-id", "site-a", "--now", now,
            ],
        )
    };
    let files = |extra: &[&str]| {
        let mut args = vec!["context", "files", "filings", "--project", "research"];
        args.extend_from_slice(extra);
        p.run(&cf, &args)
    };

    // A table lands: each run commits its parts under its node directory.
    ok(&land("run-0001", "batch-1.jsonl", "2030-01-01T00:00:00Z"));
    ok(&land("run-0002", "batch-2.jsonl", "2030-01-01T01:00:00Z"));
    refused(&land("run-0003", "batch-3.jsonl", "2030-01-01T01:30:00Z"), "StoreSchemaIncompatible");

    // A scan resolves an explicit sorted file list; a stray file and an uncommitted run join nothing.
    p.write(&format!("{STORE}/tables/filings/data/runs/stray.parquet"), "not parquet");
    p.write(&format!("{STORE}/tables/filings/data/runs/run-0009/ingest-a/part-00000.parquet"), "in flight");
    let listed = ok(&files(&[]));
    assert_eq!(
        listed.lines().collect::<Vec<_>>(),
        [
            "tables/filings/data/runs/run-0001/ingest-a/part-00000.parquet",
            "tables/filings/data/runs/run-0002/ingest-a/part-00000.parquet",
        ],
    );
    refused(&p.run(&cf, &["context", "files", "filling", "--project", "research"]), "StoreUnknownTable");

    // A fold publishes one snapshot through the pointer; the list names its part alone.
    let report = ok(&p.run(&cf, &["context", "compact", "filings", "--project", "research", "--now", "2030-01-01T02:00:00Z"]));
    assert!(report.contains("filings: folded"), "{report}");
    let listed = ok(&files(&[]));
    let snapshot: Vec<&str> = listed.lines().collect();
    assert_eq!(snapshot.len(), 1, "{listed}");
    assert!(snapshot[0].starts_with("tables/filings/data/snapshots/snapshot-"), "{listed}");
    assert!(snapshot[0].ends_with("/part-00000.parquet"), "{listed}");

    // A transaction-time bound before the fold resolves to the runs committed by then.
    assert_eq!(ok(&files(&["--as-of", "2030-01-01T00:30:00Z"])), "tables/filings/data/runs/run-0001/ingest-a/part-00000.parquet");

    // A reader opens the snapshot's Parquet without the engine: one row per key, the newest by `revised_at`.
    let file = std::fs::File::open(p.root.join(STORE).join(snapshot[0])).unwrap();
    let reader = SerializedFileReader::new(file).unwrap();
    assert_eq!(reader.metadata().file_metadata().num_rows(), 2);
    let mut titles = Vec::new();
    for row in reader.get_row_iter(None).unwrap() {
        let row = row.unwrap();
        let mut id = String::new();
        let mut title = String::new();
        let mut run = String::new();
        let mut stamped = false;
        for (name, field) in row.get_column_iter() {
            match (name.as_str(), field) {
                ("document_id", Field::Str(s)) => id = s.clone(),
                ("title", Field::Str(s)) => title = s.clone(),
                ("_run_id", Field::Str(s)) => run = s.clone(),
                ("_ingested_at", f) => stamped = !matches!(f, Field::Null),
                _ => {}
            }
        }
        assert!(stamped, "row {id} carries no `_ingested_at`");
        titles.push((id, title, run));
    }
    titles.sort();
    assert_eq!(
        titles,
        [
            ("d1".to_string(), "final".to_string(), "run-0002".to_string()),
            ("d2".to_string(), "memo".to_string(), "run-0001".to_string()),
        ],
    );
}

/// `contextful init` declares the project once, and every store command below it finds the
/// project without `--project`.
#[test]
fn m02_init_and_discovery() {
    let cf = bin("contextful");
    let p = GitRepo::init();
    let at = |dir: &std::path::Path, args: &[&str]| {
        Command::new(&cf).args(args).current_dir(dir).env_remove("CARGO_TARGET_DIR").env("CONTEXTFUL_NODE_ID", "ingest-a").output().unwrap()
    };

    ok(&at(&p.root, &["init", "research"]));
    assert!(p.root.join(STORE).is_dir());
    let declared = std::fs::read_to_string(p.root.join("contextful.toml")).unwrap();
    ok(&at(&p.root, &["init", "research"]));
    assert_eq!(std::fs::read_to_string(p.root.join("contextful.toml")).unwrap(), declared);
    refused(&at(&p.root, &["init", "archive"]), "StoreProjectConflict");

    p.write("contextful.toml", &format!("authoring_posture = \"per_request\"\n{declared}\n[[pipeline.tables]]\nname = \"filings\"\nprimary_key = [\"document_id\"]\n"));
    p.write("inbox/batch.jsonl", "{\"document_id\":\"d1\",\"title\":\"draft\"}\n");
    let inbox = p.root.join("inbox");
    let land = ["context", "land", "filings", "--rows", "batch.jsonl", "--run-id", "run-0001", "--site-id", "site-a", "--now", "2030-01-01T00:00:00Z"];
    ok(&at(&inbox, &land));
    assert_eq!(ok(&at(&inbox, &["context", "files", "filings"])), "tables/filings/data/runs/run-0001/ingest-a/part-00000.parquet");
    assert!(ok(&at(&inbox, &["context", "compact", "filings", "--now", "2030-01-01T01:00:00Z"])).contains("filings: folded"));

    let outside = tempfile::tempdir().unwrap();
    refused(&at(outside.path(), &["context", "files", "filings"]), "StoreProjectUndiscovered");
}
