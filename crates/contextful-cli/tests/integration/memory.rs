//! `contextful memory` through the built binary, against a loopback inference endpoint.
#![cfg(feature = "data-plane")]

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

/// A relation rename replaces the type of each stored edge under its existing key.
// spec: read.declare.reserved-relations@b16ea1e8
#[test]
fn relation_rename_rewrites_stored_edges_before_the_old_name_is_removed() {
    let (dir, public, token) = project();
    let manifest = dir.path().join("contextful.toml");
    let declaration = format!(
        "{}\n[[table]]\nname = \"memory/edges\"\nshape = \"memory_edges\"\ncolumns = [\"edge_id\", \"rel_type\", \"source_id\", \"target_id\", \"evidence\"]\n\n[relation]\ndeclared = [\"follows\", \"tracks\"]\n",
        std::fs::read_to_string(&manifest).unwrap()
    );
    std::fs::write(&manifest, &declaration).unwrap();
    std::fs::write(
        dir.path().join("edges.jsonl"),
        "{\"edge_id\":\"e1\",\"rel_type\":\"follows\",\"source_id\":\"a\",\"target_id\":\"b\",\"evidence\":\"[]\"}\n{\"edge_id\":\"e2\",\"rel_type\":\"tracks\",\"source_id\":\"a\",\"target_id\":\"c\",\"evidence\":\"[]\"}\n",
    ).unwrap();
    stdout(&run(dir.path(), &["context", "land", "memory/edges", "--project", "research", "--rows", "edges.jsonl", "--run-id", "run-edges", "--site-id", "site-a"], &[]));

    let args = ["memory", "relations", "rename", "follows", "tracks", "--project", "research", "--public-key", &public, "--audience", AUD];
    let report = stdout(&run(dir.path(), &args, &[("CONTEXTFUL_TOKEN", &token)]));
    assert!(report.contains("1") && report.contains("memory/edges"), "{report}");
    let query = ["query", "--json", "--project", "research", "SELECT edge_id, rel_type FROM \"memory/edges\" ORDER BY edge_id"];
    let rows: Value = serde_json::from_str(&stdout(&run(dir.path(), &query, &[]))).unwrap();
    assert_eq!(rows["rows"], json!([["e1", "tracks"], ["e2", "tracks"]]));
    assert!(stdout(&run(dir.path(), &args, &[("CONTEXTFUL_TOKEN", &token)])).contains("0"));

    std::fs::write(&manifest, declaration.replace("[\"follows\", \"tracks\"]", "[\"tracks\"]")).unwrap();
    let rows: Value = serde_json::from_str(&stdout(&run(dir.path(), &query, &[]))).unwrap();
    assert_eq!(rows["rows"], json!([["e1", "tracks"], ["e2", "tracks"]]));
}

// spec: read.recall.cli-verb@4aef14d8
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

    // The three verb calls append an entry each, as the tool call does.
    let sql = "SELECT agent, CAST(count(*) AS VARCHAR), CAST(count(claims) AS VARCHAR) FROM audit_reads WHERE table_name = 'memory/facts' GROUP BY agent ORDER BY agent";
    let audited: Value = serde_json::from_str(&stdout(&run(dir.path(), &["audit", "query", "--project", "research", "--sql", sql], &[]))).unwrap();
    assert_eq!(audited["rows"], json!([["agent://reader", "1", "0"], ["agent://synthesizer", "3", "3"]]), "{audited}");
    // The evidence table the gate read is named; the reader, holding no grant on it, read none.
    let sql = "SELECT agent, CAST(count(*) AS VARCHAR) FROM audit_reads WHERE table_name = 'research/notes' GROUP BY agent";
    let evidence: Value = serde_json::from_str(&stdout(&run(dir.path(), &["audit", "query", "--project", "research", "--sql", sql], &[]))).unwrap();
    assert_eq!(evidence["rows"], json!([["agent://synthesizer", "3"]]), "{evidence}");
}

// spec: read.declare.availability@961dd228
#[test]
fn shaped_claim_availability_is_enforced_by_native_recall() {
    let (dir, public, _) = project();
    let p = dir.path();
    let mut manifest = std::fs::read_to_string(p.join("contextful.toml")).unwrap();
    manifest = manifest.replace("name = \"research/notes\"", "name = \"research/notes\"\n[pipeline.tables.available_time]\ncolumn = \"published_at\"");
    manifest.push_str("\ncontent_hash_column = \"digest\"\n[table.available_time]\ncolumn = \"concluded_at\"\nzone = \"UTC\"\n");
    std::fs::write(p.join("contextful.toml"), manifest).unwrap();
    std::fs::write(p.join("historical-notes.jsonl"), "{\"note_id\":\"historical\",\"text\":\"A historical note.\",\"published_at\":\"2020-01-01T00:00:00Z\"}\n").unwrap();
    stdout(&run(p, &["context", "land", "research/notes", "--project", "research", "--rows", "historical-notes.jsonl", "--run-id", "historical-notes", "--site-id", "site-a", "--now", "2020-01-01T00:00:00Z"], &[]));
    let claim = |id: &str, object: &str, concluded: &str| json!({
        "claim_id": id, "subject": "acme", "predicate": "cfo", "object": object, "digest": id,
        "scope": null, "tier": "curated", "confidence": 1.0,
        "valid_from": "2020-01-01T00:00:00Z", "valid_to": null,
        "evidence": "[{\"table\":\"research/notes\",\"run\":\"historical-notes\",\"seq\":0}]",
        "superseded_by": null, "grant_id": "fixture", "agent": "fixture", "concluded_at": concluded,
    });
    let rows = [claim("known", "Dana", "2025-01-01T00:00:00Z"), claim("future", "Lee", "2026-01-01T00:00:00Z")];
    std::fs::write(p.join("claims.jsonl"), rows.iter().map(Value::to_string).collect::<Vec<_>>().join("\n")).unwrap();
    stdout(&run(p, &["context", "land", "memory/facts", "--project", "research", "--rows", "claims.jsonl", "--run-id", "claims-1", "--site-id", "site-a", "--type", "concluded_at=timestamp", "--now", "2025-01-02T00:00:00Z"], &[]));
    let token = stdout(&run(p, &["token", "mint", "--issuer-key", ".contextful/issuer.seed", "--on-behalf-of", "user://reader@example.invalid", "--zone", "on-prem:hq", "--table", "research/*", "--table", "memory/*", "--horizon", "2025-06-01T00:00:00Z", "--ttl", "600"], &[]));
    let recalled: Value = serde_json::from_str(&stdout(&run(p, &["memory", "recall", "--project", "research", "--table", "memory/facts", "--subject", "acme", "--public-key", &public, "--audience", AUD], &[("CONTEXTFUL_TOKEN", &token)]))).unwrap();
    let object_at = recalled["columns"].as_array().unwrap().iter().position(|c| c == "object").unwrap();
    assert_eq!(recalled["rows"].as_array().unwrap().len(), 1, "{recalled}");
    assert_eq!(recalled["rows"][0][object_at], json!("Dana"), "{recalled}");
}

