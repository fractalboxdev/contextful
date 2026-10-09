//! `contextful serve --http` through the built binary: startup refusals, then MCP
//! Streamable HTTP admitting a short-lived bearer, and a holder-bound credential minted by
//! `token mint --holder` under a proof on every request.
#![cfg(feature = "data-plane")]

use contextful_core::time::Instant;
use contextful_policy::possession::{jwk_thumbprint, sign_proof, ProofRequest};
use ed25519_dalek::SigningKey;
use hmac::{Hmac, Mac};
use sha2::{Digest, Sha256};
use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::path::Path;
use std::process::{Child, Command, Output, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};

const AUD: &str = "contextful://acme-research";

/// One bounded Content-Length response ends at its body, so a later TCP reset has
/// no bearing on the HTTP answer; a reset or EOF inside the frame remains an error.
fn exchange_response(stream: &mut impl Read) -> std::io::Result<String> {
    let invalid = |message| std::io::Error::new(std::io::ErrorKind::InvalidData, message);
    let mut raw = Vec::new();
    while !raw.ends_with(b"\r\n\r\n") {
        if raw.len() == 8192 { return Err(invalid("the test response head exceeds 8 KiB")); }
        let mut byte = [0];
        stream.read_exact(&mut byte)?;
        raw.push(byte[0]);
    }
    let head = std::str::from_utf8(&raw).map_err(|_| invalid("the response head is not UTF-8"))?;
    let mut lines = head.split("\r\n");
    let mut status = lines.next().unwrap_or_default().split_whitespace();
    if status.next() != Some("HTTP/1.1") ||
        !status.next().is_some_and(|code| code.len() == 3 && code.bytes().all(|b| b.is_ascii_digit())) {
        return Err(invalid("the response status does not parse"));
    }
    let mut length = None;
    for line in lines.filter(|line| !line.is_empty()) {
        let (name, value) = line.split_once(':').ok_or_else(|| invalid("the response header does not parse"))?;
        if name.eq_ignore_ascii_case("Transfer-Encoding") {
            return Err(invalid("the test response requires Content-Length framing"));
        }
        if name.eq_ignore_ascii_case("Content-Length") {
            let value = value.trim();
            if length.is_some() || value.is_empty() || !value.bytes().all(|b| b.is_ascii_digit()) {
                return Err(invalid("the response length is ambiguous or malformed"));
            }
            length = Some(value.parse::<usize>().map_err(|_| invalid("the response length exceeds usize"))?);
        }
    }
    let length = length.ok_or_else(|| invalid("the response has no Content-Length"))?;
    if length > 1024 * 1024 { return Err(invalid("the test response body exceeds 1 MiB")); }
    let start = raw.len();
    raw.resize(start + length, 0);
    stream.read_exact(&mut raw[start..])?;
    String::from_utf8(raw).map_err(|_| invalid("the response is not UTF-8"))
}

struct ResetAfter(std::io::Cursor<Vec<u8>>);

impl Read for ResetAfter {
    fn read(&mut self, out: &mut [u8]) -> std::io::Result<usize> {
        match self.0.read(out)? {
            0 => Err(std::io::ErrorKind::ConnectionReset.into()),
            n => Ok(n),
        }
    }
}

#[test]
fn exchange_response_reads_one_complete_frame_before_a_later_reset() {
    let wire = b"HTTP/1.1 404 Not Found\r\nContent-Length: 2\r\n\r\n{}";
    assert_eq!(exchange_response(&mut &wire[..]).unwrap(), std::str::from_utf8(wire).unwrap());
    assert_eq!(exchange_response(&mut ResetAfter(std::io::Cursor::new(wire.to_vec()))).unwrap(), std::str::from_utf8(wire).unwrap());
}

#[test]
fn exchange_response_refuses_truncated_headers_and_bodies() {
    for wire in [b"HTTP/1.1 404 Not Found\r\nContent-Length: 2\r\n".as_slice(),
        b"HTTP/1.1 404 Not Found\r\nContent-Length: 2\r\n\r\n{".as_slice()] {
        assert!(exchange_response(&mut &wire[..]).is_err(), "EOF inside a frame refuses");
        assert!(exchange_response(&mut ResetAfter(std::io::Cursor::new(wire.to_vec()))).is_err(), "a reset inside a frame refuses");
    }
}

#[test]
fn exchange_response_refuses_ambiguous_or_unbounded_framing() {
    for head in ["not-http\r\nContent-Length: 0", "HTTP/1.1 404 Not Found",
        "HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nContent-Length: 0",
        "HTTP/1.1 404 Not Found\r\nContent-Length: -1",
        "HTTP/1.1 404 Not Found\r\nContent-Length: 1048577",
        "HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nTransfer-Encoding: chunked"] {
        assert!(exchange_response(&mut format!("{head}\r\n\r\n").as_bytes()).is_err(), "{head}");
    }
    assert!(exchange_response(&mut vec![b'x'; 8193].as_slice()).is_err());
}

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
    serve_binary(Path::new(env!("CARGO_BIN_EXE_contextful")), dir, args)
}

