//! Built embedding coverage for canonical preparation of paid body-effect results.

use contextful_acceptance::{bin, workspace_root, GitRepo};
use serde_json::{json, Value};
use std::process::{Command, Output};

const CANARY: &str = "generated-model-result-canary-98";
const AUD: &str = "contextful://acme-research";

fn ok(out: &Output) -> String {
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8_lossy(&out.stdout).trim().to_owned()
}

#[test]
fn protected_paid_body_results_record_prepared_inline_and_blob_values_and_resume_once() {
    let cf = bin("contextful");
    let status = Command::new(env!("CARGO")).args(["build", "--locked", "-q", "-p", "contextful-cli", "--example", "store_driven"]).current_dir(workspace_root()).status().unwrap();
    assert!(status.success());
    let host = cf.parent().unwrap().join("examples/store_driven").with_extension(std::env::consts::EXE_EXTENSION);
    for padding in [0, 1024 * 1024 + 1] {
        let p = GitRepo::init();
        p.write(".contextful/context/research/config.toml", "[node]\nid = 'body-a'\n");
        p.write(".contextful/issuance.toml", &format!("default_audience = '{AUD}'\nmax_lifetime_secs = 3600\n"));
        p.write("contextful.toml", r#"
authoring_posture = "per_request"
[[pipeline.tables]]
name = "scores"
redaction = [{ table = "scores", column = "score", operation = "hash" }]
[[pipeline.tables]]
name = "reference"
redaction = [{ table = "reference", column = "score", operation = "hash" }]
[[job]]
name = "score-documents"
kind = "store-driven"
body = "score"
statement = "SELECT doc_id, body FROM documents ORDER BY doc_id"
as_of = "2030-01-01T00:00:10Z"
tables = ["scores"]
max_in_flight = 1
"#);
        p.write("input.jsonl", &(1..=3).map(|i| json!({"doc_id":format!("d{i}"),"body":"public-input"}).to_string()).collect::<Vec<_>>().join("\n"));
        ok(&p.run(&cf, &["context", "land", "documents", "--project", "research", "--rows", "input.jsonl", "--run-id", "input", "--site-id", "site", "--now", "2030-01-01T00:00:00Z"]));
        let public = ok(&p.run(&cf, &["token", "keygen", "--out", ".contextful/issuer.seed"]));
        let token = ok(&p.run(&cf, &["token", "mint", "--issuer-key", ".contextful/issuer.seed", "--on-behalf-of", "svc://body-test", "--zone", "on-prem:hq", "--table", "documents", "--ttl", "600"]));
        let padding = padding.to_string();
        let fire = |run: &str, now: &str, die: &str| p.run_env(&host, &["job", "fire", "score-documents", "--project", "research", "--run-id", run, "--site-id", "site", "--now", now, "--public-key", &public, "--audience", AUD], &[("CONTEXTFUL_TOKEN", &token), ("SCORE_RECORDED", "1"), ("SCORE_CANARY", CANARY), ("SCORE_PADDING_BYTES", &padding), ("SCORE_LEDGER", "paid.txt"), ("SCORE_DIE_AFTER", die)]);
        let interrupted = fire("body-first", "2030-01-01T00:01:00Z", "2");
        assert_eq!(interrupted.status.code(), Some(9), "{}", String::from_utf8_lossy(&interrupted.stderr));
        assert_eq!(std::fs::read_to_string(p.root.join("paid.txt")).unwrap().lines().count(), 2);
        assert!(p.files_containing(CANARY.as_bytes()).is_empty(), "a retained effect record contains its removed result value");
        ok(&fire("body-resumed", "2030-01-01T00:02:00Z", "99"));
        assert_eq!(std::fs::read_to_string(p.root.join("paid.txt")).unwrap().lines().count(), 3, "recorded effects replay without another paid call");
        assert!(p.files_containing(CANARY.as_bytes()).is_empty());
        // Direct canonical landing supplies the byte-value reference, including one HMAC.
        p.write("reference.jsonl", &(1..=3).map(|i| json!({"doc_id":format!("d{i}"),"score":CANARY,"padding":"p".repeat(padding.parse().unwrap())}).to_string()).collect::<Vec<_>>().join("\n"));
        ok(&p.run(&cf, &["context", "land", "reference", "--project", "research", "--rows", "reference.jsonl", "--run-id", "reference", "--site-id", "site"]));
        let query = |table: &str| -> Value { serde_json::from_str(&ok(&p.run(&cf, &["query", "--json", "--project", "research", &format!("SELECT doc_id, score, padding FROM {table} ORDER BY doc_id")]))).unwrap() };
        assert_eq!(query("scores")["rows"], query("reference")["rows"], "prepared replay preserves direct removal and does not HMAC twice");
    }
}
