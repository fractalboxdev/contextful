//! `contextful serve --http` through the built binary: startup refusals, then MCP
//! Streamable HTTP admitting a short-lived bearer, and a holder-bound credential minted by
//! `token mint --holder` under a proof on every request.

use contextful_core::time::Instant;
use contextful_policy::possession::{jwk_thumbprint, sign_proof, ProofRequest};
use ed25519_dalek::SigningKey;
use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::path::Path;
use std::process::{Child, Command, Output, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};

const AUD: &str = "contextful://acme-research";

fn run(dir: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_contextful"))
        .args(args)
        .current_dir(dir)
        .env_remove("CONTEXTFUL_ISSUER_PUBKEY")
        .env_remove("CONTEXTFUL_AUDIENCE")
        .output()
        .unwrap()
}

fn stdout(out: &Output) -> String {
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

/// A project holding one landed table and an issuer; the issuer's public key.
fn project() -> (tempfile::TempDir, String) {
    project_declaring("")
}

/// [`project`], its table block carrying `keys`.
fn project_declaring(keys: &str) -> (tempfile::TempDir, String) {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path();
    std::fs::create_dir_all(p.join(".contextful")).unwrap();
    std::fs::write(p.join(".contextful/issuance.toml"), format!("default_audience = \"{AUD}\"\nmax_lifetime_secs = 86400\n")).unwrap();
    std::fs::write(p.join("contextful.toml"), format!("authoring_posture = \"per_request\"\n[[pipeline.tables]]\nname = \"research/notes\"\n{keys}")).unwrap();
    std::fs::write(p.join("notes.jsonl"), "{\"note_id\":\"n1\"}\n").unwrap();
    stdout(&run(p, &["context", "land", "research/notes", "--project", "research", "--rows", "notes.jsonl", "--run-id", "run-0001", "--site-id", "site-a"]));
    let public = stdout(&run(p, &["token", "keygen", "--out", ".contextful/issuer.seed"]));
    (dir, public)
}

/// A started face, killed however the test ends.
struct Listener(Child);

impl Drop for Listener {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// Start the face; its address, reported on standard error.
fn serve(dir: &Path, args: &[&str]) -> (Listener, String) {
    let mut child = Command::new(env!("CARGO_BIN_EXE_contextful"))
        .args(args)
        .current_dir(dir)
        .env_remove("CONTEXTFUL_ISSUER_PUBKEY")
        .env_remove("CONTEXTFUL_AUDIENCE")
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut err = BufReader::new(child.stderr.take().unwrap());
    let listener = Listener(child);
    let mut line = String::new();
    while err.read_line(&mut line).unwrap() > 0 {
        if let Some(addr) = line.trim().strip_prefix("listening on http://").and_then(|a| a.strip_suffix("/mcp")) {
            return (listener, addr.to_string());
        }
        line.clear();
    }
    panic!("the face never listened")
}

static NONCE: AtomicU64 = AtomicU64::new(0);

/// `POST /mcp` carrying `message`, with a proof from `key` or as a bare bearer.
fn post(addr: &str, message: &Value, token: &str, key: Option<&SigningKey>) -> (u16, Value) {
    let body = message.to_string();
    let auth = match key {
        None => format!("Authorization: Bearer {token}\r\n"),
        Some(key) => {
            let now = Instant::from_unix_secs(std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs() as i64).unwrap();
            let nonce = format!("cli-nonce-{}", NONCE.fetch_add(1, Ordering::SeqCst));
            let proof = sign_proof(key, &ProofRequest { method: "POST", target: "/mcp", body: body.as_bytes() }, now, &nonce);
            format!("Authorization: DPoP {token}\r\nDPoP: {proof}\r\n")
        }
    };
    let mut s = TcpStream::connect(addr).unwrap();
    write!(s, "POST /mcp HTTP/1.1\r\n{auth}Content-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}", body.len()).unwrap();
    let mut raw = String::new();
    s.read_to_string(&mut raw).unwrap();
    let (head, answer) = raw.split_once("\r\n\r\n").unwrap();
    (head.split(' ').nth(1).unwrap().parse().unwrap(), serde_json::from_str(answer).unwrap())
}

fn query() -> Value {
    json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/call", "params": { "name": "context.query", "arguments": { "sql": "SELECT note_id FROM \"research/notes\"" } } })
}

