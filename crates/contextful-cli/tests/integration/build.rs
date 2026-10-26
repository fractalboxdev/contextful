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

#[test]
fn a_published_build_records_its_coordinated_frontier_without_exporting() {
    let dir = project();
    let p = dir.path();
    let group = "[[export]]\nname='daily-copy'\ntable='daily'\nendpoint='http://127.0.0.1:1/events'\nformat='changes-v1'\nkey=['day']\nschedule='every 30s'\n[[publication]]\nname='daily-projection'\njob='daily'\nmembers=['daily-copy']\n";
    std::fs::write(p.join("contextful.toml"),format!("{DECLARATION}\n{group}")).unwrap();
    let built = build(p,"2030-01-01T00:01:00Z");
    let ledger = contextful_sqlite::PublicationLedger::open(&p.join(".contextful/context/research/machine.sqlite")).unwrap();
    let source = ledger.source("daily-projection").unwrap().unwrap();
    assert_eq!(source.run,built["build_id"].as_str().unwrap());
    assert_eq!(source.input,built["watermark"]);
    assert_eq!(source.frontiers.len(),1);
    assert!(ledger.active_namespace("daily-projection").unwrap().is_none());
}

#[test]
fn a_scheduled_build_records_the_local_publication_over_its_applied_outputs() {
    let dir = project();
    let p = dir.path();
    let group = "[[job]]\nname='daily-build'\nkind='build'\ntarget='daily'\nschedule='every 1h'\n[[export]]\nname='daily-copy'\ntable='daily'\nendpoint='http://127.0.0.1:1/events'\nformat='changes-v1'\nkey=['day']\nschedule='every 30s'\n[[publication]]\nname='daily-projection'\njob='daily-build'\nmembers=['daily-copy']\n";
    std::fs::write(p.join("contextful.toml"),format!("site_id='site-a'\n{DECLARATION}\n{group}")).unwrap();
    stdout(&run(p,&["pipeline","import","--project","research"]));
    for _ in 0..2 {
        stdout(&run(p,&["pipeline","serve","--cycle","--project","research","--now","2030-01-01T00:01:00Z"]));
    }
    let ledger = contextful_sqlite::PublicationLedger::open(&p.join(".contextful/context/research/machine.sqlite")).unwrap();
    let source = ledger.source("daily-projection").unwrap().expect("an applied producer retains its local delivery group");
    assert!(!source.run.is_empty());
    assert_eq!(source.frontiers.len(),1);
    assert!(!source.input.is_null());
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
// spec: run.publish.staleness@bb1ce925
#[test]
fn model_status_derives_freshness_on_both_sides_of_max_lag() {
    let dir = project();
    let p = dir.path();
    std::fs::write(p.join("contextful.toml"), format!("{DECLARATION}\n[model.freshness]\nmax_lag = \"1h\"\n")).unwrap();
    let built = build(p, "2030-01-01T00:30:00Z");
    let status = |at| -> Value {
        serde_json::from_str(&stdout(&run(p, &["build", "status", "daily", "--now", at, "--json"]))).unwrap()
    };
    // No landing after the build leaves it fresh past `max_lag`.
    let quiet = status("2030-01-03T00:30:00Z");
    assert_eq!(quiet["published_build_id"], built["build_id"]);
    assert_eq!(quiet["stale"], false);
    assert_eq!(quiet["stale_inputs"], serde_json::json!([]));
    assert_eq!(quiet["freshness"]["max_lag"], "1h");
    stdout(&run(p, &["context", "land", "events", "--rows", "events.jsonl", "--run-id", "r2", "--site-id", "site-a", "--now", "2030-01-04T00:00:00Z"]));
    assert_eq!(status("2030-01-04T00:59:59Z")["stale"], false);
    let stale = status("2030-01-04T01:00:01Z");
    assert_eq!(stale["stale"], true);
    assert_eq!(stale["stale_inputs"], serde_json::json!(["events"]));
    build(p, "2030-01-04T02:00:00Z");
    let rebuilt = status("2030-01-04T02:00:00Z");
    assert_eq!(rebuilt["stale"], false);
    assert_eq!(rebuilt["stale_inputs"], serde_json::json!([]));
}

const CYCLE: &str = r#"authoring_posture = "per_request"
site_id = "site-a"

[project]
name = "research"

[[pipeline.tables]]
name = "events"

[[model]]
id = "a"
sql = "SELECT day FROM b"
publish = false

[[model]]
id = "b"
sql = "SELECT day FROM a"
publish = false
"#;

fn cycle_refused(out: &Output, error: &str) {
    assert!(!out.status.success(), "expected {error}, got: {}", String::from_utf8_lossy(&out.stdout));
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains(error) && stderr.contains("`a`, `b`"), "{stderr}");
}

