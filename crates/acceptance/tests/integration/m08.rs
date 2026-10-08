//! Milestone 8 — accountability.
//!
//! Reach: an operator answers what a named person could have seen over a past window,
//! from the store, in SQL.

use contextful_acceptance::stdio::Session;
use contextful_acceptance::{bin, GitRepo};
use serde_json::{json, Value};
use std::process::Output;

const AUD: &str = "contextful://acme-research";

#[test]
fn column_keyset_erasure_collects_declared_memory_rows() {
    let cf = bin("contextful"); let p = GitRepo::init();
    let canary = "memory-column-erasure-canary-793d";
    ok(&p.run(&cf, &["init", "research", "--authoring-posture", "per_request"]));
    p.write(".contextful/issuance.toml", &format!("default_audience = \"{AUD}\"\nmax_lifetime_secs = 3600\n"));
    p.write("contextful.toml", r#"authoring_posture = "per_request"
[[pipeline.tables]]
name = "notes"
primary_key = ["id"]
columns = {subject="utf8"}
[[table]]
name = "memory/facts"
shape = "memory_facts"
columns = ["claim_id","subject","predicate","object","scope","tier","confidence","valid_from","valid_to","evidence","superseded_by","grant_id","agent"]
"#);
    p.write("notes.jsonl", &format!("{}\n{}\n", json!({"id":"erased","subject":canary}), json!({"id":"kept","subject":"survivor"})));
    ok(&p.run(&cf, &["context", "land", "notes", "--rows", "notes.jsonl", "--run-id", "r1", "--site-id", "fixture"]));
    let public = ok(&p.run(&cf, &["token", "keygen", "--out", ".contextful/issuer.seed"]));
    let token = ok(&p.run(&cf, &["token", "mint", "--issuer-key", ".contextful/issuer.seed", "--on-behalf-of", "user://fixture", "--agent", "agent://fixture", "--zone", "on-prem:fixture", "--action", "read", "--action", "write", "--action", "forget", "--table", "*", "--ttl", "3600"]));
    for subject in [canary, "survivor"] {
        let claim = json!({"subject":subject,"predicate":"fixture","object":subject,"confidence":1.0,"evidence":[{"table":"notes","run":"r1","seq":0}]}).to_string();
        ok(&p.run_env(&cf, &["memory", "write", "--project", "research", "--into", "memory/facts", "--claim", &claim, "--public-key", &public, "--audience", AUD], &[("CONTEXTFUL_TOKEN", &token)]));
    }
    let query = |sql: &str| -> Value { serde_json::from_str(&ok(&p.run_env(&cf, &["query", "--json", "--project", "research", sql], &[("CONTEXTFUL_ISSUER_PUBKEY", &public)]))).unwrap() };
    assert_eq!(query("SELECT subject FROM \"memory/facts\" ORDER BY subject")["rows"], json!([[canary],["survivor"]]));
    p.write("keys.json", &json!({"subject_hash":format!("hmac-sha256:{}", "a".repeat(64)),"column":"subject","keys":[canary]}).to_string());
    let receipt: Value = serde_json::from_str(&ok(&p.run_env(&cf, &["context", "erase", "--project", "research", "--key-set", "keys.json", "--issuer-key", ".contextful/issuer.seed", "--public-key", &public, "--audience", AUD, "--json"], &[("CONTEXTFUL_TOKEN", &token)]))).unwrap();
    assert_eq!(receipt["affected_counts"]["memory/facts"], json!(1), "the canonical memory column is omitted from physical collection: {receipt}");
    assert_eq!(query("SELECT subject FROM \"memory/facts\" ORDER BY subject")["rows"], json!([["survivor"]]));
    assert_eq!(query("SELECT id FROM notes ORDER BY id")["rows"], json!([["kept"]]));
    fn parts(directory: &std::path::Path, out: &mut Vec<std::path::PathBuf>) {
        for entry in std::fs::read_dir(directory).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() { parts(&path, out); }
            else if path.extension().is_some_and(|extension| extension == "parquet") { out.push(path); }
        }
    }
    let mut retained = Vec::new(); parts(&p.root.join(".contextful/context/research"), &mut retained);
    let memory = retained.into_iter().filter(|path| path.to_string_lossy().contains("tables/memory/facts/")).collect::<Vec<_>>();
    assert!(!memory.is_empty(), "no retained memory survivor witnesses exist");
    for path in memory {
        let sql = format!("SELECT subject, object FROM read_parquet('{}')", path.to_string_lossy().replace('\'', "''"));
        assert!(!query(&sql)["rows"].to_string().contains(canary), "a retained memory part holds erased values");
    }
}

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