fn serve_binary(binary: &Path, dir: &Path, args: &[&str]) -> (Listener, String) {
    let mut child = Command::new(binary)
        .args(args)
        .env("CONTEXTFUL_CONTROL_ATTESTATION_SECRET", "separate-attestation-secret")
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

#[test]
fn control_apply_uses_the_host_registered_task_set() {
    let binary = super::derive::host_binary();
    let (dir, public) = project();
    let root = dir.path();
    std::fs::write(root.join("contextful.toml"), format!("authoring_posture = \"per_request\"\n{}", super::derive::host_manifest("word-split"))).unwrap();
    let imported = Command::new(&binary).args(["pipeline", "import", "--project", "research"]).current_dir(root).output().unwrap();
    stdout(&imported);
    let draft = super::derive::host_manifest("word-split").replace("[pipeline.source]", "schedule = \"every 1h\"\n[pipeline.source]");
    std::fs::write(root.join("contextful.toml"), format!("authoring_posture = \"per_request\"\n{draft}")).unwrap();
    let admin = stdout(&run(root, &["token", "mint", "--issuer-key", ".contextful/issuer.seed", "--on-behalf-of", "user://dana@acme.example", "--zone", "on-prem:hq", "--action", "admin", "--table", "*", "--ttl", "900"]));
    let (_listener, addr) = serve_binary(&binary, root, &["serve", "--http", "127.0.0.1:0", "--audience", AUD, "--max-in-flight", "2", "--project", "research", "--public-key", &public]);
    let edit = json!({ "expected": 1, "document": draft }).to_string();
    let (status, edited) = control(&addr, "POST", "/control/edit", Some(&admin), &edit);
    assert_eq!(status, 200, "{edited}");
    let apply = json!({ "expected": 1, "nonce": edited["nonce"] }).to_string();
    let (status, state) = control(&addr, "POST", "/control/apply", Some(&admin), &apply);
    assert_eq!(status, 200, "{state}");
    assert_eq!(state["applied"], json!(2));
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

fn control(addr: &str, method: &str, path: &str, token: Option<&str>, body: &str) -> (u16, Value) {
    let headers = if method == "POST" && (path == "/control/edit" || path == "/control/apply") {
        let at = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs() as i64;
        let nonce = NONCE.fetch_add(1, Ordering::SeqCst);
        attestation(path, body, "user://dana@acme.example", at, &format!("{nonce:032x}"))
    } else { String::new() };
    control_headers(addr, method, path, token, body, &headers)
}

fn control_headers(addr: &str, method: &str, path: &str, token: Option<&str>, body: &str, headers: &str) -> (u16, Value) {
    let mut stream = TcpStream::connect(addr).unwrap();
    let auth = token.map(|token| format!("Authorization: Bearer {token}\r\n")).unwrap_or_default();
    write!(stream, "{method} {path} HTTP/1.1\r\n{auth}{headers}Content-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}", body.len()).unwrap();
    let mut raw = String::new();
    stream.read_to_string(&mut raw).unwrap();
    let (head, answer) = raw.split_once("\r\n\r\n").unwrap();
    (head.split(' ').nth(1).unwrap().parse().unwrap(), serde_json::from_str(answer).unwrap())
}

fn attestation(path: &str, body: &str, operator: &str, at: i64, nonce: &str) -> String {
    let digest = format!("{:x}", Sha256::digest(body.as_bytes()));
    let mut mac = Hmac::<Sha256>::new_from_slice(b"separate-attestation-secret").unwrap();
    mac.update(format!("POST\n{path}\n{digest}\n{operator}\n{at}\n{nonce}").as_bytes());
    let signature = mac.finalize().into_bytes().iter().map(|byte| format!("{byte:02x}")).collect::<String>();
    format!("X-Contextful-Operator: {operator}\r\nX-Contextful-Operator-Time: {at}\r\nX-Contextful-Operator-Nonce: {nonce}\r\nX-Contextful-Operator-Signature: {signature}\r\n")
}

#[test]
fn pending_control_edit_refuses_revoked_authority_before_nonce_or_draft_commit() {
    pending_control_mutation(false, false);
}

#[test]
fn pending_control_apply_refuses_revoked_authority_before_nonce_or_pointer_commit() {
    pending_control_mutation(true, false);
}

#[test]
fn pending_control_edit_refuses_expired_authority_before_nonce_or_draft_commit() {
    pending_control_mutation(false, true);
}

fn pending_control_mutation(apply: bool, expired: bool) {
    let (dir, public) = project();
    let root = dir.path();
    let original = "[[pipeline]]\nid = \"filings-flow\"\ntables = [\"research/notes\"]\n[pipeline.source]\nname = \"http\"\nconfig = { endpoint = \"https://example.test/filings\" }\n";
    std::fs::create_dir_all(root.join("pipelines")).unwrap();
    std::fs::write(root.join("pipelines/filings.toml"), original).unwrap();
    stdout(&run(root, &["pipeline", "import", "--project", "research"]));
    let (_listener, addr) = serve(root, &["serve", "--http", "127.0.0.1:0", "--audience", AUD, "--max-in-flight", "2", "--project", "research", "--public-key", &public]);
    let admin = stdout(&run(root, &["token", "mint", "--on-behalf-of", "user://dana@acme.example", "--zone", "on-prem:hq", "--action", "admin", "--table", "*", "--ttl", if expired { "2" } else { "900" }]));
    let edit = json!({ "expected": 1, "document": original }).to_string();
    let (path, body) = if apply {
        let (status, saved) = control(&addr, "POST", "/control/edit", Some(&admin), &edit);
        assert_eq!(status, 200, "{saved}");
        ("/control/apply", json!({ "expected": 1, "nonce": saved["nonce"] }).to_string())
    } else { ("/control/edit", edit) };
    let snapshots = root.join(".contextful/control/research");
    let lock = std::fs::OpenOptions::new().read(true).write(true).open(snapshots.join("manifest.lock")).unwrap();
    lock.lock().unwrap();
    let at = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs() as i64;
    let nonce = "f".repeat(32);
    let headers = attestation(path, &body, "user://dana@acme.example", at, &nonce);
    std::thread::scope(|scope| {
        let pending = scope.spawn(|| control_headers(&addr, "POST", path, Some(&admin), &body, &headers));
        std::thread::sleep(std::time::Duration::from_millis(if expired { 3100 } else { 200 }));
        if !expired { stdout(&run(root, &["token", "revoke", "--principal-class", "delegated"])); }
        drop(lock);
        let (status, response) = pending.join().unwrap();
        assert_eq!(status, 401, "{response}");
        assert_eq!(response["error"]["identifier"], if expired { "AuthorityExpired" } else { "AuthorityRevoked" });
    });
    assert!(!snapshots.join(format!("attestation-nonces/{at}/{nonce}")).exists());
    assert_eq!(std::fs::read_to_string(snapshots.join("manifest@current")).unwrap().trim(), "1");
    assert_eq!(snapshots.join("manifest@draft.json").exists(), apply);
}

#[test]
fn admin_changes_require_fresh_console_attestation_and_record_the_operator() {
    let (dir, public) = project();
    let root = dir.path();
    let original = "[[pipeline]]\nid = \"filings-flow\"\nschedule = \"every 1h\"\ntables = [\"research/notes\"]\n[pipeline.source]\nname = \"http\"\nconfig = { endpoint = \"https://example.test/filings\" }\n";
    std::fs::create_dir_all(root.join("pipelines")).unwrap();
    std::fs::write(root.join("pipelines/filings.toml"), original).unwrap();
    stdout(&run(root, &["pipeline", "import", "--project", "research"]));
    let admin = stdout(&run(root, &["token", "mint", "--issuer-key", ".contextful/issuer.seed", "--on-behalf-of", "user://service@acme.example", "--zone", "on-prem:hq", "--action", "admin", "--table", "*", "--ttl", "900"]));
    let (listener, addr) = serve(root, &["serve", "--http", "127.0.0.1:0", "--audience", AUD, "--max-in-flight", "2", "--project", "research", "--public-key", &public]);
    assert_eq!(control(&addr, "GET", "/control/record", Some(&admin), "").0, 200);
    let edit = json!({ "expected": 1, "document": original.replace("every 1h", "every 1d") }).to_string();
    assert_eq!(control_headers(&addr, "POST", "/control/edit", Some(&admin), &edit, "").0, 403);
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs() as i64;
    let stale = attestation("/control/edit", &edit, "operator-a", now - 120, &"1".repeat(32));
    assert_eq!(control_headers(&addr, "POST", "/control/edit", Some(&admin), &edit, &stale).0, 403);
    let forged = attestation("/control/edit", &edit, "operator-a", now, &"2".repeat(32)).replace("Operator: operator-a", "Operator: operator-b");
    assert_eq!(control_headers(&addr, "POST", "/control/edit", Some(&admin), &edit, &forged).0, 403);
    let signed = attestation("/control/edit", &edit, "operator-a", now, &"3".repeat(32));
    let (status, saved) = control_headers(&addr, "POST", "/control/edit", Some(&admin), &edit, &signed);
    assert_eq!(status, 200, "{saved}");
    assert_eq!(control_headers(&addr, "POST", "/control/edit", Some(&admin), &edit, &signed).0, 403);
    drop(listener);
    let (_restart, restarted_addr) = serve(root, &["serve", "--http", "127.0.0.1:0", "--audience", AUD, "--max-in-flight", "2", "--project", "research", "--public-key", &public]);
    assert_eq!(control_headers(&restarted_addr, "POST", "/control/edit", Some(&admin), &edit, &signed).0, 403);
    let apply = json!({ "expected": 1, "nonce": saved["nonce"] }).to_string();
    let signed = attestation("/control/apply", &apply, "operator-a", now, &"4".repeat(32));
    let (status, result) = control_headers(&restarted_addr, "POST", "/control/apply", Some(&admin), &apply, &signed);
    assert_eq!(status, 200, "{result}");
    let entries = contextful_policy::audit::entries(&root.join(".contextful/audit")).unwrap();
    assert!(entries.iter().any(|entry| entry.attributes["contextful.operator.subject"] == "operator-a" && entry.attributes["contextful.control.operation"] == "apply"));
    let (status, record) = control(&restarted_addr, "GET", "/control/record", Some(&admin), "");
    assert_eq!(status, 200, "{record}");
    assert!(record["entries"].as_array().unwrap().iter().any(|entry| entry["attributes"]["contextful.operator.subject"] == "operator-a"));
}

#[test]
fn control_routes_require_admin_and_project_applied_workflows() {
    let (dir, public) = project();
    let root = dir.path();
    let pipeline = "[[pipeline]]\nid = \"filings-flow\"\nschedule = \"every 1h\"\ntables = [\"research/notes\"]\n[pipeline.source]\nname = \"http\"\nconfig = { endpoint = \"https://example.test/filings\" }\n";
    std::fs::create_dir_all(root.join("pipelines")).unwrap();
    std::fs::write(root.join("pipelines/filings.toml"), pipeline).unwrap();
    stdout(&run(root, &["pipeline", "import", "--project", "research"]));
    let read = stdout(&run(root, &["token", "mint", "--issuer-key", ".contextful/issuer.seed", "--on-behalf-of", "user://dana@acme.example", "--zone", "on-prem:hq", "--action", "read", "--table", "*", "--ttl", "900"]));
    let admin = stdout(&run(root, &["token", "mint", "--issuer-key", ".contextful/issuer.seed", "--on-behalf-of", "user://dana@acme.example", "--zone", "on-prem:hq", "--action", "admin", "--table", "*", "--ttl", "900"]));
    let narrow_admin = stdout(&run(root, &["token", "mint", "--issuer-key", ".contextful/issuer.seed", "--on-behalf-of", "user://dana@acme.example", "--zone", "on-prem:hq", "--action", "admin", "--table", "unrelated/*", "--ttl", "900"]));
    let (_listener, addr) = serve(root, &["serve", "--http", "127.0.0.1:0", "--audience", AUD, "--max-in-flight", "2", "--project", "research", "--public-key", &public]);

    assert_eq!(control(&addr, "GET", "/control/workflows", None, "").0, 401);
    assert_eq!(control(&addr, "GET", "/control/record", None, "").0, 401);
    let (status, answer) = control(&addr, "GET", "/control/workflows", Some(&read), "");
    assert_eq!(status, 403, "{answer}");
    assert_eq!(control(&addr, "GET", "/control/record", Some(&read), "").0, 403);
    assert_eq!(control(&addr, "POST", "/control/apply", Some(&read), "{}").0, 403);
    assert_eq!(control(&addr, "GET", "/control/workflows", Some(&narrow_admin), "").0, 403);
    let (status, view) = control(&addr, "GET", "/control/workflows", Some(&admin), "");
    assert_eq!(status, 200, "{view}");
    assert_eq!(view["applied"], json!(1));
    assert_eq!(view["pipelines"][0]["id"], "filings-flow");
    assert_eq!(view["pipelines"][0]["tables"], json!(["research/notes"]));

    assert_eq!(control(&addr, "POST", "/control/apply", Some(&admin), "{\"id\":1}").0, 400);

    let edited = json!({ "expected": 1, "document": pipeline.replace("every 1h", "every 1d") }).to_string();
    let (_, saved) = control(&addr, "POST", "/control/edit", Some(&admin), &edited);
    let apply = json!({ "expected": 1, "nonce": saved["nonce"] }).to_string();
    let (status, applied) = control(&addr, "POST", "/control/apply", Some(&admin), &apply);
    assert_eq!(status, 200, "{applied}");
    assert_eq!(applied["applied"], json!(2));
    let (_, view) = control(&addr, "GET", "/control/workflows", Some(&admin), "");
    assert_eq!(view["pipelines"][0]["schedule"], "every 1d");
}

#[test]
fn control_edit_saves_a_validated_draft_and_apply_claims_its_expected_version() {
    let (dir, public) = project();
    let root = dir.path();
    let original = "[[pipeline]]\nid = \"filings-flow\"\nschedule = \"every 1h\"\ntables = [\"research/notes\"]\n[pipeline.source]\nname = \"http\"\nconfig = { endpoint = \"https://example.test/filings\" }\n";
    std::fs::create_dir_all(root.join("pipelines")).unwrap();
    std::fs::write(root.join("pipelines/filings.toml"), original).unwrap();
    stdout(&run(root, &["pipeline", "import", "--project", "research"]));
    let admin = stdout(&run(root, &["token", "mint", "--issuer-key", ".contextful/issuer.seed", "--on-behalf-of", "user://dana@acme.example", "--zone", "on-prem:hq", "--action", "admin", "--table", "*", "--ttl", "900"]));
    let read = stdout(&run(root, &["token", "mint", "--issuer-key", ".contextful/issuer.seed", "--on-behalf-of", "user://dana@acme.example", "--zone", "on-prem:hq", "--action", "read", "--table", "*", "--ttl", "900"]));
    let (_listener, addr) = serve(root, &["serve", "--http", "127.0.0.1:0", "--audience", AUD, "--max-in-flight", "2", "--project", "research", "--public-key", &public]);
    let changed = original.replace("every 1h", "every 1d");
    let edit = json!({ "expected": 1, "document": changed }).to_string();
    let (status, absent) = control(&addr, "POST", "/control/apply", Some(&admin), &json!({ "expected": 1, "nonce": "000000000000000000000000000000000000000000000000" }).to_string());
    assert_eq!(status, 409, "{absent}");
    assert_eq!(absent["error"]["identifier"], "ControlDraftAbsent");
    assert_eq!(control(&addr, "POST", "/control/edit", None, &edit).0, 401);
    assert_eq!(control(&addr, "POST", "/control/edit", Some(&read), &edit).0, 403);
    let invalid = json!({ "expected": 1, "document": changed.replace("every 1d", "invalid schedule") }).to_string();
    assert_eq!(control(&addr, "POST", "/control/edit", Some(&admin), &invalid).0, 422);
    let (status, draft) = control(&addr, "POST", "/control/edit", Some(&admin), &edit);
    assert_eq!(status, 200, "{draft}");
    assert_eq!(draft["expected"], 1);
    let declaration = root.join("contextful.toml");
    let owner_text = std::fs::read_to_string(&declaration).unwrap();
    std::fs::write(&declaration, format!("{owner_text}\n[control]\nurl = \"http://127.0.0.1:12345/\"\n")).unwrap();
    let apply = json!({ "expected": 1, "nonce": draft["nonce"] }).to_string();
    let (status, refused) = control(&addr, "POST", "/control/apply", Some(&admin), &apply);
    assert_eq!(status, 503, "{refused}");
    assert_eq!(refused["error"]["identifier"], "ConfigOwnerUnconfigured");
    std::fs::write(&declaration, owner_text).unwrap();
    let (_, before) = control(&addr, "GET", "/control/workflows", Some(&admin), "");
    assert_eq!(before["applied"], 1);
    assert_eq!(before["pipelines"][0]["schedule"], "every 1h");
    let (status, applied) = control(&addr, "POST", "/control/apply", Some(&admin), &apply);
    assert_eq!(status, 200, "{applied}");
    assert_eq!(applied["applied"], 2);
    assert_eq!(applied["pipelines"][0]["schedule"], "every 1d");
    let (status, refused) = control(&addr, "POST", "/control/apply", Some(&admin), &apply);
    assert_eq!(status, 409, "{refused}");
    assert_eq!(refused["error"]["identifier"], "ControlDraftAbsent");
    let later = original.replace("every 1h", "every 2d");
    let edit = json!({ "expected": 2, "document": later }).to_string();
    let (_, later_draft) = control(&addr, "POST", "/control/edit", Some(&admin), &edit);
    std::fs::write(root.join("pipelines/filings.toml"), original.replace("every 1h", "every 3d")).unwrap();
    stdout(&run(root, &["pipeline", "apply", "filings-flow", "--project", "research"]));
    let (status, refused) = control(&addr, "POST", "/control/apply", Some(&admin), &json!({ "expected": 2, "nonce": later_draft["nonce"] }).to_string());
    assert_eq!(status, 409, "{refused}");
    assert_eq!(refused["error"]["identifier"], "ManifestVersionConflict");
}

#[test]
fn published_workflow_listing_caps_entries_and_flags_truncation() {
    let (dir, public) = project();
    let root = dir.path();
    let snapshot = contextful_engine::control::SnapshotDir::open(&root.join(".contextful/control/research"));
    let document = (0..1001).map(|index| format!(
        "[[pipeline]]\nid = \"flow-{index:04}\"\ntables = [\"research/notes\"]\n[pipeline.source]\nname = \"http\"\nconfig = {{ endpoint = \"https://example.test/filings\" }}\n"
    )).collect::<Vec<_>>().join("\n");
    snapshot.import(&document).unwrap();
    let admin = stdout(&run(root, &["token", "mint", "--issuer-key", ".contextful/issuer.seed", "--on-behalf-of", "user://dana@acme.example", "--zone", "on-prem:hq", "--action", "admin", "--table", "*", "--ttl", "900"]));
    let (_listener, addr) = serve(root, &["serve", "--http", "127.0.0.1:0", "--audience", AUD, "--max-in-flight", "2", "--project", "research", "--public-key", &public]);
    let (status, view) = control(&addr, "GET", "/control/workflows", Some(&admin), "");
    assert_eq!(status, 200, "{view}");
    assert_eq!(view["pipelines"].as_array().unwrap().len(), 1000);
    assert_eq!(view["truncated"], true);
    assert_eq!(view["declined"], 0);
}

#[test]
fn published_workflows_include_completed_local_runs_before_sync_push() {
    let vendor = super::pipeline::Vendor::start(|_| (200, "[{\"note_id\":\"filed\"}]".into()));
    let (dir, public) = project();
    let root = dir.path();
    let pipeline = format!("[[pipeline]]\nid = \"filings-flow\"\ntables = [\"research/filings\"]\n[pipeline.source]\nname = \"http\"\nconfig = {{ endpoint = \"{}\" }}\n", vendor.url("/filings"));
    std::fs::create_dir_all(root.join("pipelines")).unwrap();
    std::fs::write(root.join("pipelines/filings.toml"), pipeline).unwrap();
    stdout(&run(root, &["pipeline", "import", "--project", "research"]));
    stdout(&run(root, &["pipeline", "run", "filings-flow", "--project", "research", "--run-id", "run-flow", "--site-id", "site-a"]));
    let admin = stdout(&run(root, &["token", "mint", "--issuer-key", ".contextful/issuer.seed", "--on-behalf-of", "user://dana@acme.example", "--zone", "on-prem:hq", "--action", "admin", "--table", "*", "--ttl", "900"]));
    let (_listener, addr) = serve(root, &["serve", "--http", "127.0.0.1:0", "--audience", AUD, "--max-in-flight", "2", "--project", "research", "--public-key", &public]);
    let (status, view) = control(&addr, "GET", "/control/workflows", Some(&admin), "");
    assert_eq!(status, 200, "{view}");
    assert_eq!(view["runs"]["filings-flow"]["run_id"], "run-flow");
    assert_eq!(view["runs"]["filings-flow"]["status"], "success");
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
            "--task", task, "--zone", "on-prem:hq", "--table", "memory/facts", "--table", "research/notes", "--ttl", "900"];
        for action in actions { args.extend(["--action", action]); }
        stdout(&run(p, &args))
    };
    let alice = mint("user://alice", "session-1", &["read", "write"]);
    let bob = mint("user://bob", "session-2", &["read", "write"]);
    let reader = mint("user://alice", "session-1", &["read"]);
    let wrong_table = stdout(&run(p, &["token", "mint", "--issuer-key", ".contextful/issuer.seed", "--on-behalf-of", "user://alice",
        "--task", "session-1", "--zone", "on-prem:hq", "--action", "write", "--table", "research/notes", "--ttl", "900"]));
    let expired = stdout(&run(p, &["token", "mint", "--issuer-key", ".contextful/issuer.seed", "--on-behalf-of", "user://alice",
        "--task", "session-1", "--zone", "on-prem:hq", "--action", "read", "--action", "write",
        "--table", "memory/facts", "--table", "research/notes", "--ttl", "900", "--now", "2020-01-01T00:00:00Z"]));
    let claim = json!({ "into": "memory/facts", "actor": "user://alice", "session": "session-1", "dedup_key": "turn-1",
        "claim": { "subject": "Northwind", "predicate": "filings", "object": "arrived", "confidence": 0.9,
            "evidence": [{ "table": "research/notes", "run": "run-0001", "seq": 0 }] } });
    let (listener, addr) = serve(p, &["serve", "--http", "127.0.0.1:0", "--audience", AUD,
        "--max-in-flight", "2", "--project", "research", "--public-key", &public]);
    assert_eq!(post_claim(&addr, &reader, &claim, false).0, 403, "read grant cannot write");
    assert_eq!(post_claim(&addr, &wrong_table, &claim, false).0, 403, "a write grant on another table cannot land memory");
    assert_eq!(post_claim(&addr, &expired, &claim, false).0, 401, "expired authority cannot write");
    let mut client_scope = claim.clone();
    client_scope["claim"]["scope"] = json!("shared");
    assert_eq!(post_claim(&addr, &alice, &client_scope, false).0, 400, "the caller cannot set claim scope");
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
    let colon = mint("user://alice", "session:1", &["read", "write"]);
    let mut colon_claim = claim.clone();
    colon_claim["session"] = json!("session:1");
    let (status, refused) = post_claim(&addr, &colon, &colon_claim, false);
    assert_eq!((status, refused["error"]["identifier"].clone()), (403, json!("MemoryClaimScopeRefused")),
        "a session holding the scope separator cannot alias another actor's scope");
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
    let raw = exchange_response(&mut stream).unwrap();
    let (head, data) = raw.split_once("\r\n\r\n").unwrap();
    assert_eq!(head.split(' ').nth(1), Some("404"));
    let answer: Value = serde_json::from_str(data).unwrap();
    assert_eq!(answer["error"]["identifier"], "ExchangeUnconfigured");
    let mut stream = TcpStream::connect(&addr).unwrap();
    stream.set_read_timeout(Some(std::time::Duration::from_secs(2))).unwrap();
    write!(stream, "POST /auth/exchange HTTP/1.1\r\nContent-Type: application/json\r\nContent-Length: 100\r\n\r\n").unwrap();
    let raw = exchange_response(&mut stream).expect("unconfigured exchange answers before waiting for its body");
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
        let raw = exchange_response(&mut stream).unwrap();
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

#[test]
fn synced_admin_draft_apply_keeps_a_signed_receipt_under_network_authority() {
    let (dir, public) = project();
    let root = dir.path();
    let original = "[[pipeline]]\nid = \"filings-flow\"\nschedule = \"every 1h\"\ntables = [\"research/notes\"]\n[pipeline.source]\nname = \"http\"\nconfig = { endpoint = \"https://example.test/filings\" }\n";
    std::fs::create_dir_all(root.join("pipelines")).unwrap();
    std::fs::write(root.join("pipelines/filings.toml"), original).unwrap();
    let admin = stdout(&run(root, &["token", "mint", "--issuer-key", ".contextful/issuer.seed", "--on-behalf-of", "user://service@acme.example", "--zone", "on-prem:hq", "--action", "admin", "--table", "*", "--ttl", "900"]));
    let bucket = tempfile::tempdir().unwrap();
    std::fs::write(root.join(".contextful/context/research/config.toml"), format!("[node]\nid = \"site-a\"\n[sync]\nendpoint = \"file://{}\"\nbucket = \"control-test\"\nprefix = \"team\"\ncoordination = \"single-writer\"\n", bucket.path().display())).unwrap();
    let imported = Command::new(env!("CARGO_BIN_EXE_contextful")).args(["pipeline", "import", "--project", "research", "--public-key", &public, "--audience", AUD]).env("CONTEXTFUL_TOKEN", &admin).current_dir(root).output().unwrap();
    stdout(&imported);
    let (listener, addr) = serve(root, &["serve", "--http", "127.0.0.1:0", "--audience", AUD, "--max-in-flight", "2", "--project", "research", "--public-key", &public]);
    assert_eq!(control(&addr, "GET", "/control/record", Some(&admin), "").0, 200);
    let edit = json!({ "expected": 1, "document": original.replace("every 1h", "every 1d") }).to_string();
    assert_eq!(control_headers(&addr, "POST", "/control/edit", Some(&admin), &edit, "").0, 403);
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs() as i64;
    let stale = attestation("/control/edit", &edit, "operator-a", now - 120, &"1".repeat(32));
    assert_eq!(control_headers(&addr, "POST", "/control/edit", Some(&admin), &edit, &stale).0, 403);
    let forged = attestation("/control/edit", &edit, "operator-a", now, &"2".repeat(32)).replace("Operator: operator-a", "Operator: operator-b");
    assert_eq!(control_headers(&addr, "POST", "/control/edit", Some(&admin), &edit, &forged).0, 403);
    let signed = attestation("/control/edit", &edit, "operator-a", now, &"3".repeat(32));
    let (status, saved) = control_headers(&addr, "POST", "/control/edit", Some(&admin), &edit, &signed);
    assert_eq!(status, 200, "{saved}");
    assert_eq!(control_headers(&addr, "POST", "/control/edit", Some(&admin), &edit, &signed).0, 403);
    drop(listener);
    let (_restart, restarted_addr) = serve(root, &["serve", "--http", "127.0.0.1:0", "--audience", AUD, "--max-in-flight", "2", "--project", "research", "--public-key", &public]);
    assert_eq!(control_headers(&restarted_addr, "POST", "/control/edit", Some(&admin), &edit, &signed).0, 403);
    let apply = json!({ "expected": 1, "nonce": saved["nonce"] }).to_string();
    let signed = attestation("/control/apply", &apply, "operator-a", now, &"4".repeat(32));
    let (status, result) = control_headers(&restarted_addr, "POST", "/control/apply", Some(&admin), &apply, &signed);
    assert_eq!(status, 200, "{result}");
    let receipts = root.join(".contextful/control/research");
    let receipt: contextful_policy::control_receipt::ControlReceipt = serde_json::from_str(&std::fs::read_to_string(receipts.join("receipt@v2.json")).unwrap()).unwrap();
    let snapshot = std::fs::read(receipts.join("manifest@v2.toml")).unwrap();
    let signer = contextful_policy::issue::SeedSigner::resolve(Some(&root.join(".contextful/issuer.seed"))).unwrap();
    receipt.verify("research", &snapshot, &[contextful_policy::issue::SignerKey::of(&signer)]).unwrap();
    assert!(receipt.parent.is_some());
    let entries = contextful_policy::audit::entries(&root.join(".contextful/audit")).unwrap();
    assert!(entries.iter().any(|entry| entry.attributes["contextful.operator.subject"] == "operator-a" && entry.attributes["contextful.control.operation"] == "apply"));
    let (status, record) = control(&restarted_addr, "GET", "/control/record", Some(&admin), "");
    assert_eq!(status, 200, "{record}");
    assert!(record["entries"].as_array().unwrap().iter().any(|entry| entry["attributes"]["contextful.operator.subject"] == "operator-a"));
}

#[test]
fn control_editor_refuses_to_discard_applied_jobs() {
    let (dir, public) = project();
    let root = dir.path();
    // A maintenance declaration requires no compiled body, but still belongs to the snapshot.
    let manifest = root.join("contextful.toml");
    let text = std::fs::read_to_string(&manifest).unwrap();
    std::fs::write(&manifest, format!("{text}\n[[job]]\nname = \"nightly-check\"\nkind = \"validate\"\nschedule = \"every 1h\"\n")).unwrap();
    stdout(&run(root, &["pipeline", "import", "--project", "research"]));
    let admin = stdout(&run(root, &["token", "mint", "--issuer-key", ".contextful/issuer.seed", "--on-behalf-of", "user://dana@acme.example", "--zone", "on-prem:hq", "--action", "admin", "--table", "*", "--ttl", "900"]));
    let (_listener, addr) = serve(root, &["serve", "--http", "127.0.0.1:0", "--audience", AUD, "--max-in-flight", "2", "--project", "research", "--public-key", &public]);
    let edit = json!({"expected": 1, "document": "pipeline = []"}).to_string();
    let (status, response) = control(&addr, "POST", "/control/edit", Some(&admin), &edit);
    assert_eq!(status, 422, "{response}");
    assert_eq!(response["error"]["identifier"], "ApplyValidationRefused");
    let snapshot = std::fs::read_to_string(root.join(".contextful/control/research/manifest@v1.toml")).unwrap();
    assert!(snapshot.contains("nightly-check"));
}

fn machine(root: &Path) -> contextful_sqlite::MachineCatalog {
    let clock = std::sync::Arc::new(contextful_core::ports::FixedClock(Instant::parse("2030-01-01T00:00:00Z").unwrap()));
    contextful_sqlite::MachineCatalog::open(&root.join(".contextful/context/research/machine.sqlite"), clock).unwrap()
}

/// Record `run_id` of `pipeline` in `status` in the store's machine catalog.
fn record_run(root: &Path, run_id: &str, pipeline: &str, status: &str) {
    use contextful_core::coordinate::Catalog;
    let row: contextful_core::run::record::RunRow = serde_json::from_value(json!({
        "run_id": run_id, "pipeline_id": pipeline, "table": "research/notes", "site_id": "site-a", "status": status,
        "started_at": "2030-01-01T00:00:00Z", "rows": 0, "bytes": 0, "batches": 0,
        "connector_id": "vendor", "connector_version": "1", "connector_hash": "h", "phase": "plan", "execution_id": format!("x-{run_id}"),
    })).unwrap();
    machine(root).put_run(&row).unwrap();
}

fn stop_mark(root: &Path, run_id: &str) -> Option<contextful_core::run::record::StopMark> {
    use contextful_core::coordinate::Catalog;
    machine(root).run(run_id).unwrap().expect("the run row").stop
}

fn mint(root: &Path, action: &str, table: &str) -> String {
    stdout(&run(root, &["token", "mint", "--issuer-key", ".contextful/issuer.seed", "--on-behalf-of", "user://dana@acme.example", "--zone", "on-prem:hq", "--action", action, "--table", table, "--ttl", "900"]))
}

/// A websocket upgrade to `path`, writing `frame` after the request; the answer's head and the bytes after it.
fn upgrade(addr: &str, path: &str, token: Option<&str>, frame: &[u8]) -> (String, Vec<u8>) {
    let mut stream = TcpStream::connect(addr).unwrap();
    let auth = token.map(|token| format!("Authorization: Bearer {token}\r\n")).unwrap_or_default();
    write!(stream, "GET {path} HTTP/1.1\r\nHost: localhost\r\n{auth}Upgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Version: 13\r\nSec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\n\r\n").unwrap();
    let _ = stream.write_all(frame);
    let mut raw = Vec::new();
    stream.read_to_end(&mut raw).unwrap();
    let split = raw.windows(4).position(|w| w == b"\r\n\r\n").unwrap();
    (String::from_utf8_lossy(&raw[..split]).into_owned(), raw[split + 4..].to_vec())
}

/// The payload of the first server frame in `bytes`, which is unmasked.
fn first_frame(bytes: &[u8]) -> &[u8] {
    match bytes[1] & 0x7f {
        126 => &bytes[4..4 + usize::from(u16::from_be_bytes([bytes[2], bytes[3]]))],
        127 => panic!("a snapshot past 64 KiB"),
        n => &bytes[2..2 + usize::from(n)],
    }
}

/// `POST /runs/:run_id/stop` writes a stop under a per-request network credential, its optional JSON body carrying
/// `scope` and `reason`, and answers `200` with the marked run ids.
// spec: run.cancel.stop-route@6e91ad11
#[test]
fn the_served_stop_route_marks_the_run_and_answers_its_ids() {
    let (dir, public) = project();
    let root = dir.path();
    record_run(root, "live-a", "feed-a", "running");
    record_run(root, "live-a2", "feed-a", "waiting");
    record_run(root, "live-b", "feed-b", "running");
    let execute = mint(root, "execute", "feed-a");
    let (_listener, addr) = serve(root, &["serve", "--http", "127.0.0.1:0", "--audience", AUD, "--max-in-flight", "2", "--project", "research", "--public-key", &public]);

    assert_eq!(control(&addr, "POST", "/runs/live-a/stop", None, "").0, 401);
    let (status, answer) = control(&addr, "POST", "/runs/live-a/stop", Some(&execute), &json!({ "reason": "ops" }).to_string());
    assert_eq!(status, 200, "{answer}");
    assert_eq!(answer, json!({ "stopped": ["live-a"] }));
    let mark = stop_mark(root, "live-a").unwrap();
    assert_eq!((mark.scope.as_str(), mark.reason.as_deref()), ("run", Some("ops")));
    assert!(stop_mark(root, "live-a2").is_none(), "a run-scoped stop marks one run");
    let (status, answer) = control(&addr, "POST", "/runs/live-a2/stop", Some(&execute), &json!({ "scope": "pipeline" }).to_string());
    assert_eq!(status, 200, "{answer}");
    let mut stopped: Vec<String> = serde_json::from_value(answer["stopped"].clone()).unwrap();
    stopped.sort();
    assert_eq!(stopped, ["live-a", "live-a2"]);
    assert!(stop_mark(root, "live-b").is_none(), "a pipeline stop halts only its own pipeline");
    assert_eq!(control(&addr, "POST", "/runs/live-a/stop", Some(&execute), &json!({ "scope": "fire" }).to_string()).0, 400);
    assert_eq!(control(&addr, "POST", "/runs/live-a/stop", Some(&execute), "[1]").0, 400);
}

/// Only a `pending`, `running` or `waiting` row accepts a mark; a stop matching none raises
/// `CancelTargetNotInFlight`, answering `409` over HTTP and exiting non-zero at the terminal.
// spec: run.cancel.not-in-flight@285fe285
#[test]
fn a_stop_on_a_finished_run_answers_409_and_exits_non_zero() {
    let (dir, public) = project();
    let root = dir.path();
    record_run(root, "done-a", "feed-a", "success");
    let execute = mint(root, "execute", "feed-a");
    let (_listener, addr) = serve(root, &["serve", "--http", "127.0.0.1:0", "--audience", AUD, "--max-in-flight", "2", "--project", "research", "--public-key", &public]);
    let (status, answer) = control(&addr, "POST", "/runs/done-a/stop", Some(&execute), "");
    assert_eq!(status, 409, "{answer}");
    assert_eq!(answer["error"]["identifier"], "CancelTargetNotInFlight");
    assert!(stop_mark(root, "done-a").is_none());
    let out = run(root, &["run", "cancel", "done-a", "--project", "research"]);
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("CancelTargetNotInFlight"));
}

