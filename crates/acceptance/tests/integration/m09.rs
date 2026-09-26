//! Milestone 9 — visibility.
//!
//! Reach: a revoked grant at the source stops answering within a declared bound.

use contextful_acceptance::stdio::Session;
use contextful_acceptance::{bin, GitRepo};
use serde_json::{json, Value};
use std::process::Output;

const AUD: &str = "contextful://acme-wiki";

fn ok(out: &Output) -> String {
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

fn query(session: &mut Session, id: u64, sql: &str) -> Value {
    let line = json!({ "jsonrpc": "2.0", "id": id, "method": "tools/call", "params": { "name": "context.query", "arguments": { "sql": sql } } });
    serde_json::from_str(&session.exchange(&line.to_string())).unwrap()
}

fn page_ids(answer: &Value) -> Vec<String> {
    let result = &answer["result"]["structuredContent"];
    result["rows"].as_array().unwrap_or_else(|| panic!("{answer}")).iter().map(|r| r[0].as_str().unwrap().to_string()).collect()
}

#[test]
#[ignore = "milestone 9 is open: no sweep lands source permissions and no read joins them"]
fn m09_visibility() {
    let cf = bin("contextful");
    let p = GitRepo::init();
    p.write(".contextful/issuance.toml", &format!("default_audience = \"{AUD}\"\nmax_lifetime_secs = 3600\n"));

    // A malformed budget refuses the table by name before anything lands.
    p.write(
        "contextful.toml",
        "[[pipeline.tables]]\nname = \"wiki/pages\"\n\n[pipeline.tables.visibility]\nsource = \"wiki\"\nresource_key = \"page_id\"\nfidelity = \"mirrored\"\nfamily = \"item-exception\"\nmax_acl_staleness = \"1h30m\"\n",
    );
    let refused = p.run(&cf, &["pipeline", "validate"]);
    assert!(!refused.status.success());
    assert!(String::from_utf8_lossy(&refused.stderr).contains("VisibilityBudgetMalformed"));

    p.write(
        "contextful.toml",
        r#"
[[pipeline.tables]]
name = "wiki/pages"

[pipeline.tables.visibility]
source            = "wiki"
resource_key      = "page_id"
resource_kind     = "page"
fidelity          = "mirrored"
family            = "item-exception"
max_acl_staleness = "15m"

[[acl_sweep]]
source      = "wiki"
enumerate   = "pages"
full        = { every = "1h" }
incremental = { every = "5m" }
"#,
    );
    p.write("pages.jsonl", &[json!({"page_id": "p1", "title": "Roadmap"}), json!({"page_id": "p2", "title": "Payroll"})].map(|r| r.to_string()).join("\n"));
    ok(&p.run(&cf, &["context", "land", "wiki/pages", "--project", "wiki", "--rows", "pages.jsonl", "--run-id", "run-0001", "--site-id", "site-a"]));

    // A full sweep grants dana both pages.
    p.write("grants.jsonl", &[
        json!({"resource_id": "p1", "principal": "dana@acme.example", "principal_kind": "user", "level": "viewer"}),
        json!({"resource_id": "p2", "principal": "dana@acme.example", "principal_kind": "user", "level": "viewer"}),
    ].map(|r| r.to_string()).join("\n"));
    ok(&p.run(&cf, &["visibility", "sweep", "--project", "wiki", "--source", "wiki", "--kind", "full", "--grants", "grants.jsonl"]));

    let public = ok(&p.run(&cf, &["token", "keygen", "--out", ".contextful/issuer.seed"]));
    let token = ok(&p.run(
        &cf,
        &["token", "mint", "--issuer-key", ".contextful/issuer.seed", "--on-behalf-of", "user://dana@acme.example", "--agent", "agent://wiki-loop", "--action", "read", "--table", "wiki/*", "--ttl", "3600"],
    ));
    let serve = || Session::spawn(&cf, &["mcp", "--project", "wiki", "--public-key", &public, "--audience", AUD], &p.root, &[("CONTEXTFUL_TOKEN", &token)]);
    let sql = r#"SELECT page_id FROM "wiki/pages" ORDER BY page_id"#;

    let mut session = serve();
    assert_eq!(page_ids(&query(&mut session, 1, sql)), ["p1", "p2"]);
    assert!(session.close().success());

    // The source revokes p2; the next incremental sweep inside the budget withdraws it.
    p.write("grants.jsonl", &json!({"resource_id": "p1", "principal": "dana@acme.example", "principal_kind": "user", "level": "viewer"}).to_string());
    ok(&p.run(&cf, &["visibility", "sweep", "--project", "wiki", "--source", "wiki", "--kind", "incremental", "--grants", "grants.jsonl"]));
    let mut session = serve();
    assert_eq!(page_ids(&query(&mut session, 1, sql)), ["p1"]);
    assert!(session.close().success());

    // Past the budget with no sweep, the read is refused rather than answered from stale state.
    let mut session = Session::spawn(
        &cf,
        &["mcp", "--project", "wiki", "--public-key", &public, "--audience", AUD],
        &p.root,
        &[("CONTEXTFUL_TOKEN", &token), ("CONTEXTFUL_NOW_OFFSET_SECS", "3600")],
    );
    let stale = query(&mut session, 1, sql);
    assert_eq!(stale["result"]["isError"], json!(true), "{stale}");
    assert!(stale["result"]["content"][0]["text"].as_str().unwrap().contains("VisibilityAccessStale"), "{stale}");
    assert!(session.close().success());
}
