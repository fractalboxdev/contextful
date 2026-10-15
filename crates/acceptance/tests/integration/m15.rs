//! Milestone 15 — point-in-time evaluation.
//!
//! Reach: an earnings row landed in one commit with its period end and filing acceptance
//! answers no `available_as_of` read between the two, and answers one after acceptance.

use contextful_acceptance::stdio::Session;
use contextful_acceptance::{bin, workspace_root, GitRepo};
use serde_json::{json, Value};
use std::path::Path;
use std::process::{Command, Output};

const AUD: &str = "contextful://acme-research";

const CONFIG: &str = r#"authoring_posture = "per_request"
[[pipeline.tables]]
name = "research/earnings"
primary_key = ["filing_id"]
content_hash_column = "digest"

[pipeline.tables.valid_time]
from = "period_end"

[pipeline.tables.available_time]
column = "accepted_at"

[[pipeline.tables]]
name = "research/prices"
primary_key = ["ticker"]
"#;

fn ok(out: &Output) -> String {
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

/// Call one tool over a fresh stdio session holding `token` and return the raw answer.
fn call(cf: &Path, p: &GitRepo, public: &str, token: &str, tool: &str, arguments: Value) -> Value {
    let mut s = Session::spawn(cf, &["mcp", "--project", "research", "--public-key", public, "--audience", AUD], &p.root, &[("CONTEXTFUL_TOKEN", token)]);
    let init: Value = serde_json::from_str(&s.exchange(&json!({ "jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {} }).to_string())).unwrap();
    assert!(init["result"].is_object(), "{init}");
    let line = json!({ "jsonrpc": "2.0", "id": 2, "method": "tools/call", "params": { "name": tool, "arguments": arguments } });
    let answer: Value = serde_json::from_str(&s.exchange(&line.to_string())).unwrap();
    assert!(s.close().success());
    answer
}

fn rows(answer: &Value) -> &Value {
    assert_ne!(answer["result"]["isError"], json!(true), "{answer}");
    &answer["result"]["structuredContent"]["rows"]
}

#[test]
fn m15_point_in_time() {
    let cf = bin("contextful");
    let p = GitRepo::init();
    p.write(".contextful/issuance.toml", &format!("default_audience = \"{AUD}\"\nmax_lifetime_secs = 3600\n"));
    p.write("contextful.toml", CONFIG);

    // One quarter's earnings: the period closes on 31 March, and the filing is accepted on
    // 2 May. Both instants land together, in one commit, long after either.
    p.write(
        "earnings.jsonl",
        &json!({ "filing_id": "acme-2030q1", "ticker": "ACME", "eps": 1.23, "digest": "q1", "period_end": "2030-03-31T00:00:00Z", "accepted_at": "2030-05-02T16:05:00Z" }).to_string(),
    );
    ok(&p.run(&cf, &[
        "context", "land", "research/earnings", "--project", "research", "--rows", "earnings.jsonl", "--run-id", "run-0001", "--site-id", "site-a",
        "--type", "period_end=timestamp", "--type", "accepted_at=timestamp", "--now", "2030-06-01T00:00:00Z",
    ]));
    p.write("prices.jsonl", &json!({ "ticker": "ACME", "close": 41.5 }).to_string());
    ok(&p.run(&cf, &["context", "land", "research/prices", "--project", "research", "--rows", "prices.jsonl", "--run-id", "run-0002", "--site-id", "site-a", "--now", "2030-06-01T00:00:00Z"]));
    p.commit("one quarter's earnings and a price, landed after the filing");

    let public = ok(&p.run(&cf, &["token", "keygen", "--out", ".contextful/issuer.seed"]));
    let reader = ok(&p.run(&cf, &[
        "token", "mint", "--issuer-key", ".contextful/issuer.seed", "--on-behalf-of", "user://dana@acme.example", "--agent", "agent://backtest",
        "--zone", "on-prem:hq", "--action", "read", "--table", "research/*", "--ttl", "3600",
    ]));
    let read = |sql: &str, available_as_of: &str| call(&cf, &p, &public, &reader, "context.query", json!({ "sql": sql, "available_as_of": available_as_of }));
    let earnings = r#"SELECT filing_id, eps FROM "research/earnings""#;

    // Between period end and acceptance the figure was not yet public: nothing answers.
    let before = read(earnings, "2030-04-15T00:00:00Z");
    assert_eq!(rows(&before), &json!([]), "{before}");
    assert_eq!(before["result"]["structuredContent"]["contextful.bounds"]["available_as_of"], json!("2030-04-15T00:00:00.000000000Z"), "{before}");

    // After acceptance the row answers.
    let after = read(earnings, "2030-05-03T00:00:00Z");
    assert_eq!(rows(&after), &json!([["acme-2030q1", 1.23]]), "{after}");

    // A table declaring no availability time refuses the bound rather than guessing one.
    let undeclared = read(r#"SELECT ticker FROM "research/prices""#, "2030-05-03T00:00:00Z");
    assert_eq!(undeclared["result"]["isError"], json!(true), "{undeclared}");
    let text = undeclared["result"]["content"][0]["text"].as_str().unwrap_or_default();
    assert!(text.contains("StoreAvailableTimeUndeclared") && text.contains("research/prices"), "{undeclared}");
}

const GDP: &str = r#"authoring_posture = "per_request"
[[pipeline.tables]]
name = "research/gdp"
primary_key = ["series"]
content_hash_column = "digest"

[pipeline.tables.valid_time]
from = "period"

[pipeline.tables.available_time]
column = "released_at"
"#;

/// An ALFRED-style series: one quarter's real GDP released three times, each vintage
/// answering at its release date, the first under `vintage = "first"`, and alike after a fold.
#[test]
fn m15_gdp_vintages() {
    let cf = bin("contextful");
    let p = GitRepo::init();
    p.write(".contextful/issuance.toml", &format!("default_audience = \"{AUD}\"\nmax_lifetime_secs = 3600\n"));
    p.write("contextful.toml", GDP);
    let releases = [("100.0", "a", "2030-04-28T12:30:00Z"), ("101.5", "b", "2030-05-29T12:30:00Z"), ("101.9", "c", "2030-06-26T12:30:00Z")];
    let lines: Vec<String> = releases
        .iter()
        .map(|(value, digest, released)| json!({ "series": "GDPC1", "period": "2030-01-01T00:00:00Z", "value": value, "digest": digest, "released_at": released }).to_string())
        .collect();
    p.write("gdp.jsonl", &lines.join("\n"));
    ok(&p.run(&cf, &[
        "context", "land", "research/gdp", "--project", "research", "--rows", "gdp.jsonl", "--run-id", "run-0001", "--site-id", "site-a",
        "--type", "period=timestamp", "--type", "released_at=timestamp", "--now", "2030-07-01T00:00:00Z",
    ]));
    p.commit("three releases of one quarter's GDP");

    let public = ok(&p.run(&cf, &["token", "keygen", "--out", ".contextful/issuer.seed"]));
    let reader = ok(&p.run(&cf, &[
        "token", "mint", "--issuer-key", ".contextful/issuer.seed", "--on-behalf-of", "user://dana@acme.example", "--agent", "agent://backtest",
        "--zone", "on-prem:hq", "--action", "read", "--table", "research/*", "--ttl", "3600",
    ]));
    let sql = r#"SELECT value FROM "research/gdp""#;
    let answers = || -> Vec<Value> {
        let mut out: Vec<Value> = ["2030-05-01", "2030-06-01", "2030-07-01"]
            .iter()
            .map(|at| rows(&call(&cf, &p, &public, &reader, "context.query", json!({ "sql": sql, "available_as_of": at }))).clone())
            .collect();
        out.push(rows(&call(&cf, &p, &public, &reader, "context.query", json!({ "sql": sql, "vintage": "first" }))).clone());
        out
    };
    let before = answers();
    assert_eq!(before, [json!([["100.0"]]), json!([["101.5"]]), json!([["101.9"]]), json!([["100.0"]])]);

    ok(&p.run(&cf, &["context", "compact", "research/gdp", "--project", "research", "--now", "2030-07-02T00:00:00Z"]));
    assert_eq!(answers(), before, "a fold drops no vintage");
}

const NEWS: &str = r#"authoring_posture = "per_request"
[[pipeline.tables]]
name = "research/news"
primary_key = ["story_id"]
content_hash_column = "digest"

[pipeline.tables.available_time]
column = "published_at"
"#;

#[test]
fn m15_horizon() {
    let cf = bin("contextful");
    let p = GitRepo::init();
    p.write(".contextful/issuance.toml", &format!("default_audience = \"{AUD}\"\nmax_lifetime_secs = 3600\n"));
    p.write("contextful.toml", NEWS);
    // One story published before the horizon, and one planted in June 2015, after it.
    let stories = [
        json!({ "story_id": "s-2014", "digest": "a", "headline": "acme widens margin", "published_at": "2014-11-01T00:00:00Z" }),
        json!({ "story_id": "s-2015", "digest": "b", "headline": "acme widens margin again", "published_at": "2015-06-01T00:00:00Z" }),
    ];
    p.write("news.jsonl", &stories.iter().map(Value::to_string).collect::<Vec<_>>().join("\n"));
    ok(&p.run(&cf, &[
        "context", "land", "research/news", "--project", "research", "--rows", "news.jsonl", "--run-id", "run-0001", "--site-id", "site-a",
        "--type", "published_at=timestamp", "--now", "2030-06-01T00:00:00Z",
    ]));
    p.commit("two stories, one past the horizon");

    let public = ok(&p.run(&cf, &["token", "keygen", "--out", ".contextful/issuer.seed"]));
    let reader = ok(&p.run(&cf, &[
        "token", "mint", "--issuer-key", ".contextful/issuer.seed", "--on-behalf-of", "user://dana@acme.example", "--agent", "agent://backtest",
        "--zone", "on-prem:hq", "--action", "read", "--table", "research/*", "--horizon", "2015-01-01T00:00:00Z", "--ttl", "3600",
    ]));

    // No derivation extends the horizon.
    let widened = p.run(&cf, &["token", "attenuate", "--token", &reader, "--horizon", "2016-01-01T00:00:00Z"]);
    assert!(!widened.status.success());
    let err = String::from_utf8_lossy(&widened.stderr);
    assert!(err.contains("AttenuationWidens") && err.contains("horizon"), "{err}");

    // Every read tool, called with no bound, reads at the horizon and omits the planted story.
    let read = |tool: &str, arguments: Value| call(&cf, &p, &public, &reader, tool, arguments);
    let queried = read("context.query", json!({ "sql": r#"SELECT story_id FROM "research/news""# }));
    assert_eq!(rows(&queried), &json!([["s-2014"]]), "{queried}");
    assert_eq!(queried["result"]["structuredContent"]["contextful.bounds"]["available_as_of"], json!("2015-01-01T00:00:00.000000000Z"), "{queried}");
    let described = read("context.describe", json!({ "table": "research/news" }));
    assert_eq!(described["result"]["structuredContent"]["row_count"], json!("1"), "{described}");
    let retrieved = read("corpus.retrieve", json!({ "prefix": "research/news", "query": "acme margin" }));
    let ids: Vec<String> = rows(&retrieved).as_array().unwrap().iter().map(|r| r.to_string()).collect();
    assert!(!ids.is_empty() && ids.iter().all(|r| !r.contains("s-2015")), "{retrieved}");
    let listed = read("context.files", json!({}));
    let part = rows(&listed).as_array().unwrap()[0][1].clone();
    let previewed = read("context.file", json!({ "path": part }));
    assert_eq!(rows(&previewed).as_array().unwrap().len(), 1, "{previewed}");
}

const SWEEP: &str = r#"authoring_posture = "per_request"
[[pipeline.tables]]
name = "filings"
columns = { accepted_at = "timestamp" }

[pipeline.tables.available_time]
column = "accepted_at"

[[pipeline.tables]]
name = "scores"

[[job]]
name = "score-filings"
kind = "store-driven"
body = "score"
statement = "SELECT doc_id, body FROM filings ORDER BY doc_id"
as_of = "2030-02-01T00:00:00Z"
tables = ["scores"]
max_in_flight = 1
sweep = { start = "2030-01-01T00:00:00Z", end = "2030-01-12T00:00:00Z", step = "1d" }
"#;

#[test]
fn m15_sweep_reads_a_backfilled_row_from_its_availability_origin() {
    let cf = bin("contextful");
    let status = Command::new(env!("CARGO")).args(["build", "--locked", "-q", "-p", "contextful-cli", "--example", "store_driven"]).current_dir(workspace_root()).status().unwrap();
    assert!(status.success());
    let host = cf.parent().unwrap().join("examples/store_driven").with_extension(std::env::consts::EXE_EXTENSION);
    let p = GitRepo::init();
    p.write(".contextful/context/research/config.toml", "[node]\nid = 'body-a'\n");
    p.write(".contextful/issuance.toml", &format!("default_audience = \"{AUD}\"\nmax_lifetime_secs = 3600\n"));
    p.write("contextful.toml", SWEEP);

    // Three filings public since December and one planted filing accepted on 7 January, all
    // backfilled in one landing on 20 January, after every origin.
    let rows = [("f1", "2029-12-01T00:00:00Z"), ("f2", "2029-12-02T00:00:00Z"), ("f3", "2029-12-03T00:00:00Z"), ("planted", "2030-01-07T00:00:00Z")];
    let lines: Vec<String> = rows.iter().map(|(id, at)| json!({ "doc_id": id, "body": "public filing", "accepted_at": at }).to_string()).collect();
    p.write("filings.jsonl", &lines.join("\n"));
    ok(&p.run(&cf, &["context", "land", "filings", "--project", "research", "--rows", "filings.jsonl", "--run-id", "backfill", "--site-id", "site-a", "--now", "2030-01-20T00:00:00Z"]));
    let public = ok(&p.run(&cf, &["token", "keygen", "--out", ".contextful/issuer.seed"]));
    let token = ok(&p.run(&cf, &["token", "mint", "--issuer-key", ".contextful/issuer.seed", "--on-behalf-of", "svc://sweep", "--zone", "on-prem:hq", "--table", "filings", "--ttl", "600"]));

    let fired = ok(&p.run_env(&host, &["job", "fire", "score-filings", "--project", "research", "--run-id", "sweep-1", "--site-id", "site-a", "--now", "2030-02-02T00:00:00Z", "--public-key", &public, "--audience", AUD], &[("CONTEXTFUL_TOKEN", &token)]));
    let origins: Vec<(String, u64)> = fired
        .lines()
        .filter_map(|l| {
            let rest = l.strip_prefix("score-filings: origin ")?;
            let origin = rest.split_whitespace().next()?.to_string();
            let input = rest.split(" from ").nth(1)?.split_whitespace().next()?.parse().ok()?;
            Some((origin, input))
        })
        .collect();
    assert_eq!(origins.len(), 12, "{fired}");
    let first = origins.iter().find(|(_, n)| *n == 4).unwrap_or_else(|| panic!("{fired}"));
    assert_eq!(first.0, "2030-01-07T00:00:00.000000000Z", "the planted row first shows at the origin where it becomes available: {fired}");
    let before: Vec<&(String, u64)> = origins.iter().take_while(|(o, _)| o != &first.0).collect();
    assert_eq!(before.len(), 6, "{fired}");
    assert!(!before.is_empty() && before.iter().all(|(_, n)| *n == 3), "{fired}");
    assert!(fired.lines().last().unwrap().contains("sweep") && fired.contains("12 origins"), "{fired}");
}