fn post_claim(addr: &str, token: &str, body: &Value, browser_origin: bool) -> (u16, Value) {
    let body = body.to_string();
    let origin = if browser_origin { "Origin: https://console.example\r\n" } else { "" };
    let mut stream = TcpStream::connect(addr).unwrap();
    write!(stream, "POST /memory/claims HTTP/1.1\r\nAuthorization: Bearer {token}\r\n{origin}Content-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}", body.len()).unwrap();
    let mut raw = String::new();
    stream.read_to_string(&mut raw).unwrap();
    let (head, data) = raw.split_once("\r\n\r\n").unwrap();
    (head.split(' ').nth(1).unwrap().parse().unwrap(), serde_json::from_str(data).unwrap_or(Value::Null))
}

#[test]
fn served_claim_write_is_durable_actor_bound_and_outside_read_mcp() {
    let memory = "\n[[table]]\nname = \"memory/facts\"\nshape = \"memory_facts\"\ncolumns = [\"claim_id\", \"subject\", \"predicate\", \"object\", \"scope\", \"tier\", \"confidence\", \"valid_from\", \"valid_to\", \"evidence\", \"superseded_by\", \"grant_id\", \"agent\"]\n";
    let (dir, public) = project_declaring(memory);
    let p = dir.path();
    let mint = |who: &str, task: &str, actions: &[&str]| {
        let mut args = vec!["token", "mint", "--issuer-key", ".contextful/issuer.seed", "--on-behalf-of", who,
            "--task", task, "--zone", "on-prem:hq", "--table", "memory/facts", "--ttl", "900"];
        for action in actions { args.extend(["--action", action]); }
        stdout(&run(p, &args))
    };
    let alice = mint("user://alice", "session-1", &["read", "write"]);
    let bob = mint("user://bob", "session-2", &["read", "write"]);
    let reader = mint("user://alice", "session-1", &["read"]);
    let claim = json!({ "into": "memory/facts", "actor": "user://alice", "session": "session-1", "dedup_key": "turn-1",
        "claim": { "subject": "Northwind", "predicate": "filings", "object": "arrived", "confidence": 0.9, "evidence": [] } });
    let (listener, addr) = serve(p, &["serve", "--http", "127.0.0.1:0", "--audience", AUD,
        "--max-in-flight", "2", "--project", "research", "--public-key", &public]);
    assert_eq!(post_claim(&addr, &reader, &claim, false).0, 403, "read grant cannot write");
    assert_eq!(post_claim(&addr, &alice, &claim, true).0, 403, "a browser origin cannot reach the write");
    let (status, first) = post_claim(&addr, &alice, &claim, false);
    assert_eq!(status, 200, "{first}");
    assert_eq!(first["landed"], true);
    assert!(first["scope"].as_str().is_some_and(|scope| scope.contains("alice") && scope.contains("session-1")), "{first}");
    let (status, duplicate) = post_claim(&addr, &alice, &claim, false);
    assert_eq!(status, 200, "{duplicate}");
    assert_eq!(duplicate["landed"], false);
    let mut borrowed = claim.clone();
    borrowed["actor"] = json!("user://bob");
    assert_eq!(post_claim(&addr, &alice, &borrowed, false).0, 403, "a writer cannot author for another operator");
    borrowed["actor"] = json!("user://alice");
    borrowed["session"] = json!("session-2");
    assert_eq!(post_claim(&addr, &alice, &borrowed, false).0, 403, "a writer cannot pick another session");
    assert_eq!(post_claim(&addr, &bob, &claim, false).0, 403, "the second operator cannot author Alice's claim");
    let write_tool = json!({ "jsonrpc": "2.0", "id": 7, "method": "tools/call",
        "params": { "name": "memory.write", "arguments": claim } });
    let (_, mcp) = post(&addr, &write_tool, &alice, None);
    assert!(mcp.to_string().contains("no tool"), "{mcp}");
    drop(listener);
    let (_restarted, addr) = serve(p, &["serve", "--http", "127.0.0.1:0", "--audience", AUD,
        "--max-in-flight", "2", "--project", "research", "--public-key", &public]);
    let recall = json!({ "jsonrpc": "2.0", "id": 8, "method": "tools/call",
        "params": { "name": "memory.recall", "arguments": { "table": "memory/facts", "subject": "Northwind" } } });
    let (status, memory) = post(&addr, &recall, &reader, None);
    assert_eq!(status, 200, "{memory}");
    assert_eq!(memory["result"]["structuredContent"]["rows"].as_array().map(Vec::len), Some(1), "{memory}");
    stdout(&run(p, &["token", "revoke", "--principal-class", "delegated"]));
    assert_eq!(post_claim(&addr, &alice, &claim, false).0, 401, "revoked authority commits nothing");
}