// spec: run.model.model-cycle@31a50a17
#[test]
fn validate_apply_and_build_refuse_a_two_model_cycle() {
    let dir = project();
    let p = dir.path();
    std::fs::write(p.join("contextful.toml"), CYCLE).unwrap();
    cycle_refused(&run(p, &["pipeline", "validate"]), "ModelCycle");
    cycle_refused(&run(p, &["pipeline", "import", "--project", "research"]), "ModelCycle");
    cycle_refused(&run(p, &["build", "a", "--site-id", "site-a", "--now", "2030-01-01T01:00:00Z"]), "ModelCycle");
    assert!(!p.join(".contextful/context/research/tables/a").exists());
    let acyclic = CYCLE.replace("SELECT day FROM a", "SELECT day FROM events");
    std::fs::write(p.join("contextful.toml"), &acyclic).unwrap();
    stdout(&run(p, &["pipeline", "import", "--project", "research"]));
    std::fs::write(p.join("contextful.toml"), CYCLE).unwrap();
    cycle_refused(&run(p, &["pipeline", "apply", "--project", "research"]), "ModelCycle");
}

// spec: run.model.build-frontier@3cc993c1
#[test]
fn build_fires_no_pipeline_and_reads_the_committed_frontier() {
    let dir = project();
    let p = dir.path();
    let fed = DECLARATION.replace(
        "[[pipeline.tables]]\nname = \"events\"\n",
        "[[pipeline]]\nid = \"feed\"\ntables = [\"events\"]\n\n[pipeline.source]\nname = \"http\"\nconfig = { endpoint = \"http://127.0.0.1:1/events\" }\n",
    );
    std::fs::write(p.join("contextful.toml"), fed).unwrap();
    let built = build(p, "2030-01-01T01:00:00Z");
    let events = &built["watermark"]["inputs"]["events"];
    let r1 = contextful_context::Store::open(p, "research").unwrap().committed_runs("events").unwrap();
    assert_eq!(r1.len(), 1, "{built}");
    assert_eq!(events["omitted"], serde_json::to_value(contextful_core::pipeline::model::Omitted::of([r1[0].key()])).unwrap(), "{built}");
    let landed = std::fs::read_dir(p.join(".contextful/context/research/tables/events/data/runs")).unwrap().count();
    assert_eq!(landed, 1, "a build records no run of its feeder");
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
    assert_eq!(built["watermark"]["inputs"]["events"]["omitted"]["count"], 1);
    assert_eq!(built["watermark"]["inputs"]["events"]["through"], 1);
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

/// `contextful build <model> --available-as-of <t>` reads every input at `available_as_of` equal to `t` and records `t` as the watermark's `horizon`; a build without the flag records a null horizon.
// spec: run.model.build-horizon@d3d41c05
#[test]
fn a_build_at_a_horizon_reads_inputs_available_by_it_and_records_it() {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path();
    let declaration = DECLARATION.replace("name = \"events\"\n", "name = \"events\"\ncolumns = { published_at = \"timestamp\" }\n\n[pipeline.tables.available_time]\ncolumn = \"published_at\"\n");
    std::fs::write(p.join("contextful.toml"), &declaration).unwrap();
    // Three events backfilled in one landing, published on three days.
    std::fs::write(
        p.join("events.jsonl"),
        "{\"day\":\"d1\",\"published_at\":\"2029-12-01T00:00:00Z\"}\n{\"day\":\"d1\",\"published_at\":\"2029-12-10T00:00:00Z\"}\n{\"day\":\"d2\",\"published_at\":\"2029-12-20T00:00:00Z\"}\n",
    )
    .unwrap();
    stdout(&run(p, &["context", "land", "events", "--rows", "events.jsonl", "--run-id", "r1", "--site-id", "site-a", "--now", "2030-01-01T00:00:00Z"]));

    let bounded: Value = serde_json::from_str(&stdout(&run(p, &["build", "daily", "--site-id", "site-a", "--now", "2030-01-01T01:00:00Z", "--available-as-of", "2029-12-15T00:00:00Z", "--json"]))).unwrap();
    assert_eq!(bounded["rows"], 1, "{bounded}");
    assert_eq!(bounded["watermark"]["horizon"], "2029-12-15T00:00:00Z", "{bounded}");
    let read: Value = serde_json::from_str(&stdout(&run(p, &["query", "--json", "--project", "research", "SELECT day, n FROM daily ORDER BY day"]))).unwrap();
    assert_eq!(read["rows"], serde_json::json!([["d1", "2"]]));

    let open = build(p, "2030-01-01T02:00:00Z");
    assert_eq!(open["rows"], 2);
    assert_eq!(open["watermark"]["horizon"], Value::Null, "{open}");
    assert!(open["watermark"].as_object().unwrap().contains_key("horizon"), "a build without the flag records a null horizon");

    // An input declaring no availability time refuses the bound rather than reading past it.
    let plain = project();
    let out = run(plain.path(), &["build", "daily", "--site-id", "site-a", "--now", "2030-01-01T01:00:00Z", "--available-as-of", "2029-12-15T00:00:00Z"]);
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("StoreAvailableTimeUndeclared"), "{}", String::from_utf8_lossy(&out.stderr));
}

