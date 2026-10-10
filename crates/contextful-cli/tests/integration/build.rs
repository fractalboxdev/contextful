//! `contextful build` through the built binary: a declared model built and published,
//! and a published build held.
#![cfg(feature = "data-plane")]

use serde_json::Value;
use std::path::Path;
use std::process::{Command, Output};
use std::time::{Duration, Instant};

const DECLARATION: &str = r#"authoring_posture = "per_request"

[project]
name = "research"

[[pipeline.tables]]
name = "events"

[[model]]
id = "daily"
sql = "SELECT day, CAST(count(*) AS BIGINT) AS n FROM events GROUP BY day"
unique_key = ["day"]

[model.contract]
version = "1.0.0"
columns = [{ name = "day", type = "utf8", nullable = false }, { name = "n", type = "int64", nullable = false }]
"#;

fn run(dir: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_contextful")).args(args).current_dir(dir).env_remove("CONTEXTFUL_NODE_ID").output().unwrap()
}

fn stdout(out: &Output) -> String {
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

fn refused(out: &Output, error: &str) -> String {
    assert!(!out.status.success(), "expected {error}, got: {}", String::from_utf8_lossy(&out.stdout));
    let stderr = String::from_utf8_lossy(&out.stderr).trim().to_string();
    assert!(stderr.starts_with(&format!("{error}:")), "expected {error}, got: {stderr}");
    stderr
}

/// A project with three landed events.
fn project() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path();
    std::fs::write(p.join("contextful.toml"), DECLARATION).unwrap();
    std::fs::write(p.join("events.jsonl"), "{\"day\":\"d1\"}\n{\"day\":\"d1\"}\n{\"day\":\"d2\"}\n").unwrap();
    stdout(&run(p, &["context", "land", "events", "--rows", "events.jsonl", "--run-id", "r1", "--site-id", "site-a", "--now", "2030-01-01T00:00:00Z"]));
    dir
}

fn build(p: &Path, now: &str) -> Value {
    serde_json::from_str(&stdout(&run(p, &["build", "daily", "--site-id", "site-a", "--now", now, "--json"]))).unwrap()
}

