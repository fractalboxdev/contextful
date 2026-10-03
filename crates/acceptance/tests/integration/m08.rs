//! Milestone 8 — accountability.
//!
//! Reach: an operator answers what a named person could have seen over a past window,
//! from the store, in SQL.

use contextful_acceptance::stdio::Session;
use contextful_acceptance::{bin, GitRepo};
use serde_json::{json, Value};
use std::process::Output;

const AUD: &str = "contextful://acme-research";

fn ok(out: &Output) -> String {
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

#[test]
fn m08_accountability() {
    let cf = bin("contextful");
    let p = GitRepo::init();
    p.write(".contextful/issuance.toml", &format!("default_audience = \"{AUD}\"\nmax_lifetime_secs = 3600\n"));
    p.write("contextful.toml", "authoring_posture = \"per_request\"\n[[pipeline.tables]]\nname = \"research/notes\"\n\n[[pipeline.tables]]\nname = \"hr/salaries\"\n");
    p.write("notes.jsonl", &json!({ "note_id": "n1", "title": "Solar battery storage" }).to_string());
    p.write("salaries.jsonl", &json!({ "employee": "e1", "salary": 1 }).to_string());
    for (table, rows) in [("research/notes", "notes.jsonl"), ("hr/salaries", "salaries.jsonl")] {
        ok(&p.run(&cf, &["context", "land", table, "--project", "research", "--rows", rows, "--run-id", "run-0001", "--site-id", "site-a"]));
    }

    // Ada's agent reads the notes; the salaries lie outside her authority.
    let public = ok(&p.run(&cf, &["token", "keygen", "--out", ".contextful/issuer.seed"]));
    let token = ok(&p.run(
        &cf,
        &[
            "token", "mint", "--issuer-key", ".contextful/issuer.seed", "--on-behalf-of", "user://ada@acme.example",
            "--agent", "agent://research-loop", "--zone", "on-prem:hq", "--action", "read", "--table", "research/*", "--ttl", "3600",
        ],
    ));
    let mut s = Session::spawn(&cf, &["mcp", "--project", "research", "--public-key", &public, "--audience", AUD], &p.root, &[("CONTEXTFUL_TOKEN", &token)]);
    let init: Value = serde_json::from_str(&s.exchange(&json!({ "jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {} }).to_string())).unwrap();
    assert!(init["result"].is_object(), "{init}");
    let call = json!({ "jsonrpc": "2.0", "id": 2, "method": "tools/call", "params": { "name": "context.query", "arguments": { "sql": "SELECT note_id FROM \"research/notes\"" } } });
    let answer: Value = serde_json::from_str(&s.exchange(&call.to_string())).unwrap();
    assert_ne!(answer["result"]["isError"], json!(true), "{answer}");
    assert!(s.close().success());

    // The read left an entry; the issuer anchors the chain, which then verifies signed.
    ok(&p.run(&cf, &["audit", "anchor", "--project", "research", "--issuer-key", ".contextful/issuer.seed"]));
    ok(&p.run(&cf, &["audit", "verify", "--project", "research", "--public-key", &public]));

    // The operator's answer, in SQL over the store: what Ada could have seen in the last hour.
    let seen = ok(&p.run(
        &cf,
        &[
            "audit", "query", "--project", "research", "--sql",
            "SELECT DISTINCT table_name FROM audit_reads WHERE on_behalf_of = 'user://ada@acme.example' AND read_at >= now() - INTERVAL 1 HOUR",
        ],
    ));
    assert!(seen.contains("research/notes"), "{seen}");
    assert!(!seen.contains("hr/salaries"), "{seen}");

    // An entry rewritten after the fact breaks the chain at its index.
    let segments = p.files_containing(b"user://ada@acme.example");
    let segment = segments.iter().find(|f| f.ends_with("000001.jsonl")).unwrap_or_else(|| panic!("no audit segment in {segments:?}"));
    let text = std::fs::read_to_string(p.root.join(segment)).unwrap();
    std::fs::write(p.root.join(segment), text.replace("user://ada@acme.example", "user://eve@acme.example")).unwrap();
    let refused = p.run(&cf, &["audit", "verify", "--project", "research", "--public-key", &public]);
    assert!(!refused.status.success());
    assert!(String::from_utf8_lossy(&refused.stderr).contains("AuditChainBroken"));
}