const SUPERSEDED: &str = "x-interrupted";

/// A scheduled build job feeding a coordinated publication whose receiver accepts every
/// group: imported as v1, fired once at 01:00 and exported once.
fn published_job(receiver: &contextful_acceptance::http::Server) -> (tempfile::TempDir, String, String) {
    let dir = project();
    let p = dir.path();
    let group = format!("[[job]]\nname='daily-build'\nkind='build'\ntarget='daily'\nschedule='every 1h'\n[[export]]\nname='daily-copy'\ntable='daily'\nendpoint='{}'\nformat='changes-v1'\nkey=['day']\nschedule='every 30s'\n[[publication]]\nname='daily-projection'\njob='daily-build'\nmembers=['daily-copy']\n", receiver.url("/events"));
    std::fs::write(p.join("contextful.toml"), format!("site_id='site-a'\n{DECLARATION}\n{group}")).unwrap();
    std::fs::write(p.join(".contextful/issuance.toml"), "default_audience = 'contextful://acme-research'\nmax_lifetime_secs = 3600\n").unwrap();
    stdout(&run(p, &["pipeline", "import", "--project", "research"]));
    stdout(&run(p, &["pipeline", "serve", "--cycle", "--project", "research", "--now", "2030-01-01T01:00:00Z"]));
    let public = stdout(&run(p, &["token", "keygen", "--out", ".contextful/issuer.seed"]));
    let token = stdout(&run(p, &["token", "mint", "--issuer-key", ".contextful/issuer.seed", "--on-behalf-of", "svc://projection", "--zone", "on-prem:hq", "--table", "daily", "--ttl", "600"]));
    export_group(p, &public, &token);
    assert_eq!(receiver.received("/events").len(), 1);
    (dir, public, token)
}

