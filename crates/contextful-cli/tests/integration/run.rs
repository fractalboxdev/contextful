//! `contextful run` through the built binary: history, and the refusals each surface raises.

use std::path::Path;
use std::process::{Command, Output};

fn project() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    let store = dir.path().join(".contextful/context/research");
    std::fs::create_dir_all(&store).unwrap();
    std::fs::write(store.join("config.toml"), "[node]\nid = \"ingest-a\"\n").unwrap();
    std::fs::write(dir.path().join("contextful.toml"), "[[pipeline.tables]]\nname = \"filings\"\n").unwrap();
    std::fs::write(dir.path().join("empty.sh"), "printf '{\"rows\":[],\"more\":false}'\n").unwrap();
    for p in ["feed-a", "feed-b"] {
        std::fs::write(
            dir.path().join(format!("{p}.toml")),
            format!("pipeline = \"{p}\"\ntable = \"filings\"\n[connector]\nid = \"vendor\"\nversion = \"1\"\ncommand = [\"sh\", \"empty.sh\"]\n"),
        )
        .unwrap();
    }
    dir
}

fn cf(dir: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_contextful")).args(args).current_dir(dir).env_remove("CONTEXTFUL_NODE_ID").output().unwrap()
}

fn ok(out: &Output) -> String {
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

fn refused(out: &Output, error: &str) {
    assert!(!out.status.success(), "expected {error}, got success: {}", String::from_utf8_lossy(&out.stdout));
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains(error), "expected {error}, got: {stderr}");
}

fn start(dir: &Path, plan: &str, run: &str, now: &str) -> Output {
    cf(dir, &["run", "start", "--plan", plan, "--project", "research", "--run-id", run, "--site-id", "site-a", "--now", now])
}

fn history(dir: &Path, extra: &[&str]) -> Output {
    let mut args = vec!["run", "history", "--project", "research"];
    args.extend_from_slice(extra);
    cf(dir, &args)
}

/// History leaves the machine as a process listing and an NDJSON export over one run projection; the export's
/// first line carries store, window, count and truncation flag, then one run per line. It resolves no bucket.
// spec: run.record.history-export@f66be52e
#[test]
fn history_exports_a_header_then_one_run_per_line() {
    let dir = project();
    for (i, run) in ["a1", "a2", "a3"].iter().enumerate() {
        ok(&start(dir.path(), "feed-a.toml", run, &format!("2030-01-01T00:0{i}:00Z")));
    }
    let listing: serde_json::Value = serde_json::from_str(&ok(&history(dir.path(), &[]))).unwrap();
    assert_eq!(listing["runs"].as_array().unwrap().len(), 3);
    let text = ok(&history(dir.path(), &["--export", "--since", "2030-01-01T00:01:00Z"]));
    let lines: Vec<serde_json::Value> = text.lines().map(|l| serde_json::from_str(l).unwrap()).collect();
    assert_eq!(lines[0]["store"], "research");
    assert_eq!(lines[0]["window"]["since"], "2030-01-01T00:01:00Z");
    assert_eq!(lines[0]["count"], 2);
    assert_eq!(lines[0]["truncated"], false);
    assert_eq!(lines[1..].iter().map(|r| r["run_id"].as_str().unwrap()).collect::<Vec<_>>(), ["a3", "a2"]);
    // Each run line is the run row the listing serves.
    assert_eq!(lines[1], listing["runs"][0]);
}