/// A read verb's `--issuer-key` never anchors the chain: over an unanchored chain it refuses
/// and appends nothing, unsigned reads go on, and once `audit anchor` holds the chain the
/// same verb appends signed entries.
// spec: disclosure.record.signed-read@e4226fd4
#[test]
fn a_signed_read_over_an_unanchored_chain_refuses_and_unsigned_reads_go_on() {
    let (dir, public, token) = project();
    let p = dir.path();
    stdout(&run(p, &["memory", "write", "--project", "research", "--into", "memory/facts", "--claim", &one("Dana"), "--public-key", &public, "--audience", AUD], &[("CONTEXTFUL_TOKEN", &token)]));
    let recall = ["memory", "recall", "--project", "research", "--table", "memory/facts", "--subject", "acme", "--public-key", &public, "--audience", AUD];
    stdout(&run(p, &recall, &[("CONTEXTFUL_TOKEN", &token)]));
    let entries = || contextful_policy::audit::entries(&p.join(".contextful/audit")).unwrap().len();
    assert_eq!(entries(), 1);

    let signed = ["memory", "explain", "--project", "research", "--table", "memory/facts", "--reference", "acme", "--issuer-key", ".contextful/issuer.seed", "--public-key", &public, "--audience", AUD];
    let refused = run(p, &signed, &[("CONTEXTFUL_TOKEN", &token)]);
    assert!(!refused.status.success());
    let stderr = String::from_utf8_lossy(&refused.stderr);
    assert!(stderr.contains("AuditLogUnanchored") && stderr.contains("audit anchor"), "{stderr}");
    assert_eq!(entries(), 1, "a refused signed read appends nothing");
    stdout(&run(p, &recall, &[("CONTEXTFUL_TOKEN", &token)]));
    assert_eq!(entries(), 2, "an unsigned read still appends");

    stdout(&run(p, &["audit", "anchor", "--project", "research", "--issuer-key", ".contextful/issuer.seed"], &[]));
    let mut signed_recall = recall.to_vec();
    signed_recall.extend(["--issuer-key", ".contextful/issuer.seed"]);
    stdout(&run(p, &signed_recall, &[("CONTEXTFUL_TOKEN", &token)]));
    assert_eq!(entries(), 3);
}

// spec: disclosure.record.read-signer@decf0053
#[test]
fn a_signed_read_whose_signing_key_the_pins_omit_is_refused_and_appends_nothing() {
    let (dir, public, token) = project();
    let p = dir.path();
    stdout(&run(p, &["audit", "anchor", "--project", "research", "--issuer-key", ".contextful/issuer.seed"], &[]));
    stdout(&run(p, &["token", "keygen", "--out", ".contextful/other.seed"], &[]));
    let entries = || contextful_policy::audit::entries(&p.join(".contextful/audit")).unwrap().len();
    let before = entries();
    let recall = ["memory", "recall", "--project", "research", "--table", "memory/facts", "--subject", "acme", "--public-key", &public, "--audience", AUD];
    let mut wrong = recall.to_vec();
    wrong.extend(["--issuer-key", ".contextful/other.seed"]);
    let refused = run(p, &wrong, &[("CONTEXTFUL_TOKEN", &token)]);
    assert!(!refused.status.success());
    let stderr = String::from_utf8_lossy(&refused.stderr);
    assert!(stderr.contains("IssuerKeyUnresolvable"), "{stderr}");
    assert_eq!(entries(), before, "a read under an unpinned signing key appends nothing");
    let mut right = recall.to_vec();
    right.extend(["--issuer-key", ".contextful/issuer.seed"]);
    stdout(&run(p, &right, &[("CONTEXTFUL_TOKEN", &token)]));
    assert_eq!(entries(), before + 1);
}