fn export_group(p: &Path, public: &str, token: &str) {
    let out = Command::new(env!("CARGO_BIN_EXE_contextful"))
        .args(["export", "group", "daily-projection", "--project", "research", "--public-key", public, "--audience", "contextful://acme-research"])
        .current_dir(p).env("CONTEXTFUL_TOKEN", token).env_remove("CONTEXTFUL_NODE_ID").output().unwrap();
    stdout(&out);
}

fn machine(p: &Path) -> contextful_sqlite::MachineCatalog {
    let clock = std::sync::Arc::new(contextful_core::ports::FixedClock(contextful_core::time::Instant::parse("2030-01-01T01:00:00Z").unwrap()));
    contextful_sqlite::MachineCatalog::open(&p.join(".contextful/context/research/machine.sqlite"), clock).unwrap()
}

/// Leave the job's scope as a container replaced during the plan phase leaves it: a
/// running attempt at phase `plan` whose owner lease lapses, holding a pending owner pinned
/// to the plan reference the completed fire ran under. Answers that reference.
fn interrupt_in_plan_phase(p: &Path) -> String {
    use contextful_core::coordinate::Catalog;
    use contextful_core::run::own::{ExecutionOwner, OwnerScope, PlanPins};
    use contextful_core::run::record::{Owner, Phase, RunStatus};
    let catalog = machine(p);
    let mut row = catalog.runs(None).unwrap().into_iter()
        .find(|r| r.host_scope.as_deref() == Some("job:daily-build") && r.status == RunStatus::Success)
        .expect("the completed fire");
    let plan_ref = row.plan_ref.clone().expect("a host attempt records its plan reference");
    row.run_id = "build-job-interrupted".into();
    row.execution_id = SUPERSEDED.into();
    row.status = RunStatus::Running;
    row.phase = Phase::Plan;
    row.ended_at = None;
    row.rows = 0;
    row.owner = Some(Owner::leased(1, "replaced-container", row.started_at));
    catalog.put_run(&row).unwrap();
    catalog.put_owner(&ExecutionOwner {
        execution_id: SUPERSEDED.into(),
        scope: OwnerScope::host("job:daily-build"),
        pins: PlanPins { plan_ref: plan_ref.clone(), identities: Default::default() }.into(),
        attempts: vec![row.run_id.clone()],
        opened_at: row.started_at,
        next_attempt_at: None,
    }).unwrap();
    plan_ref
}

/// Change the job's target model and land a third day the next build publishes.
fn change_declaration(p: &Path) {
    let declaration = std::fs::read_to_string(p.join("contextful.toml")).unwrap();
    std::fs::write(p.join("contextful.toml"), declaration.replace("FROM events GROUP BY day", "FROM events WHERE day IS NOT NULL GROUP BY day")).unwrap();
    std::fs::write(p.join("third.jsonl"), "{\"day\":\"d3\"}\n").unwrap();
    stdout(&run(p, &["context", "land", "events", "--rows", "third.jsonl", "--run-id", "r3", "--site-id", "site-a", "--now", "2030-01-01T01:30:00Z"]));
}

fn history(p: &Path) -> Vec<Value> {
    stdout(&run(p, &["run", "history", "--project", "research", "--export"])).lines().skip(1).map(|l| serde_json::from_str(l).unwrap()).collect()
}

/// The retirement's row, and the job's attempts that completed after it.
fn retirement_and_completions(p: &Path) -> (Value, usize) {
    let rows = history(p);
    let retired = rows.iter().find(|r| r["execution_id"] == SUPERSEDED && r["status"] == "canceled").cloned().unwrap_or_else(|| panic!("no retirement row: {rows:?}"));
    let completed = rows.iter().filter(|r| r["host_scope"] == "job:daily-build" && r["status"] == "success").count();
    (retired, completed)
}