#[test]
fn served_exchange_without_policy_returns_the_unconfigured_refusal() {
    let (dir, public) = project();
    let (_listener, addr) = serve(dir.path(), &["serve", "--http", "127.0.0.1:0", "--audience", AUD,
        "--max-in-flight", "2", "--project", "research", "--public-key", &public]);
    let body = "not-json";
    let mut stream = TcpStream::connect(&addr).unwrap();
    write!(stream, "POST /auth/exchange HTTP/1.1\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}", body.len()).unwrap();
    let mut raw = String::new();
    stream.read_to_string(&mut raw).unwrap();
    let (head, data) = raw.split_once("\r\n\r\n").unwrap();
    assert_eq!(head.split(' ').nth(1), Some("404"));
    let answer: Value = serde_json::from_str(data).unwrap();
    assert_eq!(answer["error"]["identifier"], "ExchangeUnconfigured");
    let mut stream = TcpStream::connect(&addr).unwrap();
    stream.set_read_timeout(Some(std::time::Duration::from_secs(2))).unwrap();
    write!(stream, "POST /auth/exchange HTTP/1.1\r\nContent-Type: application/json\r\nContent-Length: 100\r\n\r\n").unwrap();
    let mut raw = String::new();
    stream.read_to_string(&mut raw).expect("unconfigured exchange answers before waiting for its body");
    assert!(raw.starts_with("HTTP/1.1 404"), "{raw}");
}

#[test]
fn served_exchange_mints_a_reader_credential_for_the_read_face() {
    let (dir, public) = project();
    let p = dir.path();
    std::fs::create_dir_all(p.join(".contextful/exchange")).unwrap();
    std::fs::write(p.join(".contextful/exchange/policy.toml"),
        "expected_iss = \"https://login.example.test/\"\nexpected_aud = \"console\"\nrole_claim = \"roles\"\n\
         [subject_map]\non_behalf_of = { claim = \"sub\", template = \"user://{}\" }\nzone = { claim = \"zone\" }\n\
         [[role_grants.reader]]\nactions = [\"read\"]\ntables = [\"research/*\"]\n").unwrap();
    std::fs::write(p.join(".contextful/exchange/verify.key"), "exchange-secret").unwrap();
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs();
    let jwt = jsonwebtoken::encode(&jsonwebtoken::Header::new(jsonwebtoken::Algorithm::HS256),
        &json!({ "iss": "https://login.example.test/", "aud": "console", "exp": now + 300,
            "sub": "reader@example.test", "zone": "on-prem:hq", "roles": ["reader"] }),
        &jsonwebtoken::EncodingKey::from_secret(b"exchange-secret")).unwrap();
    let (_listener, addr) = serve(p, &["serve", "--http", "127.0.0.1:0", "--audience", AUD,
        "--max-in-flight", "2", "--project", "research", "--public-key", &public]);
    let exchange = |jwt: &str| {
        let body = json!({ "jwt": jwt }).to_string();
        let mut stream = TcpStream::connect(&addr).unwrap();
        write!(stream, "POST /auth/exchange HTTP/1.1\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}", body.len()).unwrap();
        let mut raw = String::new();
        stream.read_to_string(&mut raw).unwrap();
        let (head, data) = raw.split_once("\r\n\r\n").unwrap();
        (head.split(' ').nth(1).unwrap().parse::<u16>().unwrap(), serde_json::from_str::<Value>(data).unwrap())
    };
    let (status, minted) = exchange(&jwt);
    assert_eq!(status, 200, "{minted}");
    let token = minted["token"].as_str().expect("the exchange returns a credential");
    let (status, answer) = post(&addr, &query(), token, None);
    assert_eq!(status, 200, "{answer}");
    assert_eq!(answer["result"]["structuredContent"]["rows"], json!([["n1"]]));
    let (status, refused) = exchange("invalid");
    assert_ne!(status, 200);
    assert_eq!(refused["error"]["identifier"], "ExchangeAssertionInvalid");
}