/// By default, `contextful memory synthesize` starts synthesis. A scheduled `[[job]]` of kind `synthesize` targeting
/// the claims table replaces that default. Each execution advances only through {{read.synthesize.pass-cursor}}.
// spec: read.synthesize.cadence@381c8db9
#[test]
fn a_scheduled_synthesize_job_replaces_the_operator_triggered_pass() {
    let (dir, public, token) = project();
    let p = dir.path();
    let endpoint = Endpoint::start(vec![claim("Dana")]);
    let manifest = std::fs::read_to_string(p.join("contextful.toml")).unwrap();
    let declare = |schedule: &str| {
        let job = format!(
            "site_id = \"site\"\n{manifest}\n[[job]]\nname = \"synth-facts\"\nkind = \"synthesize\"\ntarget = \"memory/facts\"\nsource = \"research/notes\"\nendpoint = \"{}\"\nmodel = \"fixture\"\n{schedule}",
            endpoint.url()
        );
        std::fs::write(p.join("contextful.toml"), job).unwrap();
    };
    let env = [("CONTEXTFUL_TOKEN", token.as_str()), ("CONTEXTFUL_ISSUER_PUBKEY", public.as_str()), ("CONTEXTFUL_AUDIENCE", AUD), ("CONTEXTFUL_INFERENCE_KEY", "")];
    let cycle = |now: &str| -> Value { serde_json::from_str(&stdout(&run(p, &["pipeline", "serve", "--cycle", "--project", "research", "--now", now], &env))).unwrap() };
    let recalled = || -> usize {
        let args = ["memory", "recall", "--project", "research", "--table", "memory/facts", "--subject", "acme", "--observed-at", "2030-06-01T00:00:00Z", "--public-key", &public, "--audience", AUD];
        let answer: Value = serde_json::from_str(&stdout(&run(p, &args, &[("CONTEXTFUL_TOKEN", &token)]))).unwrap();
        answer["rows"].as_array().unwrap().len()
    };

    // Without a schedule the job stays unarmed: synthesis waits for the operator.
    declare("");
    stdout(&run(p, &["pipeline", "import", "--project", "research"], &env));
    let idle = cycle("2030-01-01T00:01:00Z");
    assert_eq!((idle["armed"].clone(), idle["fired"].clone()), (json!(0), json!([])), "{idle}");
    assert_eq!((recalled(), endpoint.bodies.lock().unwrap().len()), (0, 0));

    // A schedule arms it; each fire reads only runs past the pass cursor.
    declare("schedule = \"every 1m\"\n");
    stdout(&run(p, &["pipeline", "apply", "--project", "research"], &env));
    let fired = cycle("2030-01-01T00:02:00Z");
    assert_eq!(fired["fired"], json!(["job:synth-facts"]), "{fired}");
    assert_eq!((recalled(), endpoint.bodies.lock().unwrap().len()), (1, 1));
    let again = cycle("2030-01-01T00:04:00Z");
    assert_eq!(again["fired"], json!(["job:synth-facts"]), "{again}");
    assert_eq!(endpoint.bodies.lock().unwrap().len(), 1, "no run past the cursor, no model call");
}

/// A project scheduling `synth-facts` every minute, `zone` appended, imported under the minted token.
fn scheduled_synthesis(p: &Path, public: &str, token: &str, endpoint: &Endpoint, zone: &str) {
    let manifest = std::fs::read_to_string(p.join("contextful.toml")).unwrap();
    let job = format!(
        "site_id = \"site\"\n{manifest}\n[[job]]\nname = \"synth-facts\"\nkind = \"synthesize\"\ntarget = \"memory/facts\"\nsource = \"research/notes\"\nendpoint = \"{}\"\nmodel = \"fixture\"\nschedule = \"every 1m\"\n{zone}",
        endpoint.url()
    );
    std::fs::write(p.join("contextful.toml"), job).unwrap();
    stdout(&run(p, &["pipeline", "import", "--project", "research"], &[("CONTEXTFUL_TOKEN", token), ("CONTEXTFUL_ISSUER_PUBKEY", public), ("CONTEXTFUL_AUDIENCE", AUD)]));
}

const HQ: &str = "[project.zone]\ndefault = \"on-prem:hq\"\n";

/// One serve cycle holding the issuer pin and audience and no credential.
fn tokenless_cycle(p: &Path, public: &str, extra: &[&str]) -> Output {
    let mut args = vec!["pipeline", "serve", "--cycle", "--project", "research", "--now", "2030-01-01T00:02:00Z"];
    args.extend_from_slice(extra);
    run(p, &args, &[("CONTEXTFUL_ISSUER_PUBKEY", public), ("CONTEXTFUL_AUDIENCE", AUD), ("CONTEXTFUL_INFERENCE_KEY", "")])
}

#[test]
fn an_issuer_key_mints_each_scheduled_synthesize_fire_its_own_credential() {
    let (dir, public, token) = project();
    let p = dir.path();
    let endpoint = Endpoint::start(vec![claim("Dana")]);
    scheduled_synthesis(p, &public, &token, &endpoint, HQ);
    let out = tokenless_cycle(p, &public, &["--issuer-key", ".contextful/issuer.seed"]);
    let fired: Value = serde_json::from_str(&stdout(&out)).unwrap();
    assert_eq!(fired["fired"], json!(["job:synth-facts"]), "{fired}");
    assert_eq!(endpoint.bodies.lock().unwrap().len(), 1, "{}", String::from_utf8_lossy(&out.stderr));
    let query = |sql: &str| -> Value { serde_json::from_str::<Value>(&stdout(&run(p, &["query", "--json", "--project", "research", sql], &[]))).unwrap()["rows"].clone() };
    assert_eq!(query("SELECT object, _authored_by FROM \"memory/facts\""), json!([["Dana", "svc://job/synth-facts"]]), "the fire's own credential writes the claim");
}

#[test]
fn a_serve_scheduling_a_synthesize_job_without_key_or_credential_refuses_at_start() {
    let (dir, public, token) = project();
    let p = dir.path();
    let endpoint = Endpoint::start(vec![claim("Dana")]);
    scheduled_synthesis(p, &public, &token, &endpoint, HQ);
    let refused = tokenless_cycle(p, &public, &[]);
    assert!(!refused.status.success());
    let stderr = String::from_utf8_lossy(&refused.stderr);
    assert!(stderr.contains("FireCredentialMissing") && stderr.contains("synth-facts"), "{stderr}");
    assert!(endpoint.bodies.lock().unwrap().is_empty(), "no fire runs");
}