// spec: surface.fire.executable-pin@462b2270
/// `contextful run rewind` retires a pending execution pinned to a superseded plan reference, recording the reason
/// and that reference, so the job's next fire plans fresh and completes and its publication exports again.
// spec: run.own.rewind@7fbd9152
// spec: run.own.rewind-record@e95e062b
#[test]
fn a_rewound_plan_phase_interruption_leaves_the_job_fireable_after_a_declaration_change() {
    let receiver = contextful_acceptance::http::Server::start(|_| contextful_acceptance::http::Response::json(200, "{}"));
    let (dir, public, token) = published_job(&receiver);
    let p = dir.path();
    let superseded = interrupt_in_plan_phase(p);
    change_declaration(p);
    let fire = || run(p, &["job", "fire", "daily-build", "--project", "research", "--now", "2030-01-01T02:00:00Z"]);
    let out = fire();
    assert!(!out.status.success());
    let mismatch = String::from_utf8_lossy(&out.stderr).trim().to_string();
    assert!(mismatch.contains("ExecutionPinMismatch: host scope `job:daily-build`"), "{mismatch}");
    assert!(mismatch.contains(&format!("contextful run rewind {SUPERSEDED} --reason <text>")), "{mismatch}");
    assert!(!mismatch.ends_with("or rewind"), "{mismatch}");
    let unknown = run(p, &["run", "rewind", "x-unknown", "--project", "research", "--reason", "declaration changed", "--now", "2030-01-01T02:00:00Z"]);
    refused(&unknown, "ExecutionRewindNotPending");
    let rewound = stdout(&run(p, &["run", "rewind", SUPERSEDED, "--project", "research", "--reason", "declaration changed", "--now", "2030-01-01T02:00:00Z"]));
    assert!(rewound.contains(&superseded), "{rewound}");
    stdout(&fire());
    let (retired, completed) = retirement_and_completions(p);
    assert_eq!(retired["plan_ref"], superseded.as_str());
    assert!(retired["error_message"].as_str().unwrap().contains("declaration changed"), "{retired}");
    assert_eq!(completed, 2);
    export_group(p, &public, &token);
    let delivered = receiver.received("/events");
    assert_eq!(delivered.len(), 2, "the publication exports again after the job completes");
    let batch: Value = serde_json::from_slice(&delivered[1].body).unwrap();
    assert!(batch.to_string().contains("d3"), "{batch}");
}

/// `pipeline apply` retires a job's pending owner whose declared plan reference the applied snapshot no longer
/// produces, recording the snapshot version, so the next scheduled fire plans fresh and completes.
// spec: run.own.apply-retires-superseded@ecdb4cdb
#[test]
fn applying_a_changed_declaration_retires_a_plan_phase_interruption() {
    let receiver = contextful_acceptance::http::Server::start(|_| contextful_acceptance::http::Response::json(200, "{}"));
    let (dir, public, token) = published_job(&receiver);
    let p = dir.path();
    let superseded = interrupt_in_plan_phase(p);
    change_declaration(p);
    let applied = stdout(&run(p, &["pipeline", "apply", "--project", "research", "--now", "2030-01-01T01:45:00Z"]));
    assert!(applied.contains(&format!("retired execution `{SUPERSEDED}`")), "{applied}");
    stdout(&run(p, &["pipeline", "serve", "--cycle", "--project", "research", "--now", "2030-01-01T02:00:00Z"]));
    let (retired, completed) = retirement_and_completions(p);
    assert_eq!(retired["plan_ref"], superseded.as_str());
    assert!(retired["error_message"].as_str().unwrap().contains("superseded by applied snapshot v2"), "{retired}");
    assert_eq!(completed, 2, "{:?}", history(p));
    export_group(p, &public, &token);
    assert_eq!(receiver.received("/events").len(), 2, "the publication exports again after the job completes");
}

