//! `contextful memory` through the built binary, against a loopback inference endpoint.

use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::path::Path;
use std::process::{Command, Output, Stdio};
use std::sync::{Arc, Mutex};

const AUD: &str = "contextful://acme-research";

/// An OpenAI-compatible endpoint answering each completion with the next scripted content.
struct Endpoint {
    port: u16,
    bodies: Arc<Mutex<Vec<String>>>,
}

impl Endpoint {
    fn start(contents: Vec<String>) -> Endpoint {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let bodies: Arc<Mutex<Vec<String>>> = Arc::default();
        let seen = bodies.clone();
        let answers = Arc::new(Mutex::new(contents.into_iter().rev().collect::<Vec<_>>()));
        std::thread::spawn(move || {
            for mut stream in listener.incoming().flatten() {
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                let mut line = String::new();
                reader.read_line(&mut line).unwrap();
                let mut len = 0;
                loop {
                    let mut h = String::new();
                    if reader.read_line(&mut h).unwrap() == 0 || h.trim().is_empty() {
                        break;
                    }
                    if let Some((k, v)) = h.split_once(':') {
                        if k.eq_ignore_ascii_case("content-length") {
                            len = v.trim().parse().unwrap();
                        }
                    }
                }
                let mut body = vec![0; len];
                reader.read_exact(&mut body).unwrap();
                seen.lock().unwrap().push(String::from_utf8_lossy(&body).into_owned());
                let content = answers.lock().unwrap().pop().unwrap_or_default();
                let reply = json!({ "choices": [{ "message": { "role": "assistant", "content": content } }] }).to_string();
                let _ = write!(stream, "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{reply}", reply.len());
            }
        });
        Endpoint { port, bodies }
    }

    fn url(&self) -> String {
        format!("http://127.0.0.1:{}/v1", self.port)
    }
}

fn run(dir: &Path, args: &[&str], env: &[(&str, &str)]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_contextful")).args(args).current_dir(dir).env_remove("CONTEXTFUL_TOKEN").envs(env.iter().copied()).output().unwrap()
}