/// A `cohort` model over `research/notes`, one row per note.
const COHORT: &str = r#"
[[model]]
id = "research/by_text"
sql = "SELECT text, note_id FROM \"research/notes\""
unique_key = ["text", "note_id"]
[model.contract]
version = "1.0.0"
columns = [{ name = "text", type = "utf8", nullable = false }, { name = "note_id", type = "utf8", nullable = false }]
[model.disclosure]
grouping_allowlist = ["text"]
contributor_key = "note_id"
min_group_size = 2
governance = "cohort"
"#;

/// A scheduled synthesize fire over a cohort model's source refuses before reading a row or
/// calling its endpoint until the job declares `read_individual = true`; the fire then mints
/// `read-individual` over its source and target, runs, and its claims answer only
/// `read-individual`.
// spec: surface.dispatch.fire-individual@cd29bedf
// spec: disclosure.bound-cohort.synthesize-individual@fa72ffb0
// spec: disclosure.bound-cohort.derived-individual@b63ba1cf
#[test]
fn a_synthesize_fire_over_a_cohort_source_runs_only_under_its_declared_grant() {
    let (dir, public, token) = project();
    let p = dir.path();
    let manifest = std::fs::read_to_string(p.join("contextful.toml")).unwrap();
    std::fs::write(p.join("contextful.toml"), format!("{manifest}{COHORT}")).unwrap();
    let endpoint = Endpoint::start(vec![claim("Dana")]);
    scheduled_synthesis(p, &public, &token, &endpoint, HQ);
    let facts = p.join(".contextful/context/research/tables/memory/facts");
    let cycle = |now: &str| -> Output {
        let args = ["pipeline", "serve", "--cycle", "--project", "research", "--now", now, "--issuer-key", ".contextful/issuer.seed"];
        run(p, &args, &[("CONTEXTFUL_ISSUER_PUBKEY", public.as_str()), ("CONTEXTFUL_AUDIENCE", AUD), ("CONTEXTFUL_INFERENCE_KEY", "")])
    };

    let refused = cycle("2030-01-01T00:02:00Z");
    let report = format!("{}{}", String::from_utf8_lossy(&refused.stdout), String::from_utf8_lossy(&refused.stderr));
    assert!(report.contains("DisclosurePerIndividualUngranted") && report.contains("research/notes"), "{report}");
    assert!(endpoint.bodies.lock().unwrap().is_empty(), "a refused fire calls no endpoint");
    assert!(!facts.exists(), "a refused fire lands no claim");

    let declared = std::fs::read_to_string(p.join("contextful.toml")).unwrap().replace("model = \"fixture\"\n", "model = \"fixture\"\nread_individual = true\n");
    std::fs::write(p.join("contextful.toml"), declared).unwrap();
    stdout(&run(p, &["pipeline", "apply", "--project", "research"], &[("CONTEXTFUL_TOKEN", &token), ("CONTEXTFUL_ISSUER_PUBKEY", &public), ("CONTEXTFUL_AUDIENCE", AUD)]));
    let fired: Value = serde_json::from_str(&stdout(&cycle("2030-01-01T00:04:00Z"))).unwrap();
    assert_eq!(fired["fired"], json!(["job:synth-facts"]), "{fired}");
    assert_eq!(endpoint.bodies.lock().unwrap().len(), 1, "{fired}");
    assert!(facts.exists(), "the granted fire lands its claims");

    let mint = |actions: &[&str]| {
        let mut args = vec!["token", "mint", "--issuer-key", ".contextful/issuer.seed", "--on-behalf-of", "user://dana@acme.example", "--agent", "agent://reader", "--zone", "on-prem:hq", "--table", "memory/*", "--table", "research/notes", "--ttl", "600"];
        for action in actions {
            args.extend(["--action", action]);
        }
        stdout(&run(p, &args, &[]))
    };
    let args = ["memory", "recall", "--project", "research", "--table", "memory/facts", "--subject", "acme", "--observed-at", "2030-06-01T00:00:00Z", "--public-key", &public, "--audience", AUD];
    let recall = |token: &str| run(p, &args, &[("CONTEXTFUL_TOKEN", token)]);
    let withheld = recall(&mint(&["read"]));
    assert!(!withheld.status.success());
    assert!(String::from_utf8_lossy(&withheld.stderr).contains("DisclosurePerIndividualUngranted"), "{}", String::from_utf8_lossy(&withheld.stderr));
    let granted: Value = serde_json::from_str(&stdout(&recall(&mint(&["read", "read-individual"])))).unwrap();
    assert_eq!(granted["rows"].as_array().map(Vec::len), Some(1), "{granted}");
}

// spec: surface.dispatch.fire-credential-zone@33b36162
#[test]
fn an_issuer_key_serve_in_a_project_declaring_no_default_zone_refuses_at_start() {
    let (dir, public, token) = project();
    let p = dir.path();
    let endpoint = Endpoint::start(vec![claim("Dana")]);
    scheduled_synthesis(p, &public, &token, &endpoint, "");
    let refused = tokenless_cycle(p, &public, &["--issuer-key", ".contextful/issuer.seed"]);
    assert!(!refused.status.success());
    let stderr = String::from_utf8_lossy(&refused.stderr);
    assert!(stderr.contains("FireCredentialZoneUnset") && stderr.contains("[project.zone]"), "{stderr}");
    assert!(endpoint.bodies.lock().unwrap().is_empty(), "no fire runs");
}