/// The network transport refuses to start without its audience, its ceiling, or an issuer key that resolves and parses.
// spec: topology.publish-hostname.issuer-key@a7736a7f
#[test]
fn serve_refuses_to_start_without_its_declarations_or_an_issuer_key() {
    let (dir, public) = project();
    let p = dir.path();
    let base = ["serve", "--http", "127.0.0.1:0", "--project", "research", "--public-key", public.as_str()];
    for (extra, flag) in [(vec!["--audience", AUD], "--max-in-flight"), (vec!["--audience", AUD, "--max-in-flight", "0"], "--max-in-flight"), (vec!["--max-in-flight", "2"], "--audience")] {
        let out = run(p, &[&base[..], &extra[..]].concat());
        assert!(!out.status.success());
        let err = String::from_utf8_lossy(&out.stderr);
        assert!(err.starts_with("ServeDeclarationMissing") && err.contains(flag) && !err.contains("listening"), "{err}");
    }
    for key in [None, Some("ed25519/not-hex")] {
        let mut args = vec!["serve", "--http", "127.0.0.1:0", "--audience", AUD, "--max-in-flight", "4", "--project", "research"];
        if let Some(k) = key {
            args.extend(["--public-key", k]);
        }
        let out = run(p, &args);
        assert!(!out.status.success());
        let err = String::from_utf8_lossy(&out.stderr);
        assert!(err.starts_with("IssuerKeyUnusable") && err.contains("--public-key") && !err.contains("listening"), "{err}");
    }
}