/// The built surface exercises the erasure contract over disposable declared stores.
#[test]
fn subject_and_keyset_erasure_is_atomic_and_physically_complete() {
    let expected_root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..").canonicalize().unwrap();
    assert_eq!(contextful_acceptance::workspace_root(), expected_root, "acceptance harness source provenance differs from its owning test");
    let cf = bin("contextful");
    let p = GitRepo::init();
    let subject = "erasure-fixture-alice-39ac";
    let trace = "erasure-fixture-trace-71bc";
    ok(&p.run(&cf, &["init", "research", "--authoring-posture", "per_request"]));
    p.write(".contextful/issuance.toml", &format!("default_audience = \"{AUD}\"\nmax_lifetime_secs = 3600\n"));
    p.write("contextful.toml", r#"authoring_posture = "per_request"
[[pipeline.tables]]
name = "research/notes"
primary_key = ["id"]
subject_id = "subject"
erasure_key = "id"
retain_runs = "90d"
result_cache = "1h"
[[pipeline.tables.indexes]]
kind = "fulltext"
column = "text"
[[pipeline.tables]]
name = "research/keys"
primary_key = ["id"]
columns = { trace_id = "utf8" }
retain_runs = "90d"
[[pipeline.tables]]
name = "research/key_events"
primary_key = ["trace_id", "id"]
retain_runs = "90d"
[[pipeline.tables]]
name = "research/key_partitions"
primary_key = ["id"]
partition_by = ["trace_id"]
retain_runs = "90d"
[[pipeline.tables]]
name = "research/unrelated"
primary_key = ["id"]
[[pipeline.tables]]
name = "research/blobs"
primary_key = ["digest"]
erasure_key = "digest"
referenced_by = [{ table = "research/notes", column = "blob" }]
[[pipeline.tables]]
name = "research/citations"
primary_key = ["id"]
erasure_key = "id"
on_erase = "survive"
referenced_by = [{ table = "research/notes", column = "citation" }]
[[pipeline.tables]]
name = "research/citations_collect"
primary_key = ["id"]
erasure_key = "id"
referenced_by = [{ table = "research/notes", column = "citation" }]
"#);
    for (table, rows) in [
        ("research/notes", vec![json!({"id":"alice-shared","subject":subject,"blob":"shared","citation":"citation","text":subject}), json!({"id":"alice-only","subject":subject,"blob":"unique","citation":"citation","text":subject}), json!({"id":"bob","subject":"bob","blob":"shared","citation":"other","text":"remaining"})]),
        ("research/keys", vec![json!({"id":"key-a","trace_id":trace,"value":subject}), json!({"id":"key-b","trace_id":"live-trace","value":"remaining"})]),
        ("research/key_events", vec![json!({"id":"event-a","trace_id":trace,"value":subject}), json!({"id":"event-b","trace_id":"live-trace","value":"remaining"})]),
        ("research/key_partitions", vec![json!({"id":"partition-a","trace_id":trace,"value":subject}), json!({"id":"partition-b","trace_id":"live-trace","value":"remaining"})]),
        ("research/unrelated", vec![json!({"id":trace,"value":"remaining"})]),
        ("research/blobs", vec![json!({"digest":"shared","value":"remaining"}), json!({"digest":"unique","value":subject})]),
        ("research/citations", vec![json!({"id":"citation","source_table":"research/notes","source_key":"alice-only","source_run":"run-0001","source_seq":1})]),
        ("research/citations_collect", vec![json!({"id":"citation","source_table":"research/notes","source_key":"alice-only","source_run":"run-0001","source_seq":1})]),
    ] {
        p.write("rows.jsonl", &rows.iter().map(Value::to_string).collect::<Vec<_>>().join("\n"));
        ok(&p.run(&cf, &["context", "land", table, "--project", "research", "--rows", "rows.jsonl", "--run-id", "run-0001", "--site-id", "fixture"]));
        ok(&p.run(&cf, &["context", "compact", table, "--project", "research"]));
    }
    let public = ok(&p.run(&cf, &["token", "keygen", "--out", ".contextful/issuer.seed"]));
    let mint = |action| ok(&p.run(&cf, &["token", "mint", "--issuer-key", ".contextful/issuer.seed", "--on-behalf-of", "user://erasure-fixture", "--agent", "agent://erasure-fixture", "--zone", "on-prem:fixture", "--action", action, "--table", "research/*", "--ttl", "3600"]));
    let read = mint("read");
    let forget = mint("forget");
    let args = ["context", "erase", "--project", "research", "--subject", subject, "--tables", "research/notes", "--issuer-key", ".contextful/issuer.seed", "--public-key", &public, "--audience", AUD, "--json"];
    let refused = p.run_env(&cf, &args, &[("CONTEXTFUL_TOKEN", &read)]);
    assert!(!refused.status.success());
    assert!(String::from_utf8_lossy(&refused.stderr).contains("ErasureUngranted"), "{}", String::from_utf8_lossy(&refused.stderr));
    let receipt: Value = serde_json::from_str(&ok(&p.run_env(&cf, &args, &[("CONTEXTFUL_TOKEN", &forget)]))).unwrap();
    assert!(receipt["subject_hash"].as_str().is_some_and(|hash| !hash.contains(subject)));
    assert_eq!(receipt["physical_collection"], json!("complete"));
    // Each erased logical row occurs in its retained run and compacted snapshot.
    assert_eq!(receipt["affected_counts"]["research/citations_collect"], json!(2));
    assert!(receipt["affected_counts"].get("research/citations").is_none());

    let query = |sql: &str| -> Value { serde_json::from_str(&ok(&p.run_env(&cf, &["query", "--json", "--project", "research", sql], &[("CONTEXTFUL_ISSUER_PUBKEY", &public)]))).unwrap() };
    // One statement sees the committed frontier across every affected relation.
    let visible = query(r#"SELECT (SELECT count(*) FROM "research/notes") AS notes, (SELECT count(*) FROM "research/blobs") AS blobs, (SELECT count(*) FROM "research/citations") AS citations, (SELECT count(*) FROM "research/citations_collect") AS collected"#);
    assert_eq!(visible["rows"], json!([["1", "1", "1", "0"]]), "{visible}");
    assert_eq!(query(r#"SELECT digest FROM "research/blobs""#)["rows"], json!([["shared"]]));
    assert_eq!(query(r#"SELECT id FROM "research/citations""#)["rows"], json!([["citation"]]));
    let citation = query(r#"SELECT source_table, source_run, source_seq FROM "research/citations""#)["rows"][0].clone();
    let sequence = citation[2].as_str().map(str::to_string).unwrap_or_else(|| citation[2].to_string());
    let reference = ["context", "reference", citation[0].as_str().unwrap(), citation[1].as_str().unwrap(), &sequence,
        "--project", "research", "--public-key", &public, "--audience", AUD, "--issuer-key", ".contextful/issuer.seed", "--json"];
    let unavailable: Value = serde_json::from_str(&ok(&p.run_env(&cf, &reference, &[("CONTEXTFUL_TOKEN", &read)]))).unwrap();
    assert_eq!(unavailable["rows"], json!([[false, "erased"]]));

    p.write("erase-keys.json", &json!({"subject_hash":receipt["subject_hash"],"column":"trace_id","keys":[trace]}).to_string());
    let key_receipt: Value = serde_json::from_str(&ok(&p.run_env(&cf, &["context", "erase", "--project", "research", "--key-set", "erase-keys.json", "--issuer-key", ".contextful/issuer.seed", "--public-key", &public, "--audience", AUD, "--json"], &[("CONTEXTFUL_TOKEN", &forget)]))).unwrap();
    for table in ["research/keys", "research/key_events", "research/key_partitions"] {
        assert_eq!(key_receipt["affected_counts"][table], json!(2), "{key_receipt}");
    }
    assert_eq!(key_receipt["subject_hash"], receipt["subject_hash"]);
    assert_eq!(key_receipt["physical_collection"], json!("complete"));
    assert_eq!(query(r#"SELECT id FROM "research/keys""#)["rows"], json!([["key-b"]]));
    assert_eq!(query(r#"SELECT id FROM "research/key_events""#)["rows"], json!([["event-b"]]));
    assert_eq!(query(r#"SELECT id FROM "research/key_partitions""#)["rows"], json!([["partition-b"]]));
    assert_eq!(query(r#"SELECT id FROM "research/unrelated""#)["rows"], json!([[trace]]));
    assert_eq!(serde_json::from_str::<Value>(&ok(&p.run_env(&cf, &reference, &[("CONTEXTFUL_TOKEN", &read)]))).unwrap()["rows"], json!([[false, "erased"]]));
    let leaked: Vec<_> = p.files_containing(subject.as_bytes()).into_iter().filter(|path| path.starts_with(".contextful/")).collect();
    assert!(leaked.is_empty(), "retained store or sidecar bytes hold the erased subject: {leaked:?}");
    // Compressed Parquet needs decoding; absence from raw bytes alone proves nothing.
    let mut directories = vec![p.root.join(".contextful/context/research")];
    let affected = ["keys", "key_events", "key_partitions"];
    let mut affected_parquet = [0; 3];
    while let Some(directory) = directories.pop() {
        for entry in std::fs::read_dir(directory).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                directories.push(path);
                continue;
            }
            let components: Vec<_> = path.components().map(|component| component.as_os_str().to_string_lossy().into_owned()).collect();
            let affected_table = affected.iter().position(|table| components.windows(3).any(|names| names[0] == "tables" && names[1] == "research" && names[2] == *table));
            if affected_table.is_some() {
                assert!(!path.to_string_lossy().contains(trace), "an affected path retains the erased column key: {}", path.display());
                assert!(!std::fs::read(&path).unwrap().windows(trace.len()).any(|bytes| bytes == trace.as_bytes()), "an affected sidecar retains the erased column key: {}", path.display());
            }
            if path.extension().is_some_and(|extension| extension == "parquet") {
                let escaped = path.to_string_lossy().replace('\'', "''");
                let decoded = ok(&p.run(&cf, &["query", "--json", &format!("SELECT to_json(row) FROM read_parquet('{escaped}') AS row")]));
                assert!(!decoded.contains(subject), "retained Parquet {} contains erased data", path.display());
                if let Some(index) = affected_table {
                    affected_parquet[index] += 1;
                    assert!(!decoded.contains(trace), "affected Parquet {} contains the erased column key", path.display());
                }
            }
        }
    }
    assert!(affected_parquet.iter().all(|count| *count > 0), "every affected table needs a retained survivor Parquet witness: {affected_parquet:?}");
    ok(&p.run(&cf, &["audit", "anchor", "--project", "research", "--issuer-key", ".contextful/issuer.seed"]));
    ok(&p.run(&cf, &["audit", "verify", "--project", "research", "--public-key", &public]));
    let audit = ok(&p.run(&cf, &["audit", "query", "--project", "research", "--sql", "SELECT subject_hash FROM audit_erasures"]));
    assert!(audit.contains(receipt["subject_hash"].as_str().unwrap()), "{audit}");
    assert!(!audit.contains(subject));
}