const SETTLEMENT: &str = "\n[[table]]\nname = \"memory/predictions\"\nshape = \"predictions\"\ncolumns = [\"prediction_id\", \"subject\", \"predicted_at\", \"horizon_secs\", \"deadline_at\", \"resolution_source\", \"comparator\", \"statement\", \"metric\"]\n\n[[table]]\nname = \"memory/outcomes\"\nshape = \"outcomes\"\ncolumns = [\"prediction_id\", \"observed_at\", \"verdict\", \"resolution_source\", \"signal\", \"settling_citation\", \"metric\"]\n";

/// A project declaring a `predictions` and an `outcomes` table, admitting a minted token.
struct Settling {
    dir: tempfile::TempDir,
    public: String,
    token: String,
}

impl Settling {
    fn new() -> Settling {
        let (dir, public, token) = project();
        let manifest = dir.path().join("contextful.toml");
        let declaration = format!("{}{SETTLEMENT}", std::fs::read_to_string(&manifest).unwrap());
        std::fs::write(&manifest, declaration).unwrap();
        Settling { dir, public, token }
    }

    /// `contextful memory <args>` under the minted token.
    fn memory(&self, args: &[&str]) -> Output {
        let mut all = vec!["memory"];
        all.extend_from_slice(args);
        all.extend(["--project", "research", "--public-key", &self.public, "--audience", AUD]);
        run(self.dir.path(), &all, &[("CONTEXTFUL_TOKEN", &self.token)])
    }

    fn refused(&self, args: &[&str], identifier: &str) {
        let out = self.memory(args);
        assert!(!out.status.success());
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(stderr.contains(identifier), "{stderr}");
    }

    /// A written claim whose id a prediction registers twice and an observation settles;
    /// a conflicting re-use of the id refuses. Returns the claim id.
    fn settled_claim(&self) -> String {
        let written = stdout(&self.memory(&["write", "--into", "memory/facts", "--claim", &one("Dana")]));
        let claim_id = written.split_whitespace().nth(2).unwrap().to_string();
        let registration = json!({ "prediction_id": claim_id, "subject": "acme", "predicted_at": "2030-01-01T00:00:00Z", "horizon_secs": 86400,
            "resolution_source": "adjudicator" })
        .to_string();
        for _ in 0..2 {
            let printed = stdout(&self.memory(&["predict", "--into", "memory/predictions", "--registration", &registration]));
            assert_eq!(printed, format!("prediction_id: {claim_id}"));
        }
        let conflicting = registration.replace("adjudicator", "manual");
        self.refused(&["predict", "--into", "memory/predictions", "--registration", &conflicting], "OutcomeRegistrationConflict");
        let observation = json!({ "prediction_id": claim_id, "observed_at": "2030-01-01T12:00:00Z", "verdict": true, "resolution_source": "adjudicator",
            "settling_citation": "https://example.org/filing" })
        .to_string();
        stdout(&self.memory(&["observe", "--into", "memory/outcomes", "--observation", &observation]));
        claim_id
    }

    fn owner_rows(&self, sql: &str) -> Value {
        let out: Value = serde_json::from_str(&stdout(&run(self.dir.path(), &["query", "--json", "--project", "research", sql], &[]))).unwrap();
        out["rows"].clone()
    }
}

/// `contextful memory predict --into <table> --registration <json>` validates {{read.settle.registration}}. Under the
/// admitted credential's write grant, the command stores the registration in that `predictions` table and prints
/// `prediction_id: <id>`.
// spec: read.settle.predict-verb@3150c7b9
#[test]
fn predict_lands_a_registration_and_prints_its_id() {
    let s = Settling::new();
    for (verb, fields) in [
        ("predict", &["prediction_id", "subject", "predicted_at", "horizon_secs", "deadline_at", "watch", "resolution_source", "comparator", "statement", "metric", "read.settle"][..]),
        ("observe", &["prediction_id", "observed_at", "verdict", "resolution_source", "signal", "settling_citation", "metric", "read.settle"][..]),
    ] {
        let help = stdout(&run(s.dir.path(), &["memory", verb, "--help"], &[]));
        for field in fields {
            assert!(help.contains(field), "`memory {verb} --help` documents `{field}`: {help}");
        }
    }
    let claim_id = s.settled_claim();
    assert_eq!(s.owner_rows("SELECT prediction_id, resolution_source, CAST(deadline_at AS VARCHAR) FROM \"memory/predictions\""), json!([[claim_id, "adjudicator", "2030-01-02 00:00:00+00"]]));
    let unformed = json!({ "prediction_id": "p3", "subject": "acme", "predicted_at": "2030-01-01T00:00:00Z", "watch": true, "horizon_secs": 60, "resolution_source": "manual" });
    s.refused(&["predict", "--into", "memory/predictions", "--registration", &unformed.to_string()], "OutcomeRegistrationInvalid");

    // A metric prediction names its series and statement; a bare-date deadline runs through that day.
    let metric = json!({ "prediction_id": "m1", "subject": "acme", "predicted_at": "2030-01-01T00:00:00Z", "deadline_at": "2030-03-31",
        "resolution_source": "metric", "comparator": ">= 1000000", "metric": "metrics/revenue:acme-q1", "statement": "Acme books at least 1M in Q1" });
    assert_eq!(stdout(&s.memory(&["predict", "--into", "memory/predictions", "--registration", &metric.to_string()])), "prediction_id: m1");
    assert_eq!(
        s.owner_rows("SELECT metric, statement, CAST(deadline_at AS VARCHAR) FROM \"memory/predictions\" WHERE prediction_id = 'm1'"),
        json!([["metrics/revenue:acme-q1", "Acme books at least 1M in Q1", "2030-04-01 00:00:00+00"]])
    );
    let mut restated = metric.clone();
    restated["deadline_at"] = json!("2030-04-01T00:00:00Z");
    assert_eq!(stdout(&s.memory(&["predict", "--into", "memory/predictions", "--registration", &restated.to_string()])), "prediction_id: m1");
    let mut reworded = metric.clone();
    reworded["statement"] = json!("Acme books at least 2M in Q1");
    s.refused(&["predict", "--into", "memory/predictions", "--registration", &reworded.to_string()], "OutcomeRegistrationConflict");
    let mut judged = metric.clone();
    judged["prediction_id"] = json!("m2");
    judged["resolution_source"] = json!("manual");
    judged.as_object_mut().unwrap().remove("comparator");
    s.refused(&["predict", "--into", "memory/predictions", "--registration", &judged.to_string()], "OutcomeMetricReferenceInvalid");
    let mut long = metric.clone();
    long["prediction_id"] = json!("m3");
    long["statement"] = json!("x".repeat(4097));
    s.refused(&["predict", "--into", "memory/predictions", "--registration", &long.to_string()], "OutcomeStatementTooLong");
    let unnamed = json!({ "prediction_id": "m1", "observed_at": "2030-04-01T00:00:00Z", "verdict": true, "resolution_source": "metric" });
    s.refused(&["observe", "--into", "memory/outcomes", "--observation", &unnamed.to_string()], "OutcomeSourceMismatch");
    let other_series = json!({ "prediction_id": "m1", "observed_at": "2030-04-01T00:00:00Z", "verdict": true, "resolution_source": "metric", "metric": "metrics/revenue:acme-q2" });
    s.refused(&["observe", "--into", "memory/outcomes", "--observation", &other_series.to_string()], "OutcomeSourceMismatch");
    let wrong_source = json!({ "prediction_id": "m1", "observed_at": "2030-04-01T00:00:00Z", "verdict": true, "resolution_source": "manual", "settling_citation": "https://example.org/r" });
    let out = s.memory(&["observe", "--into", "memory/outcomes", "--observation", &wrong_source.to_string()]);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("`metric`") && !stderr.contains("Metric"), "wire names only: {stderr}");
}

