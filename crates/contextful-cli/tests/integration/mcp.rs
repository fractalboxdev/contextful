//! `contextful mcp` through the built binary: admission at startup, then the tool
//! protocol over standard input and output.

use serde_json::{json, Value};
use std::io::Write;
use std::path::Path;
use std::process::{Command, Output, Stdio};

const AUD: &str = "contextful://acme-research";

fn run(dir: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_contextful")).args(args).current_dir(dir).output().unwrap()
}

fn stdout(out: &Output) -> String {
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

/// A project holding one landed table, an issuer and a credential reading it.
fn project() -> (tempfile::TempDir, String, String) {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path();
    std::fs::create_dir_all(p.join(".contextful")).unwrap();
    std::fs::write(p.join(".contextful/issuance.toml"), format!("default_audience = \"{AUD}\"\nmax_lifetime_secs = 3600\n")).unwrap();
    std::fs::write(p.join("contextful.toml"), "authoring_posture = \"per_request\"\n[[pipeline.tables]]\nname = \"research/notes\"\n").unwrap();
    std::fs::write(p.join("notes.jsonl"), "{\"note_id\":\"n1\",\"title\":\"Solar battery storage\"}\n").unwrap();
    stdout(&run(p, &["context", "land", "research/notes", "--project", "research", "--rows", "notes.jsonl", "--run-id", "run-0001", "--site-id", "site-a"]));
    let public = stdout(&run(p, &["token", "keygen", "--out", ".contextful/issuer.seed"]));
    let token = stdout(&run(
        p,
        &["token", "mint", "--issuer-key", ".contextful/issuer.seed", "--on-behalf-of", "user://dana@acme.example", "--zone", "on-prem:hq", "--table", "research/*", "--ttl", "600"],
    ));
    (dir, public, token)
}

/// Run the server over `input`, one message per line.
fn serve(dir: &Path, args: &[&str], token: Option<&str>, input: &[Value]) -> Output {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_contextful"));
    cmd.args(args).current_dir(dir).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped()).env_remove("CONTEXTFUL_TOKEN");
    if let Some(t) = token {
        cmd.env("CONTEXTFUL_TOKEN", t);
    }
    let mut child = cmd.spawn().unwrap();
    {
        let mut stdin = child.stdin.take().unwrap();
        for m in input {
            let _ = writeln!(stdin, "{m}");
        }
    }
    child.wait_with_output().unwrap()
}

