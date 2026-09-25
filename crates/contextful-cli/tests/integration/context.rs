//! `contextful context` through the built binary, against a scratch project.

use std::path::Path;
use std::process::{Command, Output};

fn project() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    let store = dir.path().join(".contextful/context/research");
    std::fs::create_dir_all(&store).unwrap();
    std::fs::write(store.join("config.toml"), "[node]\nid = \"ingest-a\"\n").unwrap();
    std::fs::write(
        dir.path().join("contextful.toml"),
        "[[pipeline.tables]]\nname = \"filings\"\nprimary_key = [\"doc\"]\n\n[[pipeline.tables]]\nname = \"events\"\n",
    )
    .unwrap();
    std::fs::write(dir.path().join("rows.jsonl"), "{\"doc\":\"a\",\"e\":1}\n").unwrap();
    dir
}

fn run(dir: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_contextful"))
        .args(args)
        .current_dir(dir)
        .env_remove("CONTEXTFUL_NODE_ID")
        .output()
        .unwrap()
}

fn stdout(out: &Output) -> String {
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

fn land(dir: &Path, table: &str, run_id: &str, now: &str) -> String {
    stdout(&run(
        dir,
        &["context", "land", table, "--project", "research", "--rows", "rows.jsonl", "--run-id", run_id, "--site-id", "s", "--now", now],
    ))
}

/// A pass reports each table as folded, nothing-landed or failed, and a nothing-landed table does not stop the pass.
// spec: store.fold.result@e2b72226
#[test]
fn a_scheduled_pass_reports_every_table_and_continues_past_nothing_landed() {
    let p = project();
    land(p.path(), "filings", "run-1", "2030-01-01T00:00:00Z");
    land(p.path(), "events", "run-1", "2030-01-01T00:00:00Z");
    stdout(&run(p.path(), &["context", "compact", "events", "--project", "research", "--now", "2030-01-01T01:00:00Z"]));

    // `events` has nothing new, `filings` has one run 7 h old: the pass reports both.
    let report = stdout(&run(p.path(), &["context", "compact", "--project", "research", "--now", "2030-01-01T07:00:00Z"]));
    let lines: Vec<&str> = report.lines().collect();
    assert_eq!(lines.len(), 2, "{report}");
    assert_eq!(lines[0], "events: nothing-landed");
    assert!(lines[1].starts_with("filings: folded snapshot-"), "{report}");

    // A table that fails is reported, the rest still fold, and the command exits non-zero.
    land(p.path(), "events", "run-2", "2030-01-01T08:00:00Z");
    land(p.path(), "filings", "run-2", "2030-01-01T08:00:00Z");
    let manifest = p.path().join(".contextful/context/research/tables/events/data/runs/run-2/ingest-a/_manifest.json");
    std::fs::write(&manifest, "{").unwrap();
    let out = run(p.path(), &["context", "compact", "--project", "research", "--now", "2030-01-01T20:00:00Z"]);
    assert!(!out.status.success());
    let report = String::from_utf8_lossy(&out.stdout);
    assert!(report.contains("events: failed: StoreManifestUnreadable"), "{report}");
    assert!(report.contains("filings: folded"), "{report}");
}

/// A pass fires at 50 runs committed on a table, 6 h after the table's previous pass, or on `contextful context compact <table>`.
#[test]
fn a_scheduled_pass_skips_a_table_whose_trigger_has_not_fired() {
    let p = project();
    land(p.path(), "filings", "run-1", "2030-01-01T00:00:00Z");
    let report = stdout(&run(p.path(), &["context", "compact", "--project", "research", "--now", "2030-01-01T01:00:00Z"]));
    assert_eq!(report, "filings: not due");
    let report = stdout(&run(p.path(), &["context", "compact", "filings", "--project", "research", "--now", "2030-01-01T01:00:00Z"]));
    assert!(report.starts_with("filings: folded"), "{report}");
}

/// `contextful context scan` prints the file list, the relation and the bounds a read echoes.
#[test]
fn scan_prints_files_relation_and_bounds() {
    let p = project();
    land(p.path(), "filings", "run-1", "2030-01-01T00:00:00Z");
    let out = stdout(&run(p.path(), &["context", "scan", "filings", "--project", "research", "--as-of", "2030-01-02"]));
    let v: serde_json::Value = serde_json::from_str(&out).unwrap();
    assert_eq!(v["files"][0], "tables/filings/data/runs/run-1/ingest-a/part-00000.parquet");
    assert!(v["relation"].as_str().unwrap().contains("read_parquet(["));
    assert_eq!(v["contextful.bounds"], serde_json::json!({"as_of": "2030-01-03T00:00:00.000000000Z", "inclusive": false}));
    let unbounded: serde_json::Value =
        serde_json::from_str(&stdout(&run(p.path(), &["context", "scan", "filings", "--project", "research"]))).unwrap();
    assert!(unbounded.get("contextful.bounds").is_none());
}
