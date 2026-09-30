//! Milestone 7 — memory.
//!
//! Reach: a synthesized belief supersedes its predecessor on new evidence, and a reader
//! sees which grant produced it.

use contextful_acceptance::http::{Response, Server};
use contextful_acceptance::stdio::Session;
use contextful_acceptance::{bin, GitRepo};
use serde_json::{json, Value};
use std::process::Output;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

const AUD: &str = "contextful://acme-research";

fn ok(out: &Output) -> String {
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

/// The inference endpoint: an OpenAI-compatible chat completion whose content names the
/// CFO the newest fenced note states, citing that note. Its first answer is malformed.
fn endpoint() -> (Server, Arc<AtomicUsize>) {
    let calls = Arc::new(AtomicUsize::new(0));
    let seen = calls.clone();
    let server = Server::start(move |req| {
        let n = seen.fetch_add(1, Ordering::SeqCst);
        if n == 0 {
            return Response::json(200, &json!({ "choices": [{ "message": { "role": "assistant", "content": "the CFO is Dana" } }] }).to_string());
        }
        let body: Value = serde_json::from_slice(&req.body).unwrap();
        let prompt = body["messages"].as_array().unwrap().iter().map(|m| m["content"].as_str().unwrap_or_default()).collect::<String>();
        let (object, note) = if prompt.contains("appointed Lee") { ("Lee", "n2") } else { ("Dana", "n1") };
        let run = if note == "n2" { "run-0002" } else { "run-0001" };
        let claims = json!({ "claims": [{
            "subject": "acme", "predicate": "cfo", "object": object, "confidence": 0.9,
            "evidence": [{ "table": "research/notes", "run": run, "seq": 0 }],
        }] });
        Response::json(200, &json!({ "choices": [{ "message": { "role": "assistant", "content": claims.to_string() } }] }).to_string())
    });
    (server, calls)
}

fn column(result: &Value, name: &str) -> Vec<Value> {
    let columns = result["columns"].as_array().unwrap();
    let i = columns.iter().position(|c| c == name).unwrap_or_else(|| panic!("no column {name}: {result}"));
    result["rows"].as_array().unwrap().iter().map(|r| r[i].clone()).collect()
}

/// Ask one tool over a fresh stdio session holding `token`.
fn ask(cf: &std::path::Path, p: &GitRepo, public: &str, token: &str, tool: &str, arguments: Value) -> Value {
    let mut s = Session::spawn(cf, &["mcp", "--project", "research", "--public-key", public, "--audience", AUD], &p.root, &[("CONTEXTFUL_TOKEN", token)]);
    let init: Value = serde_json::from_str(&s.exchange(&json!({ "jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {} }).to_string())).unwrap();
    assert!(init["result"].is_object(), "{init}");
    let line = json!({ "jsonrpc": "2.0", "id": 2, "method": "tools/call", "params": { "name": tool, "arguments": arguments } });
    let answer: Value = serde_json::from_str(&s.exchange(&line.to_string())).unwrap();
    assert!(s.close().success());
    assert_ne!(answer["result"]["isError"], json!(true), "{answer}");
    answer["result"]["structuredContent"].clone()
}

#[test]
fn m07_memory() {
    let cf = bin("contextful");
    let p = GitRepo::init();
    p.write(".contextful/issuance.toml", &format!("default_audience = \"{AUD}\"\nmax_lifetime_secs = 3600\n"));
    p.write(
        "contextful.toml",
        r#"
authoring_posture = "per_request"
[[pipeline.tables]]
name = "research/notes"

[[table]]
name = "memory/facts"
shape = "memory_facts"
columns = ["claim_id", "subject", "predicate", "object", "scope", "tier", "confidence", "valid_from", "valid_to", "evidence", "superseded_by", "grant_id", "agent"]
"#,
    );
    let land = |run: &str, rows: &str| {
        ok(&p.run(&cf, &["context", "land", "research/notes", "--project", "research", "--rows", rows, "--run-id", run, "--site-id", "site-a"]))
    };
    p.write("n1.jsonl", &json!({ "note_id": "n1", "text": "Dana has served as Acme's CFO since 2027." }).to_string());
    p.write("n2.jsonl", &json!({ "note_id": "n2", "text": "Acme appointed Lee as CFO, replacing Dana." }).to_string());

    let public = ok(&p.run(&cf, &["token", "keygen", "--out", ".contextful/issuer.seed"]));
    let mint = |agent: &str, extra: &[&str]| {
        let mut args = vec!["token", "mint", "--issuer-key", ".contextful/issuer.seed", "--on-behalf-of", "user://dana@acme.example", "--agent", agent, "--zone", "on-prem:hq", "--ttl", "3600"];
        args.extend_from_slice(extra);
        ok(&p.run(&cf, &args))
    };
    let writer = mint("agent://synthesizer", &["--action", "read", "--action", "write", "--table", "research/*", "--table", "memory/*"]);
    let introspected: Value = serde_json::from_str(&ok(&p.run(&cf, &["token", "introspect", "--token", &writer]))).unwrap();
    let grant_id = introspected["rev_id"].as_str().unwrap().to_string();

    let (server, calls) = endpoint();
    let synthesize = || {
        p.run_env(
            &cf,
            &[
                "memory", "synthesize", "--project", "research", "--source", "research/notes", "--into", "memory/facts",
                "--endpoint", &server.url("/v1"), "--model", "fixture", "--public-key", &public, "--audience", AUD,
            ],
            &[("CONTEXTFUL_TOKEN", &writer)],
        )
    };

    // The first belief: a malformed answer is re-prompted, then the claim lands.
    land("run-0001", "n1.jsonl");
    ok(&synthesize());
    assert_eq!(calls.load(Ordering::SeqCst), 2, "one malformed answer, one re-prompt");
    let reprompt: Value = serde_json::from_slice(&server.received("/v1/chat/completions")[1].body).unwrap();
    assert!(reprompt.to_string().contains("did not validate"), "the re-prompt carries the validation feedback");

    // New evidence: the second belief supersedes the first on the same subject and predicate.
    land("run-0002", "n2.jsonl");
    ok(&synthesize());

    // A reader granted the evidence sees the live belief and the grant that produced it.
    let reader = mint("agent://research-loop", &["--action", "read", "--table", "research/*", "--table", "memory/*"]);
    let recalled = ask(&cf, &p, &public, &reader, "corpus.retrieve", json!({ "prefix": "memory/", "query": "acme cfo" }));
    let rows = column(&recalled, "_row");
    assert_eq!(rows.len(), 1, "{recalled}");
    assert_eq!(rows[0]["object"], json!("Lee"));
    assert_eq!(rows[0]["grant_id"], json!(grant_id));
    assert_eq!(rows[0]["agent"], json!("agent://synthesizer"));
    assert_eq!(rows[0]["tier"], json!("derived"));

    // The predecessor stays in the table, retired by its successor.
    let history = ask(&cf, &p, &public, &reader, "context.query", json!({ "sql": "SELECT object, superseded_by, valid_to FROM \"memory/facts\" ORDER BY object" }));
    let objects = column(&history, "object");
    assert_eq!(objects, [json!("Dana"), json!("Lee")]);
    assert!(column(&history, "superseded_by")[0].is_string() && column(&history, "valid_to")[0].is_string(), "{history}");
    assert!(column(&history, "superseded_by")[1].is_null());

    // A reader who cannot read the evidence is told nothing the evidence holds.
    let outsider = mint("agent://outsider", &["--action", "read", "--table", "memory/*"]);
    let withheld = ask(&cf, &p, &public, &outsider, "corpus.retrieve", json!({ "prefix": "memory/", "query": "acme cfo" }));
    assert_eq!(withheld["rows"], json!([]), "{withheld}");
    assert_eq!(withheld["contextful.recall"]["suppressed"]["MemoryEvidenceUnresolved"], json!(1), "{withheld}");
}

/// A principal writes observed claims with dedup keys, and a reader recalls one subject's
/// claim at an observed instant over MCP, under an ingest bound.
#[test]
fn m07_keyed_recall() {
    let cf = bin("contextful");
    let p = GitRepo::init();
    p.write(".contextful/issuance.toml", &format!("default_audience = \"{AUD}\"\nmax_lifetime_secs = 3600\n"));
    p.write(
        "contextful.toml",
        r#"
authoring_posture = "per_request"
[[pipeline.tables]]
name = "research/notes"

[[table]]
name = "memory/facts"
shape = "memory_facts"
columns = ["claim_id", "subject", "predicate", "object", "scope", "tier", "confidence", "valid_from", "valid_to", "evidence", "superseded_by", "grant_id", "agent"]
"#,
    );
    p.write("n1.jsonl", &json!({ "note_id": "n1", "text": "Acme's CFO changed hands in March." }).to_string());
    ok(&p.run(&cf, &["context", "land", "research/notes", "--project", "research", "--rows", "n1.jsonl", "--run-id", "run-0001", "--site-id", "site-a"]));

    let public = ok(&p.run(&cf, &["token", "keygen", "--out", ".contextful/issuer.seed"]));
    let mint = |agent: &str, actions: &[&str]| {
        let mut args = vec!["token", "mint", "--issuer-key", ".contextful/issuer.seed", "--on-behalf-of", "user://dana@acme.example", "--agent", agent, "--zone", "on-prem:hq", "--ttl", "3600"];
        for a in actions {
            args.extend_from_slice(&["--action", a]);
        }
        args.extend_from_slice(&["--table", "research/*", "--table", "memory/*"]);
        ok(&p.run(&cf, &args))
    };
    let writer = mint("agent://curator", &["read", "write"]);
    let write = |object: &str, observed: &str, key: &str| {
        let claim = json!({ "subject": "acme", "predicate": "cfo", "object": object, "confidence": 1.0,
            "evidence": [{ "table": "research/notes", "run": "run-0001", "seq": 0 }] });
        p.run_env(
            &cf,
            &[
                "memory", "write", "--project", "research", "--into", "memory/facts", "--claim", &claim.to_string(),
                "--observed-at", observed, "--dedup-key", key, "--public-key", &public, "--audience", AUD,
            ],
            &[("CONTEXTFUL_TOKEN", &writer)],
        )
    };
    assert!(ok(&write("Dana", "2025-01-01T00:00:00Z", "evt-1")).contains("landed"));
    assert!(ok(&write("Lee", "2025-03-01T00:00:00Z", "evt-2")).contains("retired 1"));
    // The same observation retried lands nothing.
    assert!(ok(&write("Dana", "2025-01-01T00:00:00Z", "evt-1")).contains("nothing landed"));
    // An observation before Lee's contradicts a later fact and refuses.
    let backfill = write("Kim", "2025-02-01T00:00:00Z", "evt-4");
    assert!(!backfill.status.success());
    assert!(String::from_utf8_lossy(&backfill.stderr).contains("MemoryObservationOutOfOrder"), "{backfill:?}");

    let reader = mint("agent://research-loop", &["read"]);
    let recall = |arguments: Value| column(&ask(&cf, &p, &public, &reader, "memory.recall", arguments), "object");
    assert_eq!(recall(json!({ "table": "memory/facts", "subject": "acme", "observed_at": "2025-02-01T00:00:00Z" })), [json!("Dana")]);
    assert_eq!(recall(json!({ "table": "memory/facts", "subject": "acme", "observed_at": "2025-04-01T00:00:00Z" })), [json!("Lee")]);
    assert_eq!(recall(json!({ "table": "memory/facts", "subject": "acme" })), [json!("Lee")]);
    assert!(recall(json!({ "table": "memory/facts", "subject": "acme", "as_of_ingest": "2000-01-01T00:00:00Z" })).is_empty());
}