/// An applied retirement meeting a live attempt leaves the owner pending and the snapshot claimed, and names the verb.
// spec: run.own.apply-retire-refused@3db6c11b
#[test]
fn apply_leaves_an_owner_a_live_attempt_holds_and_names_the_verb() {
    use contextful_core::coordinate::Catalog;
    use contextful_core::run::own::OwnerScope;
    let receiver = contextful_acceptance::http::Server::start(|_| contextful_acceptance::http::Response::json(200, "{}"));
    let (dir, _, _) = published_job(&receiver);
    let p = dir.path();
    interrupt_in_plan_phase(p);
    change_declaration(p);
    // The attempt still renews its lease at the apply's instant.
    let out = run(p, &["pipeline", "apply", "--project", "research", "--now", "2030-01-01T01:00:01Z"]);
    let applied = stdout(&out);
    assert!(applied.contains("applied v2"), "{applied}");
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("ExecutionRewindUnsafe") && err.contains(&format!("contextful run rewind {SUPERSEDED} --reason <text>")), "{err}");
    let owner = machine(p).owner_at(&OwnerScope::host("job:daily-build")).unwrap().expect("the owner stays pending");
    assert_eq!(owner.execution_id, SUPERSEDED);
}

/// While `export group` awaits its receiver, a serve cycle finding the publication's producer due starts it
/// not, reports it pending and leaves the producer frontier where the export read it; the export acknowledges
/// its group, and the next cycle fires the producer.
// spec: run.export.group-quiesce@86cad1d1
#[test]
fn a_cycle_defers_the_producer_while_its_publication_exports() {
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{mpsc, Arc, Mutex};
    let armed = Arc::new(AtomicBool::new(false));
    let (entered_tx, entered) = mpsc::channel::<()>();
    let (release, release_rx) = mpsc::channel::<()>();
    let (entered_tx, release_rx) = (Mutex::new(entered_tx), Mutex::new(release_rx));
    let gate = armed.clone();
    let receiver = contextful_acceptance::http::Server::start(move |_| {
        if gate.swap(false, Ordering::SeqCst) {
            entered_tx.lock().unwrap().send(()).unwrap();
            release_rx.lock().unwrap().recv_timeout(Duration::from_secs(60)).unwrap();
        }
        contextful_acceptance::http::Response::json(200, "{}")
    });
    let (dir, public, token) = published_job(&receiver);
    let p = dir.path();
    std::fs::write(p.join("more.jsonl"), "{\"day\":\"d3\"}\n").unwrap();
    stdout(&run(p, &["context", "land", "events", "--rows", "more.jsonl", "--run-id", "r2", "--site-id", "site-a", "--now", "2030-01-01T01:30:00Z"]));
    stdout(&run(p, &["pipeline", "serve", "--cycle", "--project", "research", "--now", "2030-01-01T02:00:00Z"]));
    let source = || contextful_sqlite::PublicationLedger::open(&p.join(".contextful/context/research/machine.sqlite")).unwrap().source("daily-projection").unwrap().unwrap().run;
    let read = source();
    armed.store(true, Ordering::SeqCst);
    let export = Command::new(env!("CARGO_BIN_EXE_contextful"))
        .args(["export", "group", "daily-projection", "--project", "research", "--public-key", &public, "--audience", "contextful://acme-research"])
        .current_dir(p).env("CONTEXTFUL_TOKEN", &token).env_remove("CONTEXTFUL_NODE_ID")
        .stdout(std::process::Stdio::piped()).stderr(std::process::Stdio::piped())
        .spawn().unwrap();
    entered.recv_timeout(Duration::from_secs(60)).expect("the export reaches its receiver");
    let cycle = |now: &str| -> Value {
        serde_json::from_str(&stdout(&run(p, &["pipeline", "serve", "--cycle", "--project", "research", "--now", now]))).unwrap()
    };
    let deferred = cycle("2030-01-01T03:00:00Z");
    assert_eq!(deferred["fired"], serde_json::json!([]), "{deferred}");
    assert_eq!(deferred["pending"], serde_json::json!(["job:daily-build"]), "{deferred}");
    assert_eq!(source(), read, "no producer completion lands during the export");
    release.send(()).unwrap();
    let out = export.wait_with_output().unwrap();
    assert!(stdout(&out).contains("acknowledged"), "{}", String::from_utf8_lossy(&out.stdout));
    assert_eq!(receiver.received("/events").len(), 2);
    let fired = cycle("2030-01-01T03:00:00Z");
    assert_eq!(fired["fired"], serde_json::json!(["job:daily-build"]), "{fired}");
    assert_ne!(source(), read);
}