/// The export answers with 500 rows by default and clamps at 5000 rows, each pipeline's window taking the full
/// ceiling before the merged result is clipped once.
// spec: run.record.export-ceiling@81c87e7e
#[test]
fn each_pipeline_takes_the_full_ceiling_and_the_merge_clips_once() {
    use contextful_core::run::record::export_ceiling;
    assert_eq!((export_ceiling(None), export_ceiling(Some(9_999))), (500, 5_000));
    let dir = project();
    for (i, run) in ["a1", "a2", "a3"].iter().enumerate() {
        ok(&start(dir.path(), "feed-a.toml", run, &format!("2030-01-01T00:0{i}:00Z")));
    }
    ok(&start(dir.path(), "feed-b.toml", "b1", "2030-01-01T00:05:00Z"));
    let text = ok(&history(dir.path(), &["--export", "--limit", "2", "--pipeline", "feed-a", "--pipeline", "feed-b"]));
    let lines: Vec<serde_json::Value> = text.lines().map(|l| serde_json::from_str(l).unwrap()).collect();
    assert_eq!((lines[0]["count"].as_u64(), lines[0]["truncated"].as_bool()), (Some(2), Some(true)));
    // feed-a contributes its two newest, feed-b its one; the merge keeps the two newest overall.
    assert_eq!(lines[1..].iter().map(|r| r["run_id"].as_str().unwrap()).collect::<Vec<_>>(), ["b1", "a3"]);
    let unasked = ok(&history(dir.path(), &["--export"]));
    assert_eq!(unasked.lines().count(), 1 + 4);
}

#[test]
fn every_run_surface_raises_its_refusal_by_name() {
    let dir = project();
    refused(&history(dir.path(), &["--since", "2030-01-01T00:00:00+08:00"]), "HistoryBoundSpelling");
    refused(&cf(dir.path(), &["run", "start", "--plan", "feed-a.toml", "--project", "research"]), "SiteIdUnresolved");
    refused(
        &cf(dir.path(), &["run", "start", "--plan", "feed-a.toml", "--project", "research", "--site-id-env", "CONTEXTFUL_TEST_UNSET_SITE"]),
        "SiteIdUnresolved",
    );
    ok(&start(dir.path(), "feed-a.toml", "a1", "2030-01-01T00:00:00Z"));
    refused(&cf(dir.path(), &["run", "cancel", "a1", "--project", "research"]), "CancelTargetNotInFlight");
    refused(&cf(dir.path(), &["run", "cancel", "nope", "--project", "research"]), "CancelTargetNotInFlight");
    std::fs::write(dir.path().join("payload.json"), "{}").unwrap();
    refused(&cf(dir.path(), &["run", "awake", "0123abcd", "--project", "research", "--payload", "payload.json"]), "AwakeableUnknown");
    std::fs::write(dir.path().join("redacting.toml"), format!("redact = [\"ssn\"]\n{}", std::fs::read_to_string(dir.path().join("feed-a.toml")).unwrap())).unwrap();
    refused(&start(dir.path(), "redacting.toml", "r1", "2030-01-01T00:00:00Z"), "JournalRedactionConflict");
}

#[test]
fn an_unknown_stop_scope_is_refused() {
    let dir = project();
    ok(&start(dir.path(), "feed-a.toml", "a1", "2030-01-01T00:00:00Z"));
    let out = cf(dir.path(), &["run", "cancel", "a1", "--project", "research", "--scope", "fire"]);
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("fire") && stderr.contains("pipeline"), "{stderr}");
}

/// The binary keeps its run rows, cursor cache and lease rows in the store root's
/// `machine.sqlite`, and every run surface reads them back from there.
#[test]
fn run_rows_live_in_the_store_roots_machine_catalog() {
    use contextful_core::coordinate::Catalog;
    use contextful_core::ports::FixedClock;
    use contextful_core::time::Instant;
    let dir = project();
    ok(&start(dir.path(), "feed-a.toml", "a1", "2030-01-01T00:00:00Z"));
    let file = dir.path().join(".contextful/context/research/machine.sqlite");
    assert!(file.is_file(), "no machine catalog at {}", file.display());
    assert!(!dir.path().join(".contextful/run/research/runs").exists(), "no run row lands beside the journal");

    let clock = std::sync::Arc::new(FixedClock(Instant::parse("2030-01-01T00:00:00Z").unwrap()));
    let catalog = contextful_sqlite::MachineCatalog::open(&file, clock).unwrap();
    let row = catalog.run("a1").unwrap().expect("the run row");
    assert_eq!((row.pipeline_id.as_str(), row.table.as_str()), ("feed-a", "filings"));
    let shown: serde_json::Value = serde_json::from_str(&ok(&cf(dir.path(), &["run", "show", "a1", "--project", "research"]))).unwrap();
    assert_eq!(shown["run_id"], "a1");
    assert_eq!(shown["status"], serde_json::to_value(row.status).unwrap());
}