// spec: run.publish.failed-attempt@b0b70724
#[test]
fn a_killed_build_reports_failed_and_keeps_the_published_build() {
    let dir = project();
    let p = dir.path();
    let published = build(p, "2030-01-01T01:00:00Z");
    std::fs::write(p.join("more.jsonl"), "{\"day\":\"d1\"}\n".repeat(10_000)).unwrap();
    stdout(&run(p, &["context", "land", "events", "--rows", "more.jsonl", "--run-id", "r2", "--site-id", "site-a", "--now", "2030-01-01T01:30:00Z"]));
    let slow = DECLARATION.replace(
        "SELECT day, CAST(count(*) AS BIGINT) AS n FROM events GROUP BY day",
        "SELECT events.day, CAST(sum(length(events.day || other.day)) AS BIGINT) AS n FROM events CROSS JOIN events AS other GROUP BY events.day",
    );
    std::fs::write(p.join("contextful.toml"), slow).unwrap();
    let mut child = Command::new(env!("CARGO_BIN_EXE_contextful"))
        .args(["build", "daily", "--site-id", "site-a", "--now", "2030-01-01T02:00:00Z"])
        .current_dir(p)
        .env_remove("CONTEXTFUL_NODE_ID")
        .spawn()
        .unwrap();
    let attempts = p.join(".contextful/context/research/tables/daily/build-attempts");
    let until = Instant::now() + Duration::from_secs(10);
    loop {
        let staged = std::fs::read_dir(&attempts).ok().is_some_and(|entries| {
            entries.filter_map(Result::ok).filter(|entry| entry.file_name().to_string_lossy().ends_with(".json")).count() >= 2
        });
        if staged {
            break;
        }
        assert!(child.try_wait().unwrap().is_none(), "build exited before staging");
        assert!(Instant::now() < until, "build did not reach staging");
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(child.try_wait().unwrap().is_none(), "the build exited before SIGKILL");
    child.kill().unwrap();
    child.wait().unwrap();
    let status: Value = serde_json::from_str(&stdout(&run(p, &["build", "status", "daily", "--json"]))).unwrap();
    assert_eq!(status["last_build_status"], "failed", "{status}");
    assert_eq!(status["published_build_id"], published["build_id"]);
    let log = std::fs::read_to_string(p.join(".contextful/context/research/tables/daily/builds.jsonl")).unwrap();
    let entries: Vec<Value> = log.lines().map(|line| serde_json::from_str(line).unwrap()).collect();
    assert_eq!(entries.len(), 2);
    assert!(entries.iter().any(|entry| entry["status"] == "failed" && entry["build_id"] == status["last_build_id"]));
    let read: Value = serde_json::from_str(&stdout(&run(p, &["query", "--json", "--project", "research", "SELECT day, n FROM daily ORDER BY day"]))).unwrap();
    assert_eq!(read["rows"], serde_json::json!([["d1", "2"], ["d2", "1"]]));
    std::fs::write(p.join("contextful.toml"), DECLARATION).unwrap();
    let recovered = build(p, "2030-01-01T03:00:00Z");
    let status: Value = serde_json::from_str(&stdout(&run(p, &["build", "status", "daily", "--json"]))).unwrap();
    assert_eq!(status["last_build_status"], "published");
    assert_eq!(status["published_build_id"], recovered["build_id"]);
    let history = std::fs::read_to_string(p.join(".contextful/context/research/tables/daily/builds.jsonl")).unwrap();
    assert!(history.lines().any(|line| line.contains("\"failed\"")), "the failed attempt remains in history");
}

// spec: run.model.status-verb@dd4c16d2
#[test]
fn model_status_derives_freshness_on_both_sides_of_max_lag() {
    let dir = project();
    let p = dir.path();
    std::fs::write(p.join("contextful.toml"), format!("{DECLARATION}\n[model.freshness]\nmax_lag = \"1h\"\n")).unwrap();
    let built = build(p, "2030-01-01T00:30:00Z");
    let status = |at| -> Value {
        serde_json::from_str(&stdout(&run(p, &["build", "status", "daily", "--now", at, "--json"]))).unwrap()
    };
    let fresh = status("2030-01-01T01:00:00Z");
    assert_eq!(fresh["published_build_id"], built["build_id"]);
    assert_eq!(fresh["stale"], false);
    assert_eq!(fresh["freshness"]["max_lag"], "1h");
    assert_eq!(status("2030-01-01T01:00:01Z")["stale"], true);
}

#[test]
fn failing_model_test_reports_refused_and_keeps_prior_publication() {
    let dir = project();
    let p = dir.path();
    let published = build(p, "2030-01-01T01:00:00Z");
    std::fs::write(p.join("contextful.toml"), format!(
        "{DECLARATION}\n[[model.test]]\nname = \"single-day\"\nsql = \"SELECT * FROM daily WHERE n > 1\"\n"
    )).unwrap();
    refused(&run(p, &["build", "daily", "--site-id", "site-a", "--now", "2030-01-01T02:00:00Z"]), "ModelTestFailed");
    let status: Value = serde_json::from_str(&stdout(&run(p, &["build", "status", "daily", "--json"]))).unwrap();
    assert_eq!(status["last_build_status"], "refused");
    assert_eq!(status["published_build_id"], published["build_id"]);
    let refused_id = status["last_build_id"].as_str().unwrap().to_string();
    let read: Value = serde_json::from_str(&stdout(&run(p, &["query", "--json", "--project", "research", "SELECT day, n FROM daily ORDER BY day"]))).unwrap();
    assert_eq!(read["rows"], serde_json::json!([["d1", "2"], ["d2", "1"]]));
    std::fs::write(p.join("contextful.toml"), DECLARATION).unwrap();
    let recovered = build(p, "2030-01-01T02:00:00Z");
    assert_ne!(recovered["build_id"], refused_id, "a retry at the same instant must preserve the refused attempt");
    let history = std::fs::read_to_string(p.join(".contextful/context/research/tables/daily/builds.jsonl")).unwrap();
    let entries: Vec<Value> = history.lines().map(|line| serde_json::from_str(line).unwrap()).collect();
    assert!(entries.iter().any(|entry| entry["build_id"] == refused_id && entry["status"] == "refused"));
    assert!(entries.iter().any(|entry| entry["build_id"] == recovered["build_id"] && entry["status"] == "published"));
}

/// `contextful build <model>` materializes the model into staging, checks its contract, runs its tests, then commits; `--json` prints the build id, row count and watermark.
// spec: run.model.build-verb@20011366
#[test]
fn build_publishes_a_model_and_prints_its_receipt() {
    let dir = project();
    let p = dir.path();
    let built = build(p, "2030-01-01T01:00:00Z");
    assert_eq!(built["model"], "daily");
    assert_eq!(built["build_id"], "snapshot-01893459600000000000");
    assert_eq!(built["rows"], 2);
    assert_eq!(built["published"], true);
    assert_eq!(built["watermark"]["at"], "2030-01-01T00:00:00Z");
    assert!(built["watermark"]["inputs"]["events"]["runs"][0].as_str().unwrap().starts_with("r1/"));
    let read: Value = serde_json::from_str(&stdout(&run(p, &["query", "--json", "--project", "research", "SELECT day, n FROM daily ORDER BY day"]))).unwrap();
    assert_eq!(read["rows"], serde_json::json!([["d1", "2"], ["d2", "1"]]));
    let text = stdout(&run(p, &["build", "daily", "--site-id", "site-a", "--now", "2030-01-01T02:00:00Z"]));
    assert!(text.starts_with("daily: built snapshot-") && text.ends_with("2 rows"), "{text}");
    // The validation verb reads the model block.
    let valid = stdout(&run(p, &["pipeline", "validate"]));
    assert!(valid.contains("daily: valid model (0 tests)"), "{valid}");
}

// spec: run.model.statement-source@e0da375b
#[test]
fn build_and_validate_read_a_local_model_statement_file() {
    let dir = project();
    let p = dir.path();
    let statement = "SELECT day, CAST(count(*) AS BIGINT) AS n FROM events GROUP BY day";
    std::fs::write(p.join("daily.sql"), statement).unwrap();
    std::fs::write(p.join("contextful.toml"), DECLARATION.replace(&format!("sql = \"{statement}\""), "sql_file = \"daily.sql\"")).unwrap();
    assert!(stdout(&run(p, &["pipeline", "validate"])).contains("daily: valid model"));
    assert_eq!(build(p, "2030-01-01T01:00:00Z")["rows"], 2);
    std::fs::write(p.join("contextful.toml"), DECLARATION.replace(&format!("sql = \"{statement}\""), "sql_file = \"daily.sql\"\nsql = \"SELECT 1\"")).unwrap();
    let invalid = run(p, &["pipeline", "validate"]);
    assert!(!invalid.status.success());
    assert!(String::from_utf8_lossy(&invalid.stderr).contains("PipelineSpecInvalid: model `daily` declares exactly one"));
}

/// `pipeline validate` names undeclared relations on stderr while allowing the store to resolve them at build.
// spec: run.model.validate-undeclared@7dd935aa
#[test]
fn validate_names_a_relation_no_manifest_declares() {
    let dir = project();
    let p = dir.path();
    std::fs::write(p.join("contextful.toml"), DECLARATION.replace("FROM events", "FROM evnts")).unwrap();
    refused(&run(p, &["build", "daily", "--site-id", "site-a", "--now", "2030-01-01T01:00:00Z"]), "EnforceUnknownRelation");
    let v = run(p, &["pipeline", "validate"]);
    assert!(stdout(&v).contains("daily: valid model (0 tests)"));
    let stderr = String::from_utf8_lossy(&v.stderr);
    assert!(stderr.contains("model `daily` reads `evnts`, which no manifest declares"), "{stderr}");
    std::fs::write(p.join("contextful.toml"), DECLARATION).unwrap();
    let v = run(p, &["pipeline", "validate"]);
    stdout(&v);
    assert!(!String::from_utf8_lossy(&v.stderr).contains("no manifest declares"));
}

/// `build` naming no declared model raises `ModelUndeclared`, naming the declared models.
// spec: run.model.unknown-model@28046609
#[test]
fn build_of_an_undeclared_model_is_refused() {
    let dir = project();
    let e = refused(&run(dir.path(), &["build", "dialy", "--site-id", "site-a"]), "ModelUndeclared");
    assert!(e.contains("`dialy`") && e.contains("[daily]"), "{e}");
    assert!(!dir.path().join(".contextful/context/research/tables/dialy").exists());
}

/// `build hold --for <n>[smhd] <model> <build>` commits a hold until now plus the duration and prints `Held`, or `Renewed` over an unexpired hold; `--json` prints the receipt as an object.
// spec: run.model.hold-verb@4d1d9ed4
#[test]
fn build_hold_prints_held_then_renewed() {
    let dir = project();
    let p = dir.path();
    let build_id = build(p, "2030-01-01T01:00:00Z")["build_id"].as_str().unwrap().to_string();
    let hold = |now: &str, json: bool| {
        let mut args = vec!["build", "hold", "--for", "7d", "daily", build_id.as_str(), "--by", "ops", "--now", now];
        if json {
            args.push("--json");
        }
        stdout(&run(p, &args))
    };
    let first: Value = serde_json::from_str(&hold("2030-01-01T02:00:00Z", true)).unwrap();
    assert_eq!(first["receipt"], "Held");
    assert_eq!(first["build_id"], build_id.as_str());
    assert_eq!(first["principal"], "ops");
    assert_eq!(first["expires_at"], "2030-01-08T02:00:00Z");
    let second = hold("2030-01-02T00:00:00Z", false);
    assert_eq!(second, format!("Renewed daily build {build_id} until 2030-01-09T00:00:00Z by ops"));
    let e = refused(&run(p, &["build", "hold", "--for", "7d", "daily", "snapshot-1", "--by", "ops"]), "ModelBuildUnknown");
    assert!(e.contains(&build_id), "{e}");
    let bad = run(p, &["build", "hold", "--for", "7w", "daily", &build_id]);
    assert!(!bad.status.success() && String::from_utf8_lossy(&bad.stderr).contains("7w"));
}

/// `pipeline validate` admits each model's `sql` and every test's statement as a build does, counting every relation but the model's own id in `sql` as registered, holds each declared input to `run.model.restricted-input`, and raises the error the build raises.
// spec: run.model.validate-statements@a705bcc8
#[test]
fn validate_refuses_a_statement_build_refuses() {
    let cases = [
        ("FROM events GROUP BY day", "FROM ref('events') GROUP BY day", "TableFunctionRefused"),
        ("FROM events GROUP BY day", "FROM duckdb_tables() GROUP BY day", "TableFunctionRefused"),
        ("FROM events GROUP BY day", "FROM daily GROUP BY day", "EnforceUnknownRelation"),
        ("name = \"events\"\n", "name = \"events\"\nclass = \"email\"\n", "ModelInputRestricted"),
    ];
    for (from, to, error) in cases {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("contextful.toml"), DECLARATION.replace(from, to)).unwrap();
        let e = refused_at(&run(dir.path(), &["pipeline", "validate"]), error);
        assert!(e.contains("model `daily`"), "{e}");
    }
    let dir = tempfile::tempdir().unwrap();
    let tested = format!("{DECLARATION}\n[[model.test]]\nname = \"peek\"\nsql = \"SELECT * FROM read_csv('x.csv')\"\n");
    std::fs::write(dir.path().join("contextful.toml"), tested).unwrap();
    let e = refused_at(&run(dir.path(), &["pipeline", "validate"]), "TableFunctionRefused");
    assert!(e.contains("model `daily` test `peek`"), "{e}");
}

/// A refusal under its declaration site: `<file>:<line> <what>: <error>: <detail>`.
fn refused_at(out: &Output, error: &str) -> String {
    assert!(!out.status.success(), "expected {error}, got: {}", String::from_utf8_lossy(&out.stdout));
    let stderr = String::from_utf8_lossy(&out.stderr).trim().to_string();
    assert!(stderr.contains(&format!(": {error}:")), "expected {error}, got: {stderr}");
    stderr
}