/// `contextful memory observe --into <table> --observation <json>` reads the stored prediction registration through
/// the writer's session. After checking {{read.settle.source-mismatch}} and {{read.settle.settling-citation}}, the
/// command stores the observation in that `outcomes` table and prints its label row.
// spec: read.settle.observe-verb@6e8cdc66
#[test]
fn observe_checks_against_the_stored_registration_and_prints_the_label_row() {
    let s = Settling::new();
    let registration = json!({ "prediction_id": "p2", "subject": "acme", "predicted_at": "2030-01-01T00:00:00Z", "horizon_secs": 86400, "resolution_source": "adjudicator" });
    stdout(&s.memory(&["predict", "--into", "memory/predictions", "--registration", &registration.to_string()]));
    let observe = |observation: Value| vec!["observe".to_string(), "--into".into(), "memory/outcomes".into(), "--observation".into(), observation.to_string()];
    let refused = |observation: Value, identifier: &str| {
        let args = observe(observation);
        s.refused(&args.iter().map(String::as_str).collect::<Vec<_>>(), identifier);
    };
    refused(json!({ "prediction_id": "p2", "observed_at": "2030-01-01T12:00:00Z", "verdict": true, "resolution_source": "metric" }), "OutcomeSourceMismatch");
    refused(json!({ "prediction_id": "p2", "observed_at": "2030-01-01T12:00:00Z", "verdict": true, "resolution_source": "adjudicator" }), "OutcomeCitationMissing");
    refused(json!({ "prediction_id": "p9", "observed_at": "2030-01-01T12:00:00Z" }), "OutcomePredictionUnregistered");
    let settled = json!({ "prediction_id": "p2", "observed_at": "2030-01-01T12:00:00Z", "verdict": false, "resolution_source": "adjudicator",
        "settling_citation": "https://example.org/ruling" });
    let args = observe(settled);
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    let label: Value = serde_json::from_str(&stdout(&s.memory(&args))).unwrap();
    let col = |n: &str| label["columns"].as_array().unwrap().iter().position(|c| c == n).unwrap();
    assert_eq!(label["rows"].as_array().unwrap().len(), 1, "{label}");
    assert_eq!(label["rows"][0][col("verdict")], json!(false));
    assert_eq!(label["rows"][0][col("settling_citation")], json!("https://example.org/ruling"));
    assert_eq!(label["rows"][0][col("lead_time_s")].as_f64(), Some(43_200.0));
    // An identical observation restates: nothing new lands and the same label prints.
    let again: Value = serde_json::from_str(&stdout(&s.memory(&args))).unwrap();
    assert_eq!(again["rows"], label["rows"]);
    assert_eq!(s.owner_rows("SELECT CAST(count(DISTINCT _run_id) AS INTEGER) FROM \"memory/outcomes\""), json!([[1]]));
}

/// Both verbs admit a capability token as `contextful memory write` does, and each landed row carries `_authored_by` from its subject under {{store.reserve.injected}}.
// spec: read.settle.verb-attribution@49aa703d
#[test]
fn settlement_rows_carry_the_token_subject() {
    let s = Settling::new();
    s.settled_claim();
    for table in ["memory/predictions", "memory/outcomes"] {
        assert_eq!(s.owner_rows(&format!("SELECT DISTINCT _authored_by FROM \"{table}\"")), json!([["user://dana@acme.example"]]), "{table}");
    }
}