/// A stop authorizes against the pipeline read off the run record, never off the request.
// spec: run.cancel.authority-from-the-record@959bdd65
#[test]
fn a_served_stop_authorizes_against_the_recorded_pipeline_not_the_request() {
    let (dir, public) = project();
    let root = dir.path();
    record_run(root, "live-b", "feed-b", "running");
    let execute = mint(root, "execute", "feed-a");
    let (_listener, addr) = serve(root, &["serve", "--http", "127.0.0.1:0", "--audience", AUD, "--max-in-flight", "2", "--project", "research", "--public-key", &public]);
    // A body naming a covered pipeline confers nothing over the pipeline the record holds.
    let (status, answer) = control(&addr, "POST", "/runs/live-b/stop", Some(&execute), &json!({ "pipeline": "feed-a" }).to_string());
    assert_eq!(status, 403, "{answer}");
    assert_eq!(answer["error"]["identifier"], "CancelUnauthorized");
    assert!(!answer.to_string().contains("feed-b"), "{answer}");
    assert!(stop_mark(root, "live-b").is_none());
    let (status, absent) = control(&addr, "POST", "/runs/zz/stop", Some(&execute), "");
    assert_eq!((status, &absent), (403, &answer), "an unrecorded run refuses as an uncovered one");
}

/// A subscription is read-only; a stop travels as an authenticated route.
// spec: run.project.read-only-socket@1a1d5f50
#[test]
fn the_served_run_stream_carries_the_snapshot_and_takes_no_stop() {
    let (dir, public) = project();
    let root = dir.path();
    record_run(root, "live-a", "feed-a", "running");
    let reader = mint(root, "read", "feed-a");
    let other = mint(root, "read", "feed-b");
    let (_listener, addr) = serve(root, &["serve", "--http", "127.0.0.1:0", "--audience", AUD, "--max-in-flight", "2", "--project", "research", "--public-key", &public]);

    let (head, body) = upgrade(&addr, "/runs/live-a/stream", None, b"");
    assert!(head.starts_with("HTTP/1.1 401"), "{head}");
    assert!(String::from_utf8_lossy(&body).contains("RunStreamUnauthorized"));
    assert!(upgrade(&addr, "/runs/zz/stream", Some(&reader), b"").0.starts_with("HTTP/1.1 404"));
    assert!(upgrade(&addr, "/runs/live-a/stream", Some(&other), b"").0.starts_with("HTTP/1.1 404"), "an unreadable run reads as unseen");

    // A masked client text frame asking for a stop: the socket reads nothing from its caller.
    let ask = br#"{"stop":"live-a"}"#;
    let mut frame = vec![0x81, 0x80 | ask.len() as u8, 1, 2, 3, 4];
    frame.extend(ask.iter().enumerate().map(|(i, b)| b ^ [1, 2, 3, 4][i % 4]));
    let (head, frames) = upgrade(&addr, "/runs/live-a/stream", Some(&reader), &frame);
    assert!(head.starts_with("HTTP/1.1 101"), "{head}");
    assert_eq!(frames[0], 0x81, "a text frame");
    assert!(frames.ends_with(&[0x88, 0x02, 0x03, 0xE8]), "then a normal close");
    let wire: Value = serde_json::from_slice(first_frame(&frames)).unwrap();
    assert_eq!((wire["run_id"].as_str(), wire["workflow_id"].as_str(), wire["status"].as_str()), (Some("live-a"), Some("feed-a"), Some("running")));
    assert!(stop_mark(root, "live-a").is_none(), "the subscription wrote no stop");
    // The stop travels as the authenticated route.
    let (status, _) = control(&addr, "POST", "/runs/live-a/stop", Some(&mint(root, "execute", "feed-a")), "");
    assert_eq!(status, 200);
    assert!(stop_mark(root, "live-a").is_some());
}