#[test]
fn the_server_admits_its_credential_then_answers_tool_calls() {
    let (dir, public, token) = project();
    let args = ["mcp", "--project", "research", "--public-key", &public, "--audience", AUD];
    let out = serve(
        dir.path(),
        &args,
        Some(&token),
        &[
            json!({ "jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {} }),
            json!({ "jsonrpc": "2.0", "method": "notifications/initialized" }),
            json!({ "jsonrpc": "2.0", "id": 2, "method": "tools/call", "params": { "name": "context.query", "arguments": { "sql": "SELECT note_id FROM \"research/notes\"" } } }),
        ],
    );
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let lines: Vec<Value> = String::from_utf8_lossy(&out.stdout).lines().map(|l| serde_json::from_str(l).unwrap()).collect();
    assert_eq!(lines.len(), 2, "a notification has no answer");
    assert_eq!(lines[0]["result"]["serverInfo"]["name"], json!("contextful"));
    assert_eq!(lines[1]["result"]["structuredContent"]["rows"], json!([["n1"]]));
    // With no pepper configured, the run signals the development pepper once.
    let err = String::from_utf8_lossy(&out.stderr);
    assert_eq!(err.matches("CONTEXTFUL_PEPPER").count(), 1, "{err}");
}

#[test]
fn a_server_with_no_admissible_credential_writes_no_framing() {
    let (dir, public, token) = project();
    let args = ["mcp", "--project", "research", "--public-key", &public, "--audience", AUD];
    let hello = [json!({ "jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {} })];
    let none = serve(dir.path(), &args, None, &hello);
    assert!(!none.status.success() && none.stdout.is_empty());
    assert!(String::from_utf8_lossy(&none.stderr).contains("CONTEXTFUL_TOKEN"));
    let other = ["mcp", "--project", "research", "--public-key", &public, "--audience", "contextful://other"];
    let mismatch = serve(dir.path(), &other, Some(&token), &hello);
    assert!(!mismatch.status.success() && mismatch.stdout.is_empty());
    assert!(String::from_utf8_lossy(&mismatch.stderr).contains("AudienceMismatch"));
    std::fs::remove_file(dir.path().join("contextful.toml")).unwrap();
    let unmanifested = serve(dir.path(), &args, Some(&token), &hello);
    assert!(!unmanifested.status.success() && unmanifested.stdout.is_empty());
}

/// Over the process transport a credential is mandatory, a capability token or an explicit owner flag; an unset one raises `StdioCredentialMissing` and does not resolve to the owner context.
#[test]
fn a_server_with_no_credential_raises_stdio_credential_missing() {
    let (dir, public, _token) = project();
    let hello = [json!({ "jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {} })];
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_contextful"));
    cmd.args(["mcp", "--project", "research"]).current_dir(dir.path()).env_remove("CONTEXTFUL_TOKEN");
    cmd.env("CONTEXTFUL_ISSUER_PUBKEY", &public).env("CONTEXTFUL_AUDIENCE", AUD);
    cmd.stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped());
    for token in [None, Some("  ")] {
        if let Some(t) = token {
            cmd.env("CONTEXTFUL_TOKEN", t);
        }
        let mut child = cmd.spawn().unwrap();
        writeln!(child.stdin.take().unwrap(), "{}", hello[0]).ok();
        let out = child.wait_with_output().unwrap();
        assert!(!out.status.success() && out.stdout.is_empty(), "no owner context answers");
        let err = String::from_utf8_lossy(&out.stderr);
        assert!(err.starts_with("StdioCredentialMissing") && err.contains("CONTEXTFUL_TOKEN"), "{err}");
    }
}

/// The server admits over the stdio pipe it inherited: a credential binding no key admits
/// through that pipe, and one binding a holder key refuses, since the pipe carries no
/// per-request holder proof.
#[test]
fn a_server_admits_over_its_inherited_pipe_only_a_credential_binding_no_key() {
    let (dir, public, token) = project();
    let args = ["mcp", "--project", "research", "--public-key", &public, "--audience", AUD];
    let hello = [json!({ "jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {} })];
    let unbound = serve(dir.path(), &args, Some(&token), &hello);
    assert!(unbound.status.success(), "{}", String::from_utf8_lossy(&unbound.stderr));
    let derivation = contextful_policy::attenuate::Derivation {
        confirmation: Some(contextful_policy::possession::jwk_thumbprint(&[7; 32])),
        ..contextful_policy::attenuate::Derivation::default()
    };
    let bound = contextful_policy::attenuate::attenuate(&token, &derivation).unwrap();
    let refused = serve(dir.path(), &args, Some(&bound), &hello);
    assert!(!refused.status.success() && refused.stdout.is_empty(), "a key-bound credential writes no framing");
    let err = String::from_utf8_lossy(&refused.stderr);
    assert!(err.contains("PossessionProofInvalid"), "{err}");
}

/// `contextful serve` and `contextful mcp` open the project's chain at `.contextful/audit/` unanchored before answering a message; a chain that does not open stops the process before it reads a row.
// spec: disclosure.record.read-chain@06b13285
#[test]
fn the_server_appends_to_the_projects_chain_and_stops_on_one_that_does_not_open() {
    let (dir, public, token) = project();
    let args = ["mcp", "--project", "research", "--public-key", &public, "--audience", AUD];
    let read = [json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/call", "params": { "name": "context.query", "arguments": { "sql": "SELECT note_id FROM \"research/notes\"" } } })];
    let out = serve(dir.path(), &args, Some(&token), &read);
    let answer: Value = serde_json::from_str(String::from_utf8_lossy(&out.stdout).trim()).unwrap();
    assert_eq!(answer["result"]["structuredContent"]["rows"], json!([["n1"]]), "{answer}");
    let segment = dir.path().join(".contextful/audit/segments/000001.jsonl");
    let entries: Vec<Value> = std::fs::read_to_string(&segment).unwrap().lines().map(|l| serde_json::from_str(l).unwrap()).collect();
    assert_eq!(entries.len(), 1);
    assert_eq!((entries[0]["seq"].clone(), entries[0]["attributes"]["contextful.tool"].clone()), (json!(1), json!("context.query")));
    let tip: Value = serde_json::from_str(&std::fs::read_to_string(dir.path().join(".contextful/audit/chain.tip")).unwrap()).unwrap();
    assert!(tip.get("signature").is_none_or(Value::is_null), "an unanchored chain carries an unsigned tip: {tip}");

    // A tip signed by an issuer key: the chain opens held alone, so the server does not start.
    let signed = json!({ "seq": 1, "entry_hash": entries[0]["entry_hash"], "signature": "00" });
    std::fs::write(dir.path().join(".contextful/audit/chain.tip"), signed.to_string()).unwrap();
    let out = serve(dir.path(), &args, Some(&token), &read);
    assert!(!out.status.success());
    assert!(out.stdout.is_empty(), "{}", String::from_utf8_lossy(&out.stdout));
    assert!(String::from_utf8_lossy(&out.stderr).contains("AuditLogAnchored"), "{}", String::from_utf8_lossy(&out.stderr));
    assert_eq!(std::fs::read_to_string(&segment).unwrap().lines().count(), 1, "no read ran");
}