/// `contextful memory calibration --table <claims>` joins verdict-bearing labels to claims by `prediction_id =
/// claim_id` through the caller's session. The command prints {{read.settle.calibration-report}} with joined-
/// prediction count, using JSON under `--json`.
// spec: read.settle.calibration-verb@e7165367
#[test]
fn calibration_reports_the_settled_claims_of_a_table() {
    let s = Settling::new();
    s.settled_claim();
    let empty = Settling::new();
    assert_eq!(stdout(&empty.memory(&["calibration", "--table", "memory/facts"])), "0 settled predictions joined a claim in memory/facts");
    let none: Value = serde_json::from_str(&stdout(&empty.memory(&["calibration", "--table", "memory/facts", "--json"]))).unwrap();
    assert_eq!(none, json!({ "joined": 0, "groups": [] }));

    let report: Value = serde_json::from_str(&stdout(&s.memory(&["calibration", "--table", "memory/facts", "--json"]))).unwrap();
    assert_eq!(report["joined"], json!(1), "{report}");
    let groups = &report["groups"];
    assert_eq!(groups.as_array().unwrap().len(), 1, "{report}");
    assert_eq!((&groups[0]["shape"], &groups[0]["predicate"], &groups[0]["samples"]), (&json!("memory_facts"), &json!("cfo"), &json!(1)));
    assert_eq!(groups[0]["brier"], json!(0.0), "a claim at confidence 1.0 that held");
    let text = stdout(&s.memory(&["calibration", "--table", "memory/facts"]));
    assert!(text.starts_with("1 settled predictions joined a claim in memory/facts"), "{text}");
    assert!(text.contains("memory_facts cfo: 1 samples"), "{text}");
}

/// `contextful memory explain` resolves an alias written in another script to one card with
/// the live claims about its identity, synthesized and written alike, lists the candidates
/// an ambiguous alias matches, and returns the identity alone to a credential holding no
/// grant on the claims table, appending the read entry the tool call appends.
// spec: read.resolve-entity.card-cli-verb@b416ffbf
#[test]
fn explain_cli_answers_one_card_candidates_or_the_identity_alone() {
    let (dir, public, token) = project();
    let p = dir.path();
    let manifest = std::fs::read_to_string(p.join("contextful.toml")).unwrap();
    std::fs::write(
        p.join("contextful.toml"),
        format!("{manifest}\n[[table]]\nname = \"memory/entities\"\nshape = \"memory_entities\"\ncolumns = [\"entity_id\", \"kind\", \"name\", \"aliases\"]\n"),
    )
    .unwrap();
    let entities = [
        json!({ "entity_id": "e-acme", "kind": "company", "name": "Acme Holdings", "aliases": "[\"ACME\",\"アクメ\"]" }),
        json!({ "entity_id": "e-globex", "kind": "company", "name": "Globex", "aliases": "[\"GX\"]" }),
        json!({ "entity_id": "e-gx", "kind": "fund", "name": "GX Partners", "aliases": "[\"gx\"]" }),
    ];
    std::fs::write(p.join("entities.jsonl"), entities.iter().map(|e| format!("{e}\n")).collect::<String>()).unwrap();
    stdout(&run(p, &["context", "land", "memory/entities", "--project", "research", "--rows", "entities.jsonl", "--run-id", "run-entities", "--site-id", "site-a"], &[]));
    let claim = json!({ "subject": "e-acme", "predicate": "cfo", "object": "Dana", "confidence": 1.0,
        "evidence": [{ "table": "research/notes", "run": "run-0001", "seq": 0 }] })
    .to_string();
    stdout(&run(p, &["memory", "write", "--project", "research", "--into", "memory/facts", "--claim", &claim, "--public-key", &public, "--audience", AUD], &[("CONTEXTFUL_TOKEN", &token)]));
    // Synthesis resolves the mention `ACME` and keys its claim by the canonical name.
    let extracted = json!({ "claims": [{ "subject": "ACME", "predicate": "ceo", "object": "Lee", "confidence": 0.9,
        "evidence": [{ "table": "research/notes", "run": "run-0001", "seq": 0 }] }] })
    .to_string();
    let endpoint = Endpoint::start(vec![extracted]);
    let url = endpoint.url();
    let synthesize = [
        "memory", "synthesize", "--project", "research", "--source", "research/notes", "--into", "memory/facts", "--endpoint", &url,
        "--model", "fixture", "--public-key", &public, "--audience", AUD,
    ];
    let report = stdout(&run(p, &synthesize, &[("CONTEXTFUL_TOKEN", &token), ("CONTEXTFUL_INFERENCE_KEY", "")]));
    assert!(report.contains("1 landed"), "{report}");

    let explain = |reference: &str, token: &str| -> Value {
        let args = ["memory", "explain", "--project", "research", "--table", "memory/entities", "--reference", reference, "--claims", "memory/facts", "--public-key", &public, "--audience", AUD];
        serde_json::from_str(&stdout(&run(p, &args, &[("CONTEXTFUL_TOKEN", token)]))).unwrap()
    };
    let identity = json!({ "entity_id": "e-acme", "kind": "company", "name": "Acme Holdings", "aliases": ["ACME", "アクメ"] });
    let card = explain("アクメ", &token);
    assert_eq!(card["entity"], identity, "{card}");
    let at = |c: &str| card["claims"]["columns"].as_array().unwrap().iter().position(|x| x == c).unwrap();
    let mut keyed: Vec<(Value, Value)> = card["claims"]["rows"].as_array().unwrap().iter().map(|r| (r[at("subject")].clone(), r[at("object")].clone())).collect();
    keyed.sort_by_key(|(subject, _)| subject.to_string());
    assert_eq!(keyed, [(json!("Acme Holdings"), json!("Lee")), (json!("e-acme"), json!("Dana"))], "{card}");

    assert_eq!(explain("GX", &token), json!({ "entity": null, "candidates": ["e-globex", "e-gx"] }));

    let identities_only = stdout(&run(
        p,
        &["token", "mint", "--issuer-key", ".contextful/issuer.seed", "--on-behalf-of", "user://dana@acme.example", "--agent", "agent://reader", "--zone", "on-prem:hq", "--action", "read", "--table", "memory/entities", "--ttl", "600"],
        &[],
    ));
    assert_eq!(explain("ACME", &identities_only), json!({ "entity": identity, "candidates": [] }));

    // `--internals` returns the tool's internals object.
    let args = ["memory", "explain", "--project", "research", "--table", "memory/entities", "--reference", "ACME", "--claims", "memory/facts", "--internals", "--public-key", &public, "--audience", AUD];
    let traced: Value = serde_json::from_str(&stdout(&run(p, &args, &[("CONTEXTFUL_TOKEN", &token)]))).unwrap();
    assert_eq!(traced["contextful.internals"]["engine"], json!("duckdb"), "{traced}");
    assert_eq!(traced["contextful.internals"]["row_count"], json!(3), "{traced}");

    // Each call appended a read entry naming only the tables it read, and its claim ids.
    let audit = |sql: &str| -> Value {
        let out: Value = serde_json::from_str(&stdout(&run(p, &["audit", "query", "--project", "research", "--sql", sql], &[]))).unwrap();
        out["rows"].clone()
    };
    let per_table = audit("SELECT table_name, CAST(count(*) AS VARCHAR), CAST(count(claims) AS VARCHAR) FROM audit_reads GROUP BY table_name ORDER BY table_name");
    assert_eq!(per_table, json!([["memory/entities", "4", "2"], ["memory/facts", "2", "2"], ["research/notes", "2", "2"]]));
    assert_eq!(audit("SELECT table_name FROM audit_reads WHERE agent = 'agent://reader'"), json!([["memory/entities"]]));
    let mut ids: Vec<Value> = card["claims"]["rows"].as_array().unwrap().iter().map(|r| json!([r[at("claim_id")]])).collect();
    ids.sort_by_key(Value::to_string);
    assert_eq!(audit("SELECT DISTINCT unnest(CAST(claims AS VARCHAR[])) AS id FROM audit_reads ORDER BY id"), json!(ids));
}

