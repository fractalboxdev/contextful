//! Milestone 15 — point-in-time evaluation.
//!
//! Reach: an earnings row landed in one commit with its period end and filing acceptance
//! answers no `available_as_of` read between the two, and answers one after acceptance.

use contextful_acceptance::stdio::Session;
use contextful_acceptance::{bin, GitRepo};
use serde_json::{json, Value};
use std::path::Path;
use std::process::Output;

const AUD: &str = "contextful://acme-research";

const CONFIG: &str = r#"authoring_posture = "per_request"
[[pipeline.tables]]
name = "research/earnings"
primary_key = ["filing_id"]

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
#[ignore = "milestone 15 is open: no read surface admits `available_as_of`"]
fn m15_point_in_time() {
    let cf = bin("contextful");
    let p = GitRepo::init();
    p.write(".contextful/issuance.toml", &format!("default_audience = \"{AUD}\"\nmax_lifetime_secs = 3600\n"));
    p.write("contextful.toml", CONFIG);

    // One quarter's earnings: the period closes on 31 March, and the filing is accepted on
    // 2 May. Both instants land together, in one commit, long after either.
    p.write(
        "earnings.jsonl",
        &json!({ "filing_id": "acme-2030q1", "ticker": "ACME", "eps": 1.23, "period_end": "2030-03-31T00:00:00Z", "accepted_at": "2030-05-02T16:05:00Z" }).to_string(),
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