/// `token mint --holder` binds a credential to a holder key: the network face admits it
/// only under a proof on every request, admits a bearer living at most 3600 s, refuses a
/// longer-lived one, and the stdio server, whose pipe carries no proof, refuses the bound one.
#[test]
fn serve_admits_a_holder_bound_credential_under_proof_and_a_short_lived_bearer() {
    let (dir, public) = project();
    let p = dir.path();
    let mint_for = |holder: Option<&str>, ttl: &str| {
        let mut args = vec!["token", "mint", "--issuer-key", ".contextful/issuer.seed", "--on-behalf-of", "user://dana@acme.example", "--zone", "on-prem:hq", "--table", "research/*", "--ttl", ttl];
        if let Some(h) = holder {
            args.extend(["--holder", h]);
        }
        stdout(&run(p, &args))
    };
    let mint = |holder: Option<&str>| mint_for(holder, "900");
    let key = SigningKey::from_bytes(&[11; 32]);
    let jkt = jwk_thumbprint(key.verifying_key().as_bytes());
    let bound = mint(Some(&jkt));
    let introspected: Value = serde_json::from_str(&stdout(&run(p, &["token", "introspect", "--token", &bound]))).unwrap();
    assert_eq!(introspected["authority"]["cnf"]["jkt"], json!(jkt), "{introspected}");
    assert!(!run(p, &["token", "mint", "--issuer-key", ".contextful/issuer.seed", "--table", "research/*", "--holder", "short"]).status.success());
    let unbound = mint(None);

    let (_listener, addr) = serve(p, &["serve", "--http", "127.0.0.1:0", "--audience", AUD, "--max-in-flight", "2", "--project", "research", "--public-key", &public]);
    for _ in 0..2 {
        let (status, answer) = post(&addr, &query(), &bound, Some(&key));
        assert_eq!(status, 200, "{answer}");
        assert_eq!(answer["result"]["structuredContent"]["rows"], json!([["n1"]]));
    }
    // The bound credential without its proof, or under another key's, admits nothing.
    let other = SigningKey::from_bytes(&[12; 32]);
    for key in [None, Some(&other)] {
        let (status, answer) = post(&addr, &query(), &bound, key);
        assert_eq!((status, answer["error"]["identifier"].clone()), (401, json!("PossessionProofInvalid")), "{answer}");
    }
    // A bearer living 3600 s admits with no proof; one living 3601 s refuses.
    for bearer in [&unbound, &mint_for(None, "3600")] {
        let (status, answer) = post(&addr, &query(), bearer, None);
        assert_eq!((status, answer["result"]["structuredContent"]["rows"].clone()), (200, json!([["n1"]])), "{answer}");
    }
    let (status, answer) = post(&addr, &query(), &mint_for(None, "3601"), None);
    assert_eq!((status, answer["error"]["identifier"].clone()), (401, json!("BearerLifetimeExceeded")), "{answer}");

    // Over the inherited stdio pipe the bound credential carries no proof, and refuses.
    let mut child = Command::new(env!("CARGO_BIN_EXE_contextful"))
        .args(["mcp", "--project", "research", "--public-key", &public, "--audience", AUD])
        .current_dir(p)
        .env("CONTEXTFUL_TOKEN", &bound)
        .env_remove("CONTEXTFUL_ISSUER_PUBKEY")
        .env_remove("CONTEXTFUL_AUDIENCE")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let _ = writeln!(child.stdin.take().unwrap(), "{}", query());
    let out = child.wait_with_output().unwrap();
    assert!(!out.status.success() && out.stdout.is_empty(), "{}", String::from_utf8_lossy(&out.stdout));
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("PossessionProofInvalid"), "{err}");
}

/// A revocation epoch scoped on project, tenant and principal class invalidates that slice of outstanding authority.
// spec: authority.revoke.epoch@a8e2dbfc
#[test]
fn a_running_face_refuses_a_credential_below_the_scoped_epoch_from_the_next_request() {
    let (dir, public) = project();
    let p = dir.path();
    let mint = |on_behalf_of: Option<&str>| {
        let mut args = vec!["token", "mint", "--zone", "on-prem:hq", "--table", "research/*", "--ttl", "900"];
        if let Some(who) = on_behalf_of {
            args.extend(["--on-behalf-of", who]);
        }
        stdout(&run(p, &args))
    };
    let (delegated, unattributed) = (mint(Some("user://dana@acme.example")), mint(None));
    let (_listener, addr) = serve(p, &["serve", "--http", "127.0.0.1:0", "--audience", AUD, "--max-in-flight", "2", "--project", "research", "--public-key", &public]);
    for token in [&delegated, &unattributed] {
        assert_eq!(post(&addr, &query(), token, None).0, 200);
    }
    assert_eq!(stdout(&run(p, &["token", "revoke", "--principal-class", "delegated"])), "epoch 1");
    let (status, answer) = post(&addr, &query(), &delegated, None);
    assert_eq!((status, answer["error"]["identifier"].clone()), (401, json!("AuthorityRevoked")), "{answer}");
    // The bump reaches its principal class alone, and a credential minted after it admits.
    assert_eq!(post(&addr, &query(), &unattributed, None).0, 200);
    let (status, answer) = post(&addr, &query(), &mint(Some("user://dana@acme.example")), None);
    assert_eq!(status, 200, "{answer}");
}

