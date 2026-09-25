//! Milestone 5 — the read face under enforcement.
//!
//! Reach: an agent asks over MCP and receives ranked rows the caller's authority admits.

use contextful_acceptance::stdio::Session;
use contextful_acceptance::{bin, GitRepo};
use serde_json::{json, Value};
use std::process::Output;

const AUD: &str = "contextful://acme-research";
const EMAIL: &str = "dana@acme.example";

fn ok(out: &Output) -> String {
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

struct Client {
    session: Session,
    next: u64,
}

impl Client {
    fn request(&mut self, method: &str, params: Value) -> Value {
        self.next += 1;
        let line = json!({ "jsonrpc": "2.0", "id": self.next, "method": method, "params": params }).to_string();
        let answer: Value = serde_json::from_str(&self.session.exchange(&line)).unwrap();
        assert_eq!(answer["id"], json!(self.next), "{answer}");
        answer
    }

    /// Call a tool; `Ok` carries the structured result, `Err` the in-band refusal text.
    fn call(&mut self, tool: &str, arguments: Value) -> Result<Value, String> {
        let answer = self.request("tools/call", json!({ "name": tool, "arguments": arguments }));
        let result = &answer["result"];
        assert!(result.is_object(), "a refusal arrives in-band, under transport success: {answer}");
        if result["isError"] == json!(true) {
            Err(result["content"][0]["text"].as_str().unwrap_or_default().to_string())
        } else {
            Ok(result["structuredContent"].clone())
        }
    }

    fn query(&mut self, sql: &str) -> Result<Value, String> {
        self.call("context.query", json!({ "sql": sql }))
    }
}

fn column(result: &Value, name: &str) -> Vec<Value> {
    let columns = result["columns"].as_array().unwrap();
    let i = columns.iter().position(|c| c == name).unwrap_or_else(|| panic!("no column {name}: {result}"));
    result["rows"].as_array().unwrap().iter().map(|r| r[i].clone()).collect()
}

#[test]
fn m05_read_face() {
    let cf = bin("contextful");
    let p = GitRepo::init();
    p.write(".contextful/issuance.toml", &format!("default_audience = \"{AUD}\"\nmax_lifetime_secs = 3600\n"));
    p.write(
        "contextful.toml",
        r#"
[[pipeline.tables]]
name = "research/notes"
partition_by = ["tenant"]
agent_description = "Research notes, one partition per tenant."

[pipeline.tables.policy.columns]
author_email = { class = "email", strategy = "hash", combine = "truncate:5" }

[[pipeline.tables]]
name = "research/vendor"

[pipeline.tables.policy.zone]
allow = ["public-cloud:*"]

[[pipeline.tables]]
name = "hr/salaries"
"#,
    );
    p.write(
        "notes.jsonl",
        &[
            json!({"note_id": "n1", "tenant": "acme", "title": "Solar battery storage costs fall", "author_email": EMAIL}),
            json!({"note_id": "n2", "tenant": "acme", "title": "Battery storage for regional grids", "author_email": EMAIL}),
            json!({"note_id": "n3", "tenant": "acme", "title": "Quarterly hiring plan", "author_email": EMAIL}),
            json!({"note_id": "n4", "tenant": "globex", "title": "Solar battery storage at Globex", "author_email": "lee@globex.example"}),
        ]
        .map(|r| r.to_string())
        .join("\n"),
    );
    p.write("vendor.jsonl", &json!({"item_id": "v1", "title": "Solar battery storage feed"}).to_string());
    p.write("salaries.jsonl", &json!({"employee": "e1", "title": "Battery storage engineer salary"}).to_string());
    for (table, rows) in [("research/notes", "notes.jsonl"), ("research/vendor", "vendor.jsonl"), ("hr/salaries", "salaries.jsonl")] {
        ok(&p.run(&cf, &["context", "land", table, "--project", "research", "--rows", rows, "--run-id", "run-0001", "--site-id", "site-a"]));
    }

    // The caller's authority: research tables, the acme tenant of the notes, an on-premises zone.
    let public = ok(&p.run(&cf, &["token", "keygen", "--out", ".contextful/issuer.seed"]));
    let token = ok(&p.run(
        &cf,
        &[
            "token", "mint", "--issuer-key", ".contextful/issuer.seed", "--on-behalf-of", "user://dana@acme.example",
            "--agent", "agent://research-loop", "--zone", "on-prem:hq", "--action", "read", "--table", "research/*",
            "--tenant", "research/notes=acme", "--ttl", "3600",
        ],
    ));

    let session = Session::spawn(
        &cf,
        &["mcp", "--project", "research", "--public-key", &public, "--audience", AUD],
        &p.root,
        &[("CONTEXTFUL_TOKEN", &token)],
    );
    let mut client = Client { session, next: 0 };

    let init = client.request("initialize", json!({ "protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": { "name": "acceptance", "version": "0" } }));
    assert_eq!(init["result"]["serverInfo"]["name"], json!("contextful"), "{init}");
    client.session.send(&json!({ "jsonrpc": "2.0", "method": "notifications/initialized" }).to_string());

    let tools = client.request("tools/list", json!({}));
    let names: Vec<&str> = tools["result"]["tools"].as_array().unwrap().iter().filter_map(|t| t["name"].as_str()).collect();
    for tool in ["context.describe", "context.query", "context.files", "context.file", "corpus.retrieve"] {
        assert!(names.contains(&tool), "{tool} missing from {names:?}");
    }

    // Ranked rows the authority admits: the other tenant's note, the zone-excluded vendor
    // table and the ungranted table all match the question and none appears.
    let ranked = client
        .call("corpus.retrieve", json!({ "prefix": "research/", "query": "solar battery storage", "limit": 10 }))
        .unwrap();
    let rows: Vec<Value> = column(&ranked, "_row");
    let ids: Vec<&str> = rows.iter().map(|r| r["note_id"].as_str().unwrap()).collect();
    assert_eq!(ids, ["n1", "n2"], "{ranked}");
    assert!(column(&ranked, "_table").iter().all(|t| t == "research/notes"), "{ranked}");
    let scores: Vec<i64> = column(&ranked, "_score").iter().map(|s| s.as_i64().unwrap()).collect();
    assert_eq!(scores, [3, 2], "{ranked}");
    for row in &rows {
        let masked = row["author_email"].as_str().unwrap();
        assert_eq!(masked.len(), 5, "{row}");
        assert_ne!(masked, EMAIL);
    }
    assert_eq!(ranked["contextful.retrieval"]["returned"], json!(2), "{ranked}");

    // A statement reads through the same restriction.
    let notes = client.query(r#"SELECT note_id FROM "research/notes" ORDER BY note_id"#).unwrap();
    assert_eq!(column(&notes, "note_id"), [json!("n1"), json!("n2"), json!("n3")]);
    let vendor = client.query(r#"SELECT * FROM "research/vendor""#).unwrap();
    assert_eq!(vendor["rows"], json!([]), "{vendor}");

    // Outside the authority, a read is refused by name rather than answered empty.
    let refused = |r: Result<Value, String>, error: &str| {
        let text = r.expect_err(error);
        assert!(text.contains(error), "expected {error}, got {text}");
    };
    refused(client.query(r#"SELECT * FROM "hr/salaries""#), "EnforceUnknownRelation");
    refused(client.query(r#"SELECT note_id FROM "research/notes" WHERE tenant = 'globex'"#), "EnforceScopeDenied");
    refused(client.query(r#"DELETE FROM "research/notes""#), "StatementNotReadOnly");
    refused(
        client.query("SELECT * FROM read_parquet('.contextful/context/research/tables/hr/salaries/**/*.parquet')"),
        "TableFunctionRefused",
    );

    assert!(client.session.close().success());
}