/// While a serve cycle's unit landing a table the publication's producer reads still runs, `export group` waits
/// for it, then acknowledges the group the unit's producer step completed.
// spec: surface.dispatch.export-claim@dde058b0
#[test]
fn an_export_waits_for_a_running_unit_writing_its_inputs() {
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{mpsc, Arc, Mutex};
    let armed = Arc::new(AtomicBool::new(false));
    let (entered_tx, entered) = mpsc::channel::<()>();
    let (release, release_rx) = mpsc::channel::<()>();
    let (entered_tx, release_rx) = (Mutex::new(entered_tx), Mutex::new(release_rx));
    let gate = armed.clone();
    let feed = contextful_acceptance::http::Server::start(move |_| {
        if gate.swap(false, Ordering::SeqCst) {
            entered_tx.lock().unwrap().send(()).unwrap();
            release_rx.lock().unwrap().recv_timeout(Duration::from_secs(60)).unwrap();
        }
        contextful_acceptance::http::Response::json(200, "[{\"day\":\"d1\"}]")
    });
    let receiver = contextful_acceptance::http::Server::start(|_| contextful_acceptance::http::Response::json(200, "{}"));
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path();
    let store = p.join(".contextful/context/research");
    std::fs::create_dir_all(&store).unwrap();
    std::fs::write(store.join("config.toml"), "[node]\nid = \"ingest-a\"\n").unwrap();
    std::fs::write(p.join("contextful.toml"), format!(
        "site_id='site-a'\nauthoring_posture='per_request'\n[project]\nname='research'\n\
         [[pipeline]]\nid='feed'\nschedule='every 1h'\ntables=['events']\n[pipeline.source]\nname='http'\nconfig={{ endpoint='{}' }}\n\
         [[model]]\nid='daily'\nsql='SELECT day, CAST(count(*) AS BIGINT) AS n FROM feed_events GROUP BY day'\nunique_key=['day']\n\
         [model.contract]\nversion='1.0.0'\ncolumns=[{{ name='day', type='utf8', nullable=false }}, {{ name='n', type='int64', nullable=false }}]\n\
         [[job]]\nname='daily-build'\nkind='build'\ntarget='daily'\nafter='feed'\n\
         [[export]]\nname='daily-copy'\ntable='daily'\nendpoint='{}'\nformat='changes-v1'\nkey=['day']\nschedule='every 30s'\n\
         [[publication]]\nname='daily-projection'\njob='daily-build'\nmembers=['daily-copy']\n",
        feed.url("/feed"), receiver.url("/events"),
    )).unwrap();
    std::fs::write(p.join(".contextful/issuance.toml"), "default_audience = 'contextful://acme-research'\nmax_lifetime_secs = 3600\n").unwrap();
    stdout(&run(p, &["pipeline", "import", "--project", "research"]));
    let cycle = |now: &str| Command::new(env!("CARGO_BIN_EXE_contextful"))
        .args(["pipeline", "serve", "--cycle", "--project", "research", "--now", now])
        .current_dir(p).env_remove("CONTEXTFUL_NODE_ID")
        .stdout(std::process::Stdio::piped()).stderr(std::process::Stdio::piped())
        .spawn().unwrap();
    let first: Value = serde_json::from_str(&stdout(&cycle("2030-01-01T01:00:00Z").wait_with_output().unwrap())).unwrap();
    assert_eq!(first["fired"], serde_json::json!(["feed"]), "{first}");
    let public = stdout(&run(p, &["token", "keygen", "--out", ".contextful/issuer.seed"]));
    let token = stdout(&run(p, &["token", "mint", "--issuer-key", ".contextful/issuer.seed", "--on-behalf-of", "svc://projection", "--zone", "on-prem:hq", "--table", "daily", "--ttl", "600"]));
    export_group(p, &public, &token);
    assert_eq!(receiver.received("/events").len(), 1);
    armed.store(true, Ordering::SeqCst);
    let running = cycle("2030-01-01T02:00:00Z");
    entered.recv_timeout(Duration::from_secs(60)).expect("the cycle's unit reaches its source");
    let mut export = Command::new(env!("CARGO_BIN_EXE_contextful"))
        .args(["export", "group", "daily-projection", "--project", "research", "--public-key", &public, "--audience", "contextful://acme-research"])
        .current_dir(p).env("CONTEXTFUL_TOKEN", &token).env_remove("CONTEXTFUL_NODE_ID")
        .stdout(std::process::Stdio::piped()).stderr(std::process::Stdio::piped())
        .spawn().unwrap();
    std::thread::sleep(Duration::from_secs(2));
    assert!(export.try_wait().unwrap().is_none(), "the export waits while the unit runs");
    release.send(()).unwrap();
    let second: Value = serde_json::from_str(&stdout(&running.wait_with_output().unwrap())).unwrap();
    assert_eq!(second["fired"], serde_json::json!(["feed"]), "{second}");
    let out = export.wait_with_output().unwrap();
    assert!(stdout(&out).contains("acknowledged"), "{}", String::from_utf8_lossy(&out.stdout));
    assert_eq!(receiver.received("/events").len(), 2);
}