/// An entity upsert lands a canonical identity, merges aliases on its key, refuses a
/// mention another entity owns, and a synthesis pass resolves the upserted alias.
// spec: read.resolve-entity.entity-upsert@5aeb908f
// spec: read.resolve-entity.upsert-merge@f43da41c
// spec: read.resolve-entity.alias-conflict@4c7114b5
#[test]
fn an_entity_upsert_lands_merges_aliases_and_refuses_a_taken_mention() {
    let (dir, public, token) = project();
    let p = dir.path();
    let manifest = p.join("contextful.toml");
    let declaration = format!(
        "{}\n[[table]]\nname = \"memory/entities\"\nshape = \"memory_entities\"\ncolumns = [\"entity_id\", \"kind\", \"name\", \"aliases\"]\n",
        std::fs::read_to_string(&manifest).unwrap()
    );
    std::fs::write(&manifest, declaration).unwrap();
    let env = [("CONTEXTFUL_TOKEN", token.as_str())];
    let upsert = |id: &str, name: &str, aliases: &[&str]| -> Output {
        let mut args = vec!["memory", "entity", "upsert", "--project", "research", "--table", "memory/entities", "--id", id, "--kind", "org", "--name", name];
        for a in aliases {
            args.extend(["--alias", a]);
        }
        args.extend(["--public-key", &public, "--audience", AUD]);
        run(p, &args, &env)
    };

    assert!(stdout(&upsert("e-acme", "Acme", &["Acme Corp"])).contains("added e-acme"));
    let merged = stdout(&upsert("e-acme", "Acme Inc", &["acme corp", "ACME Holdings"]));
    assert!(merged.contains("updated e-acme"), "{merged}");
    let query = ["query", "--json", "--project", "research", "SELECT entity_id, name, aliases, _authored_by FROM \"memory/entities\""];
    let rows: Value = serde_json::from_str(&stdout(&run(p, &query, &[]))).unwrap();
    assert_eq!(rows["rows"], json!([["e-acme", "Acme Inc", "[\"Acme Corp\",\"ACME Holdings\"]", "user://dana@acme.example"]]));

    let taken = upsert("e-other", "Globex", &["Acme Corp"]);
    assert!(!taken.status.success());
    assert!(String::from_utf8_lossy(&taken.stderr).contains("MemoryEntityAliasConflict"), "{}", String::from_utf8_lossy(&taken.stderr));
    let rows: Value = serde_json::from_str(&stdout(&run(p, &query, &[]))).unwrap();
    assert_eq!(rows["rows"].as_array().unwrap().len(), 1, "the refused upsert lands nothing");

    let content = json!({ "claims": [{ "subject": "ACME Holdings", "predicate": "cfo", "object": "Dana", "confidence": 0.9,
        "evidence": [{ "table": "research/notes", "run": "run-0001", "seq": 0 }] }] })
    .to_string();
    let endpoint = Endpoint::start(vec![content]);
    let url = endpoint.url();
    let synth = [
        "memory", "synthesize", "--project", "research", "--source", "research/notes", "--into", "memory/facts", "--endpoint", &url,
        "--model", "fixture", "--public-key", &public, "--audience", AUD,
    ];
    assert!(stdout(&run(p, &synth, &[("CONTEXTFUL_TOKEN", &token), ("CONTEXTFUL_INFERENCE_KEY", "")])).contains("1 landed"));
    let facts = ["query", "--json", "--project", "research", "SELECT subject FROM \"memory/facts\""];
    let rows: Value = serde_json::from_str(&stdout(&run(p, &facts, &[]))).unwrap();
    assert_eq!(rows["rows"], json!([["Acme Inc"]]), "the upserted alias resolves to the canonical name");
}