/// Suspected compromise of signing material runs a project-wide epoch bump together with immediate retirement of the key version. Waiting out a grace window withdraws nothing.
// spec: authority.revoke.compromise@c14a17b8
#[test]
fn a_running_face_drops_a_key_retired_by_compromise_from_the_next_request() {
    let (dir, public) = project();
    let p = dir.path();
    // A spare pin published ahead of rotation keeps the face serving.
    let spare = stdout(&run(p, &["token", "keygen", "--out", "spare.seed"]));
    let mint = |key: &str| {
        stdout(&run(p, &["token", "mint", "--issuer-key", key, "--on-behalf-of", "user://dana@acme.example", "--zone", "on-prem:hq", "--table", "research/*", "--ttl", "900"]))
    };
    let before = mint(".contextful/issuer.seed");
    std::fs::copy(p.join(".contextful/issuer.seed"), p.join("stolen.seed")).unwrap();
    let pins = format!("{public},{spare}");
    let (_listener, addr) = serve(p, &["serve", "--http", "127.0.0.1:0", "--audience", AUD, "--max-in-flight", "2", "--project", "research", "--public-key", &pins]);
    assert_eq!(post(&addr, &query(), &before, None).0, 200);

    stdout(&run(p, &["token", "rotate", "--compromise"]));
    // The stolen seed signs under the bumped epoch, and its retired key verifies nothing.
    for token in [&mint("stolen.seed"), &before] {
        let (status, answer) = post(&addr, &query(), token, None);
        assert_eq!(status, 401, "{answer}");
    }
    assert_eq!(post(&addr, &query(), &mint("spare.seed"), None).0, 200);
}

/// A checkpoint pointed at a key-set ledger, or that has read one, raises `KeySetLedgerUnavailable` and admits nothing once the file is absent or malformed.
// spec: authority.revoke.ledger-unavailable@a63dfc3b
#[test]
fn a_face_below_the_project_root_reads_the_root_ledger_and_refuses_once_it_vanishes() {
    let (dir, public) = project();
    let p = dir.path();
    std::fs::write(p.join("contextful.toml"), "[project]\nname = \"research\"\n\n[[pipeline.tables]]\nname = \"research/notes\"\n").unwrap();
    let sub = p.join("sub");
    std::fs::create_dir_all(&sub).unwrap();
    let mint = || stdout(&run(&sub, &["token", "mint", "--on-behalf-of", "user://dana@acme.example", "--zone", "on-prem:hq", "--table", "research/*", "--ttl", "900"]));
    let before = mint();
    assert_eq!(stdout(&run(&sub, &["token", "revoke"])), "epoch 1");
    assert!(p.join(".contextful/keyset.toml").is_file() && !sub.join(".contextful").exists(), "the bump lands at the project root");

    let (_listener, addr) = serve(&sub, &["serve", "--http", "127.0.0.1:0", "--audience", AUD, "--max-in-flight", "2", "--public-key", &public]);
    let (status, answer) = post(&addr, &query(), &before, None);
    assert_eq!((status, answer["error"]["identifier"].clone()), (401, json!("AuthorityRevoked")), "{answer}");
    let after = mint();
    assert_eq!(post(&addr, &query(), &after, None).0, 200);

    // A ledger the face has read and that then vanishes lifts no revocation.
    std::fs::remove_file(p.join(".contextful/keyset.toml")).unwrap();
    for token in [&before, &after] {
        let (status, answer) = post(&addr, &query(), token, None);
        assert_ne!(status, 200, "{answer}");
        assert!(answer.to_string().contains("KeySetLedgerUnavailable"), "{answer}");
    }
    let out = run(p, &["token", "verify", "--public-key", &public, "--keyset", "absent.toml", "--token", &after]);
    assert!(String::from_utf8_lossy(&out.stderr).contains("KeySetLedgerUnavailable"), "{out:?}");
}