/// A serve cycle unable to take the publication's inputs claim starts the producer not: it reports it pending
/// and leaves the producer frontier unchanged; once the claim opens, the next cycle fires it.
// spec: surface.dispatch.export-claim-unread@f7389deb
#[test]
fn a_cycle_defers_a_unit_whose_publication_claim_fails() {
    let receiver = contextful_acceptance::http::Server::start(|_| contextful_acceptance::http::Response::json(200, "{}"));
    let (dir, _public, _token) = published_job(&receiver);
    let p = dir.path();
    std::fs::write(p.join("more.jsonl"), "{\"day\":\"d3\"}\n").unwrap();
    stdout(&run(p, &["context", "land", "events", "--rows", "more.jsonl", "--run-id", "r2", "--site-id", "site-a", "--now", "2030-01-01T01:30:00Z"]));
    let source = || contextful_sqlite::PublicationLedger::open(&p.join(".contextful/context/research/machine.sqlite")).unwrap().source("daily-projection").unwrap().unwrap().run;
    let read = source();
    // A directory where the inputs lock file belongs: the claim cannot open it.
    let lock = p.join(".contextful/exports/research/publication-daily-projection.inputs.lock");
    let _ = std::fs::remove_file(&lock);
    std::fs::create_dir_all(&lock).unwrap();
    let cycle = |now: &str| -> Value {
        serde_json::from_str(&stdout(&run(p, &["pipeline", "serve", "--cycle", "--project", "research", "--now", now]))).unwrap()
    };
    let deferred = cycle("2030-01-01T03:00:00Z");
    assert_eq!(deferred["fired"], serde_json::json!([]), "{deferred}");
    assert_eq!(deferred["pending"], serde_json::json!(["job:daily-build"]), "{deferred}");
    assert_eq!(source(), read, "no producer completion lands without its claim");
    std::fs::remove_dir(&lock).unwrap();
    let fired = cycle("2030-01-01T03:00:00Z");
    assert_eq!(fired["fired"], serde_json::json!(["job:daily-build"]), "{fired}");
    assert_ne!(source(), read);
}