fn stdout(out: &Output) -> String {
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

fn project() -> (tempfile::TempDir, String, String) {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path();
    std::fs::create_dir_all(p.join(".contextful")).unwrap();
    std::fs::write(p.join(".contextful/issuance.toml"), format!("default_audience = \"{AUD}\"\nmax_lifetime_secs = 3600\n")).unwrap();
    std::fs::write(
        p.join("contextful.toml"),
        "authoring_posture = \"per_request\"\n[[pipeline.tables]]\nname = \"research/notes\"\n\n[[table]]\nname = \"memory/facts\"\nshape = \"memory_facts\"\ncolumns = [\"claim_id\", \"subject\", \"predicate\", \"object\", \"scope\", \"tier\", \"confidence\", \"valid_from\", \"valid_to\", \"evidence\", \"superseded_by\", \"grant_id\", \"agent\"]\n",
    )
    .unwrap();
    std::fs::write(p.join("n1.jsonl"), "{\"note_id\":\"n1\",\"text\":\"Dana is Acme's CFO.\"}\n").unwrap();
    stdout(&run(p, &["context", "land", "research/notes", "--project", "research", "--rows", "n1.jsonl", "--run-id", "run-0001", "--site-id", "site-a"], &[]));
    let public = stdout(&run(p, &["token", "keygen", "--out", ".contextful/issuer.seed"], &[]));
    let token = stdout(&run(
        p,
        &[
            "token", "mint", "--issuer-key", ".contextful/issuer.seed", "--on-behalf-of", "user://dana@acme.example", "--agent", "agent://synthesizer", "--zone", "on-prem:hq",
            "--action", "read", "--action", "write", "--table", "research/*", "--table", "memory/*", "--ttl", "600",
        ],
        &[],
    ));
    (dir, public, token)
}

fn claim(object: &str) -> String {
    json!({ "claims": [{ "subject": "acme", "predicate": "cfo", "object": object, "confidence": 0.9,
        "evidence": [{ "table": "research/notes", "run": "run-0001", "seq": 0 }] }] })
    .to_string()
}

#[test]
fn synthesize_extracts_through_the_endpoint_and_lands_claims() {
    let (dir, public, token) = project();
    let endpoint = Endpoint::start(vec!["not json".into(), claim("Dana")]);
    let url = endpoint.url();
    let args = [
        "memory", "synthesize", "--project", "research", "--source", "research/notes", "--into", "memory/facts", "--endpoint", &url,
        "--model", "fixture", "--public-key", &public, "--audience", AUD,
    ];
    let report = stdout(&run(dir.path(), &args, &[("CONTEXTFUL_TOKEN", &token), ("CONTEXTFUL_INFERENCE_KEY", "")]));
    assert!(report.contains("1 landed"), "{report}");
    let bodies = endpoint.bodies.lock().unwrap().clone();
    assert_eq!(bodies.len(), 2);
    let second: Value = serde_json::from_str(&bodies[1]).unwrap();
    assert!(second["messages"].to_string().contains("did not validate"));
    let again = stdout(&run(dir.path(), &args, &[("CONTEXTFUL_TOKEN", &token)]));
    assert!(again.contains("no new runs"), "{again}");
}

fn one(object: &str) -> String {
    json!({ "subject": "acme", "predicate": "cfo", "object": object, "confidence": 1.0,
        "evidence": [{ "table": "research/notes", "run": "run-0001", "seq": 0 }] })
    .to_string()
}

#[test]
fn a_direct_write_lands_a_claim_and_refuses_another_shape() {
    let (dir, public, token) = project();
    let written = stdout(&run(
        dir.path(),
        &["memory", "write", "--project", "research", "--into", "memory/facts", "--claim", &one("Dana"), "--public-key", &public, "--audience", AUD],
        &[("CONTEXTFUL_TOKEN", &token)],
    ));
    assert!(written.contains("curated"), "{written}");
    let missing = run(
        dir.path(),
        &["memory", "write", "--project", "research", "--into", "memory/other", "--claim", &one("Dana"), "--public-key", &public, "--audience", AUD],
        &[("CONTEXTFUL_TOKEN", &token)],
    );
    assert!(!missing.status.success());
    assert!(String::from_utf8_lossy(&missing.stderr).contains("memory/other"));
}

// spec: read.recall.cli-verb@4e5cfc78
#[test]
fn recall_cli_answers_historical_and_current_claims_as_json() {
    let (dir, public, token) = project();
    for (object, observed) in [("Dana", "2025-01-01T00:00:00Z"), ("Lee", "2026-01-01T00:00:00Z")] {
        stdout(&run(
            dir.path(),
            &["memory", "write", "--project", "research", "--into", "memory/facts", "--claim", &one(object), "--observed-at", observed, "--public-key", &public, "--audience", AUD],
            &[("CONTEXTFUL_TOKEN", &token)],
        ));
    }
    let recall = |observed: Option<&str>| {
        let mut args = vec!["memory", "recall", "--project", "research", "--table", "memory/facts", "--subject", "acme", "--public-key", &public, "--audience", AUD];
        if let Some(at) = observed {
            args.extend(["--observed-at", at]);
        }
        serde_json::from_str::<Value>(&stdout(&run(dir.path(), &args, &[("CONTEXTFUL_TOKEN", &token)]))).unwrap()
    };
    let historical = recall(Some("2025-06-01T00:00:00Z"));
    let current = recall(None);
    let object_at = historical["columns"].as_array().unwrap().iter().position(|c| c == "object").unwrap();
    assert_eq!(historical["rows"][0][object_at], json!("Dana"));
    assert_eq!(current["rows"][0][object_at], json!("Lee"));
    assert_eq!(historical["rows"].as_array().unwrap().len(), 1);
    assert_eq!(current["rows"].as_array().unwrap().len(), 1);

    let mut child = Command::new(env!("CARGO_BIN_EXE_contextful"))
        .args(["mcp", "--project", "research", "--public-key", &public, "--audience", AUD])
        .current_dir(dir.path())
        .env("CONTEXTFUL_TOKEN", &token)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let call = json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/call", "params": {
        "name": "memory.recall", "arguments": { "table": "memory/facts", "subject": "acme", "observed_at": "2025-06-01T00:00:00Z" }
    } });
    writeln!(child.stdin.take().unwrap(), "{call}").unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    let tool: Value = serde_json::from_slice(output.stdout.split(|b| *b == b'\n').find(|line| !line.is_empty()).unwrap()).unwrap();
    assert_eq!(historical, tool["result"]["structuredContent"]);

    let memory_only = stdout(&run(
        dir.path(),
        &["token", "mint", "--issuer-key", ".contextful/issuer.seed", "--on-behalf-of", "user://dana@acme.example", "--agent", "agent://reader", "--zone", "on-prem:hq", "--action", "read", "--table", "memory/*", "--ttl", "600"],
        &[],
    ));
    let withheld: Value = serde_json::from_str(&stdout(&run(
        dir.path(),
        &["memory", "recall", "--project", "research", "--table", "memory/facts", "--subject", "acme", "--observed-at", "2025-06-01T00:00:00Z", "--public-key", &public, "--audience", AUD],
        &[("CONTEXTFUL_TOKEN", &memory_only)],
    ))).unwrap();
    assert!(withheld["rows"].as_array().unwrap().is_empty());
    assert_eq!(withheld["contextful.recall"]["suppressed"]["MemoryEvidenceUnresolved"], json!(1));
}