/// Append a `[sync]` block whose `prefix_from` names an unset variable to the project's
/// store config: a bucket no process can open.
fn declare_unopenable_bucket(root: &Path) {
    let store = root.join(".contextful/context/research");
    std::fs::create_dir_all(&store).unwrap();
    let config = store.join("config.toml");
    let mut text = std::fs::read_to_string(&config).unwrap_or_default();
    text.push_str(&format!(
        "\n[sync]\nendpoint = \"file://{}\"\nbucket = \"context-team\"\nprefix_from = \"CONTEXTFUL_TEST_UNSET_PREFIX\"\ncoordination = \"single-writer\"\n",
        root.join("bucket").display()
    ));
    std::fs::write(config, text).unwrap();
}

/// A `[sync]` bucket that cannot open stops neither face: `serve --http` and `mcp` start
/// and answer reads (`disclosure.attest.root-replication`).
#[test]
fn a_bucket_that_cannot_open_stops_no_face_from_answering() {
    let (dir, public) = project();
    let p = dir.path();
    declare_unopenable_bucket(p);
    let token = stdout(&run(p, &["token", "mint", "--on-behalf-of", "user://dana@acme.example", "--zone", "on-prem:hq", "--table", "research/*", "--ttl", "900"]));

    let (_listener, addr) = serve(p, &["serve", "--http", "127.0.0.1:0", "--audience", AUD, "--max-in-flight", "2", "--project", "research", "--public-key", &public]);
    let (status, answer) = post(&addr, &query(), &token, None);
    assert_eq!((status, answer["result"]["structuredContent"]["rows"].clone()), (200, json!([["n1"]])), "{answer}");

    let mut child = Command::new(env!("CARGO_BIN_EXE_contextful"))
        .args(["mcp", "--project", "research", "--public-key", &public, "--audience", AUD])
        .current_dir(p)
        .env("CONTEXTFUL_TOKEN", &token)
        .env_remove("CONTEXTFUL_ISSUER_PUBKEY")
        .env_remove("CONTEXTFUL_AUDIENCE")
        .env_remove("CONTEXTFUL_TEST_UNSET_PREFIX")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let _ = writeln!(child.stdin.take().unwrap(), "{}", query());
    let out = child.wait_with_output().unwrap();
    let answer: Value = serde_json::from_str(String::from_utf8_lossy(&out.stdout).lines().next().unwrap_or("null")).unwrap();
    assert_eq!(answer["result"]["structuredContent"]["rows"], json!([["n1"]]), "{}", String::from_utf8_lossy(&out.stderr));
}

/// The result cache holds at most the byte budget its process declares, the least recently used entry evicted first; a face declaring no budget caches nothing.
#[test]
fn serve_caches_a_repeated_statement_under_its_declared_budget() {
    let (dir, public) = project_declaring("result_cache = \"10m\"\n");
    let p = dir.path();
    let token = stdout(&run(p, &["token", "mint", "--issuer-key", ".contextful/issuer.seed", "--zone", "on-prem:hq", "--table", "research/*", "--ttl", "900"]));
    let inspected = json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/call", "params": { "name": "context.query", "arguments": { "sql": "SELECT note_id FROM \"research/notes\"", "internals": true } } });
    let base = ["serve", "--http", "127.0.0.1:0", "--audience", AUD, "--max-in-flight", "2", "--project", "research", "--public-key", public.as_str()];
    let states = |extra: &[&str]| {
        let (_listener, addr) = serve(p, &[&base[..], extra].concat());
        (0..2)
            .map(|_| {
                let (status, answer) = post(&addr, &inspected, &token, None);
                assert_eq!(status, 200, "{answer}");
                assert_eq!(answer["result"]["structuredContent"]["rows"], json!([["n1"]]), "{answer}");
                answer["result"]["structuredContent"]["contextful.internals"]["cache"].clone()
            })
            .collect::<Vec<_>>()
    };
    assert_eq!(states(&["--result-cache-bytes", "65536"]), [json!("miss"), json!("hit")]);
    assert_eq!(states(&[]), [Value::Null, Value::Null], "no declared budget caches nothing");
}
