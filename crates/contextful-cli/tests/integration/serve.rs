//! `contextful serve --http` through the built binary: startup refusals, then MCP
//! Streamable HTTP admitting a short-lived bearer, and a holder-bound credential minted by
//! `token mint --holder` under a proof on every request.
#![cfg(feature = "data-plane")]

use contextful_agent::http::Hydration;
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

pub(super) const AUD: &str = "contextful://acme-research";

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

pub(super) fn run(dir: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_contextful"))
        .args(args)
        .current_dir(dir)
        .env_remove("CONTEXTFUL_ISSUER_PUBKEY")
        .env_remove("CONTEXTFUL_AUDIENCE")
        .output()
        .unwrap()
}

pub(super) fn stdout(out: &Output) -> String {
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
pub(super) struct Listener(Child);

impl Drop for Listener {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// Start the face; its address, reported on standard error.
pub(super) fn serve(dir: &Path, args: &[&str]) -> (Listener, String) {
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
            // The face binds warming and answers reads once its snapshot set is mapped.
            let addr = addr.to_string();
            line.clear();
            while err.read_line(&mut line).unwrap() > 0 {
                if line.starts_with("hydrated:") {
                    ready(&addr);
                    return (listener, addr);
                }
                line.clear();
            }
            panic!("the face never hydrated");
        }
        line.clear();
    }
    panic!("the face never listened")
}

/// Poll `GET /health` until the face reports its hydration complete: the `hydrated:` line
/// can reach standard error before the face leaves warming.
fn ready(addr: &str) {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
    loop {
        let (status, health) = control(addr, "GET", "/health", None, "");
        if status == 200 && health["hydration"] == "complete" {
            return;
        }
        assert!(std::time::Instant::now() < deadline, "the face never left warming: {status} {health}");
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
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

/// A served face over an imported store, minting write credentials from its issuer seed.
struct ConnectionFace {
    dir: tempfile::TempDir,
    _listener: Listener,
    addr: String,
}

impl ConnectionFace {
    fn start() -> ConnectionFace {
        let (dir, public) = project();
        let root = dir.path();
        stdout(&run(root, &["pipeline", "import", "--project", "research"]));
        let (_listener, addr) = serve(root, &["serve", "--http", "127.0.0.1:0", "--audience", AUD,
            "--max-in-flight", "2", "--project", "research", "--public-key", &public]);
        ConnectionFace { dir, _listener, addr }
    }

    /// A write credential for `tenant` acting on behalf of `principal`.
    fn mint(&self, tenant: &str, principal: &str) -> String {
        stdout(&run(self.dir.path(), &["token", "mint", "--issuer-key", ".contextful/issuer.seed",
            "--on-behalf-of", principal, "--zone", "on-prem:hq",
            "--action", "write", "--table", "research/notes", "--tenant", &format!("research/notes={tenant}"), "--ttl", "900"]))
    }

    fn call(&self, method: &str, path: &str, token: &str, body: &str) -> (u16, Value) {
        control(&self.addr, method, path, Some(token), body)
    }
}

const DANA: &str = "user://dana@acme.example";

fn declaration(id: &str, tenant: &str) -> Value {
    json!({"id": id, "tenant": tenant, "principal": DANA, "file_ids": ["doc-plan"],
        "refresh_secret": "drive-refresh-dana", "schedule": "every 1h"})
}

fn refused(answer: &(u16, Value), status: u16, identifier: &str) {
    assert_eq!(answer.0, status, "{}", answer.1);
    assert_eq!(answer.1["error"]["identifier"], identifier, "{}", answer.1);
}

#[test]
fn tenant_connection_routes_create_pause_and_delete_once() {
    let face = ConnectionFace::start();
    let tenant = face.mint("tenant-a", DANA);
    let create = declaration("conn-a", "tenant-a").to_string();
    let (status, made) = face.call("POST", "/connections", &tenant, &create);
    assert_eq!(status, 201, "{made}");
    assert_eq!(made["id"], "conn-a");
    assert_eq!((made["version"].clone(), made["state"].clone()), (json!(1), json!("active")), "{made}");
    assert_eq!((made["tenant"].clone(), made["principal"].clone(), made["refresh_secret"].clone()), (json!("tenant-a"), json!(DANA), json!("drive-refresh-dana")));
    assert_eq!(made["pipeline"], "connection-conn-a", "{made}");
    let (status, repeated) = face.call("POST", "/connections", &tenant, &create);
    assert_eq!(status, 200, "{repeated}");
    assert_eq!(repeated["version"], made["version"]);
    let wrong = create.replace("tenant-a", "tenant-b");
    refused(&face.call("POST", "/connections", &tenant, &wrong), 403, "ConnectionScopeDenied");
    let changed = create.replace("doc-plan", "doc-other");
    refused(&face.call("POST", "/connections", &tenant, &changed), 409, "ConnectionIdConflict");
    let (status, paused) = face.call("POST", "/connections/conn-a/pause", &tenant, "{}");
    assert_eq!(status, 200, "{paused}");
    assert_eq!((paused["version"].clone(), paused["state"].clone()), (json!(2), json!("paused")), "{paused}");
    let (status, again) = face.call("POST", "/connections/conn-a/pause", &tenant, "{}");
    assert_eq!((status, again["version"].clone()), (200, json!(2)), "a repeated pause claims nothing: {again}");
    let (status, deleted) = face.call("DELETE", "/connections/conn-a", &tenant, "{}");
    assert_eq!(status, 200, "{deleted}");
    assert_eq!((deleted["version"].clone(), deleted["state"].clone()), (json!(3), json!("deleted")), "{deleted}");
    let (status, _) = face.call("GET", "/connections", &tenant, "");
    assert_eq!(status, 405);
}

/// Create answers `201` with the claimed connection, version and state; a repeat, pause or delete answers `200`; each
/// claim advances the version by one, deleting an active connection claims twice, and a repeat claims nothing.
// spec: surface.connection.answer@b9a0a1ce
#[test]
fn each_connection_claim_advances_one_version_and_a_repeat_claims_none() {
    let face = ConnectionFace::start();
    let dana = face.mint("tenant-a", DANA);
    let create = declaration("conn-a", "tenant-a").to_string();
    let (status, made) = face.call("POST", "/connections", &dana, &create);
    assert_eq!((status, made["version"].clone(), made["state"].clone()), (201, json!(1), json!("active")), "{made}");
    let (status, repeated) = face.call("POST", "/connections", &dana, &create);
    assert_eq!((status, repeated["version"].clone()), (200, json!(1)), "a repeated create claims nothing: {repeated}");
    // Deleting an active connection claims twice: the pause, then the retirement.
    let (status, deleted) = face.call("DELETE", "/connections/conn-a", &dana, "{}");
    assert_eq!((status, deleted["version"].clone(), deleted["state"].clone()), (200, json!(3), json!("deleted")), "{deleted}");

    assert_eq!(face.call("POST", "/connections", &dana, &declaration("conn-b", "tenant-a").to_string()).0, 201);
    let (status, paused) = face.call("POST", "/connections/conn-b/pause", &dana, "{}");
    assert_eq!((status, paused["version"].clone(), paused["state"].clone()), (200, json!(2), json!("paused")), "{paused}");
    let (status, again) = face.call("POST", "/connections/conn-b/pause", &dana, "{}");
    assert_eq!((status, again["version"].clone()), (200, json!(2)), "a repeated pause claims nothing: {again}");
    let (status, retired) = face.call("DELETE", "/connections/conn-b", &dana, "{}");
    assert_eq!((status, retired["version"].clone()), (200, json!(3)), "a paused connection retires in one claim: {retired}");
}

/// A pause or delete whose path id lies outside the connection id grammar names no connection.
#[test]
fn a_pause_or_delete_naming_an_id_outside_the_grammar_is_unknown() {
    let face = ConnectionFace::start();
    let dana = face.mint("tenant-a", DANA);
    let long = "x".repeat(65);
    for id in ["a.b", long.as_str()] {
        refused(&face.call("POST", &format!("/connections/{id}/pause"), &dana, "{}"), 404, "ConnectionUnknown");
        refused(&face.call("DELETE", &format!("/connections/{id}"), &dana, "{}"), 404, "ConnectionUnknown");
    }
}

/// A connection mutation requires an admitted write credential for its tenant and principal, checked on the request
/// before reading and on the standing record before changing it; a mismatch raises `ConnectionScopeDenied`.
// spec: surface.connection.authority@43697862
#[test]
fn a_connection_outside_the_credentials_tenant_or_principal_is_denied() {
    let face = ConnectionFace::start();
    let dana = face.mint("tenant-a", DANA);
    let (status, made) = face.call("POST", "/connections", &dana, &declaration("conn-a", "tenant-a").to_string());
    assert_eq!(status, 201, "{made}");

    let mut other_principal = declaration("conn-b", "tenant-a");
    other_principal["principal"] = json!("user://erin@acme.example");
    refused(&face.call("POST", "/connections", &dana, &other_principal.to_string()), 403, "ConnectionScopeDenied");
    refused(&face.call("POST", "/connections", &dana, &declaration("conn-b", "tenant-b").to_string()), 403, "ConnectionScopeDenied");

    // Another tenant's credential names `conn-a`: create, pause and delete refuse, changing no version.
    let other = face.mint("tenant-b", DANA);
    refused(&face.call("POST", "/connections", &other, &declaration("conn-a", "tenant-b").to_string()), 403, "ConnectionScopeDenied");
    refused(&face.call("POST", "/connections/conn-a/pause", &other, "{}"), 403, "ConnectionScopeDenied");
    refused(&face.call("DELETE", "/connections/conn-a", &other, "{}"), 403, "ConnectionScopeDenied");
    let erin = face.mint("tenant-a", "user://erin@acme.example");
    refused(&face.call("POST", "/connections/conn-a/pause", &erin, "{}"), 403, "ConnectionScopeDenied");
    let (status, standing) = face.call("POST", "/connections", &dana, &declaration("conn-a", "tenant-a").to_string());
    assert_eq!((status, standing["version"].clone(), standing["state"].clone()), (200, json!(1), json!("active")), "{standing}");
    let (status, absent) = face.call("POST", "/connections/conn-b/pause", &dana, "{}");
    assert_eq!(status, 404, "a denied create claimed nothing: {absent}");
}

/// Repeating create with the same connection id and declaration returns the standing version; a differing
/// declaration raises `ConnectionIdConflict` without replacing it.
// spec: surface.connection.idempotent-create@90a2f2d8
#[test]
fn a_differing_declaration_under_one_connection_id_conflicts_and_keeps_the_standing_one() {
    let face = ConnectionFace::start();
    let dana = face.mint("tenant-a", DANA);
    let create = declaration("conn-a", "tenant-a");
    assert_eq!(face.call("POST", "/connections", &dana, &create.to_string()).0, 201);
    for (key, value) in [("file_ids", json!(["doc-plan", "doc-other"])), ("refresh_secret", json!("drive-refresh-other")), ("schedule", json!("every 2h"))] {
        let mut changed = create.clone();
        changed[key] = value;
        refused(&face.call("POST", "/connections", &dana, &changed.to_string()), 409, "ConnectionIdConflict");
    }
    let (status, standing) = face.call("POST", "/connections", &dana, &create.to_string());
    assert_eq!((status, standing["version"].clone()), (200, json!(1)), "{standing}");
    assert_eq!(standing["file_ids"], json!(["doc-plan"]), "{standing}");
}

/// A create body outside `id`, `tenant`, `principal`, `file_ids`, `refresh_secret` and `schedule`, an unreadable
/// schedule, or a pause or delete body other than `{}` raises `ConnectionRequestMalformed` and claims nothing.
// spec: surface.connection.malformed@54ab3015
#[test]
fn a_connection_body_outside_its_fields_or_carrying_an_oauth_code_is_malformed() {
    let face = ConnectionFace::start();
    let dana = face.mint("tenant-a", DANA);
    let with = |key: &str, value: Value| {
        let mut body = declaration("conn-a", "tenant-a");
        body[key] = value;
        body.to_string()
    };
    let mut missing = declaration("conn-a", "tenant-a");
    missing.as_object_mut().unwrap().remove("file_ids");
    for body in [with("code", json!("4/0Adeu5BW-oauth-code")), with("refresh_secret", json!("${secret://drive-refresh-dana}")),
        with("schedule", json!("every fortnight")), with("file_ids", json!([])), with("id", json!("../conn")),
        missing.to_string(), "[]".into(), "not json".into()] {
        refused(&face.call("POST", "/connections", &dana, &body), 400, "ConnectionRequestMalformed");
    }
    assert_eq!(face.call("POST", "/connections/conn-a/pause", &dana, "{}").0, 404, "no malformed create claimed a version");

    assert_eq!(face.call("POST", "/connections", &dana, &declaration("conn-a", "tenant-a").to_string()).0, 201);
    refused(&face.call("POST", "/connections/conn-a/pause", &dana, r#"{"now":true}"#), 400, "ConnectionRequestMalformed");
    refused(&face.call("DELETE", "/connections/conn-a", &dana, r#"{"force":true}"#), 400, "ConnectionRequestMalformed");
    let (status, standing) = face.call("POST", "/connections", &dana, &declaration("conn-a", "tenant-a").to_string());
    assert_eq!((status, standing["version"].clone(), standing["state"].clone()), (200, json!(1), json!("active")), "{standing}");
}

/// A pause or delete naming no standing connection raises `ConnectionUnknown`; a deleted id stays bound to its retired
/// record, and create refuses its reuse as {{surface.connection.idempotent-create}}.
// spec: surface.connection.unknown@4d133ef3
#[test]
fn a_pause_or_delete_naming_no_standing_connection_is_unknown_and_a_deleted_id_stays_bound() {
    let face = ConnectionFace::start();
    let dana = face.mint("tenant-a", DANA);
    refused(&face.call("POST", "/connections/conn-a/pause", &dana, "{}"), 404, "ConnectionUnknown");
    refused(&face.call("DELETE", "/connections/conn-a", &dana, "{}"), 404, "ConnectionUnknown");

    let create = declaration("conn-a", "tenant-a").to_string();
    assert_eq!(face.call("POST", "/connections", &dana, &create).0, 201);
    let (status, deleted) = face.call("DELETE", "/connections/conn-a", &dana, "{}");
    assert_eq!((status, deleted["version"].clone(), deleted["state"].clone()), (200, json!(3), json!("deleted")), "delete pauses, then retires: {deleted}");
    refused(&face.call("POST", "/connections/conn-a/pause", &dana, "{}"), 404, "ConnectionUnknown");
    refused(&face.call("DELETE", "/connections/conn-a", &dana, "{}"), 404, "ConnectionUnknown");
    refused(&face.call("POST", "/connections", &dana, &create), 409, "ConnectionIdConflict");
}

/// A sink asserting, on each write, that the face it reports on has left warming.
struct AfterFinish<'a>(&'a Hydration);

impl Write for AfterFinish<'_> {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        assert!(self.0.is_complete(), "`{}` was written while the face still reported warming", String::from_utf8_lossy(buf).trim());
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// The `hydrated:` line reaches its reader only once the face answers reads, so a client
/// that waits on the line never meets `503` while warming.
#[test]
fn the_hydrated_line_is_written_after_the_face_leaves_warming() {
    let hydration = Hydration::warming();
    contextful_cli::announce_hydrated(&hydration, 3, 4096, &mut AfterFinish(&hydration));
    assert!(hydration.is_complete());
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

pub(super) fn control(addr: &str, method: &str, path: &str, token: Option<&str>, body: &str) -> (u16, Value) {
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
    let snapshot_path = root.join(".contextful/control/research/manifest@v1.toml");
    let original_snapshot: toml::Value = toml::from_str(&std::fs::read_to_string(&snapshot_path).unwrap()).unwrap();
    let draft_path = root.join(".contextful/control/research/manifest@draft.json");
    let saved_bytes = std::fs::read(&draft_path).unwrap();
    let mut saved: serde_json::Value = serde_json::from_slice(&saved_bytes).unwrap();
    let mut saved_document: toml::Value = toml::from_str(saved["document"].as_str().unwrap()).unwrap();
    assert_eq!(saved_document["standalone_tables"], original_snapshot["standalone_tables"], "pipeline editing retains the applied table contracts");
    saved_document["standalone_tables"] = toml::Value::Array(vec![]);
    saved["document"] = json!(toml::to_string(&saved_document).unwrap());
    std::fs::write(&draft_path, serde_json::to_vec(&saved).unwrap()).unwrap();
    let (status, refused) = control(&addr, "POST", "/control/apply", Some(&admin), &json!({ "expected": 1, "nonce": draft["nonce"] }).to_string());
    assert_eq!(status, 422, "tampering with preserved table contracts refuses: {refused}");
    std::fs::write(&draft_path, saved_bytes).unwrap();
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
    let next: toml::Value = toml::from_str(&std::fs::read_to_string(root.join(".contextful/control/research/manifest@v2.toml")).unwrap()).unwrap();
    assert_eq!(next["standalone_tables"], original_snapshot["standalone_tables"]);
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
    let snapshot = contextful_snapshot::SnapshotDir::open(&root.join(".contextful/control/research"));
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

// spec: surface.open-console.source-list@f6c315e3
// spec: surface.open-console.source-validation@f0724b97
// spec: surface.open-console.source-first-run@ddcddca4
#[test]
fn admin_source_drafts_validate_and_observe_only_runs_of_the_claimed_version() {
    let vendor = super::pipeline::Vendor::start(|path| if path.contains("failed") { (500, "unavailable".into()) } else { (200, "[{\"id\":\"one\"}]".into()) });
    let (dir, public) = project();
    let root = dir.path();
    stdout(&run(root, &["pipeline", "import", "--project", "research"]));
    let admin = stdout(&run(root, &["token", "mint", "--issuer-key", ".contextful/issuer.seed", "--on-behalf-of", "user://dana@acme.example", "--zone", "on-prem:hq", "--action", "admin", "--table", "*", "--ttl", "900"]));
    let read = stdout(&run(root, &["token", "mint", "--issuer-key", ".contextful/issuer.seed", "--on-behalf-of", "user://dana@acme.example", "--zone", "on-prem:hq", "--action", "read", "--table", "*", "--ttl", "900"]));
    let (_listener, addr) = serve(root, &["serve", "--http", "127.0.0.1:0", "--audience", AUD, "--max-in-flight", "2", "--project", "research", "--public-key", &public]);
    let source = json!({ "id": "inbox", "tables": ["items"], "schedule": "every 1h", "source": { "name": "http", "config": { "endpoint": vendor.url("/items") } } });
    let edit = |expected, source: Value| json!({ "expected": expected, "source": source }).to_string();
    assert_eq!(control(&addr, "POST", "/control/edit", Some(&read), &edit(1, source.clone())).0, 403);
    for (value, field) in [
        (json!({ "id": "inbox", "tables": ["items"], "source": { "name": "missing", "config": {} } }), "source.name"),
        (json!({ "id": "inbox", "tables": ["items"], "source": { "name": "http", "config": {} } }), "source.config.endpoint"),
        (json!({ "id": "inbox", "tables": [], "source": { "name": "http", "config": { "endpoint": vendor.url("/items") } } }), "tables"),
        (json!({ "id": "inbox", "tables": ["items", "items"], "source": { "name": "http", "config": { "endpoint": vendor.url("/items") } } }), "tables"),
        (json!({ "id": "inbox", "tables": ["items"], "source": { "name": "http", "config": { "endpoint": vendor.url("/items"), "headers": null } } }), "source.config"),
        (json!({ "id": "inbox", "tables": ["items"], "destination": { "name": "remote" }, "source": { "name": "http", "config": { "endpoint": vendor.url("/items") } } }), "destination"),
        (json!({ "id": "inbox", "tables": ["items"], "schedule": "invalid", "source": { "name": "http", "config": { "endpoint": vendor.url("/items") } } }), "schedule"),
    ] {
        let (status, refusal) = control(&addr, "POST", "/control/edit", Some(&admin), &edit(1, value));
        assert_eq!(status, 422, "{refusal}");
        assert_eq!(refusal["error"]["field"], field, "{refusal}");
        assert!(refusal["error"]["message"].as_str().is_some_and(|message| !message.is_empty()), "{refusal}");
        let (_, current) = control(&addr, "GET", "/control/workflows", Some(&admin), "");
        assert_eq!(current["applied"], 1);
    }
    let (status, draft) = control(&addr, "POST", "/control/edit", Some(&admin), &edit(1, source.clone()));
    assert_eq!(status, 200, "{draft}");
    let (status, view) = control(&addr, "POST", "/control/apply", Some(&admin), &json!({ "expected": 1, "nonce": draft["nonce"] }).to_string());
    assert_eq!(status, 200, "{view}");
    assert_eq!(view["sources"][0]["version"], 2);
    assert_eq!(view["sources"][0]["connector"], "http");
    assert_eq!(view["sources"][0]["destinations"], json!(["inbox_items"]));
    assert_eq!(view["sources"][0]["first_run"], Value::Null);
    assert_eq!(view["sources"][0]["state"], "pending");
    let (status, refused) = control(&addr, "POST", "/control/edit", Some(&admin), &edit(2, source));
    assert_eq!(status, 422, "{refused}");
    assert_eq!(refused["error"]["field"], "id");
    stdout(&run(root, &["pipeline", "run", "inbox", "--applied", "2", "--project", "research", "--run-id", "first", "--site-id", "site-a"]));
    let (_, view) = control(&addr, "GET", "/control/workflows", Some(&admin), "");
    assert_eq!(view["sources"][0]["first_run"]["status"], "success", "{view}");
    assert_eq!(view["sources"][0]["first_run"]["rows"], 1);
    stdout(&run(root, &["pipeline", "run", "inbox", "--applied", "2", "--project", "research", "--run-id", "second", "--site-id", "site-a"]));
    let (_, later) = control(&addr, "GET", "/control/workflows", Some(&admin), "");
    assert_eq!(later["sources"][0]["first_run"]["run_id"], "first");
    assert_eq!(later["sources"][0]["latest_run"]["run_id"], "second");
    let document = view["document"].as_str().unwrap();
    let (_, draft) = control(&addr, "POST", "/control/edit", Some(&admin), &json!({ "expected": 2, "document": document }).to_string());
    let (status, view) = control(&addr, "POST", "/control/apply", Some(&admin), &json!({ "expected": 2, "nonce": draft["nonce"] }).to_string());
    assert_eq!(status, 200, "{view}");
    assert_eq!(view["sources"][0]["first_run"], Value::Null, "an identical claim has no run evidence for its version");
    assert_eq!(view["sources"][0]["state"], "pending");
    stdout(&run(root, &["pipeline", "run", "inbox", "--applied", "2", "--project", "research", "--run-id", "old-version", "--site-id", "site-a"]));
    let (_, view) = control(&addr, "GET", "/control/workflows", Some(&admin), "");
    assert_eq!(view["sources"][0]["first_run"], Value::Null);
    stdout(&run(root, &["pipeline", "run", "inbox", "--applied", "3", "--project", "research", "--run-id", "current-version", "--site-id", "site-a"]));
    let (_, view) = control(&addr, "GET", "/control/workflows", Some(&admin), "");
    assert_eq!(view["sources"][0]["first_run"]["run_id"], "current-version");
    let (_, draft) = control(&addr, "POST", "/control/edit", Some(&admin), &json!({ "expected": 3, "document": document.replace("/items", "/failed") }).to_string());
    let (status, view) = control(&addr, "POST", "/control/apply", Some(&admin), &json!({ "expected": 3, "nonce": draft["nonce"] }).to_string());
    assert_eq!(status, 200, "{view}");
    assert!(!run(root, &["pipeline", "run", "inbox", "--applied", "4", "--project", "research", "--run-id", "failed-version", "--site-id", "site-a"]).status.success());
    let (_, view) = control(&addr, "GET", "/control/workflows", Some(&admin), "");
    assert_eq!(view["sources"][0]["first_run"]["status"], "failed", "{view}");
    assert!(view["sources"][0]["first_run"]["error_kind"].is_string());
    let split = json!({ "id": "split", "tables": ["items", "failed"], "source": { "name": "http", "config": { "endpoint": vendor.url("/{table}") } } });
    let (status, draft) = control(&addr, "POST", "/control/edit", Some(&admin), &edit(4, split));
    assert_eq!(status, 200, "{draft}");
    let (status, view) = control(&addr, "POST", "/control/apply", Some(&admin), &json!({ "expected": 4, "nonce": draft["nonce"] }).to_string());
    assert_eq!(status, 200, "{view}");
    assert!(!run(root, &["pipeline", "run", "split", "--applied", "5", "--project", "research", "--run-id", "split-first", "--site-id", "site-a"]).status.success());
    let (_, view) = control(&addr, "GET", "/control/workflows", Some(&admin), "");
    assert_eq!(view["sources"][1]["first_run"]["status"], "failed", "a source fire includes every destination: {view}");
    assert_eq!(view["sources"][1]["first_run"]["rows"], 1);
    assert_eq!(view["sources"][1]["latest_run"]["run_id"], "split-first");
}

#[test]
#[cfg(feature = "component-host")]
fn admin_source_drafts_check_component_artifacts_and_exported_tables() {
    let (dir, public) = project();
    let root = dir.path();
    stdout(&run(root, &["pipeline", "import", "--project", "research"]));
    let admin = stdout(&run(root, &["token", "mint", "--issuer-key", ".contextful/issuer.seed", "--on-behalf-of", "user://dana@acme.example", "--zone", "on-prem:hq", "--action", "admin", "--table", "*", "--ttl", "900"]));
    let (_listener, addr) = serve(root, &["serve", "--http", "127.0.0.1:0", "--audience", AUD, "--max-in-flight", "2", "--project", "research", "--public-key", &public]);
    std::fs::write(root.join("connector.toml"), "name = \"probe\"\n[capabilities]\nallow_hosts = [\"127.0.0.1\"]\nclock = true\n").unwrap();
    let source = json!({ "id": "guest", "tables": ["items"], "source": { "name": "probe.wasm", "config": { "manifest": "connector.toml" } } });
    let edit = |source: Value| json!({ "expected": 1, "source": source }).to_string();
    let (status, refusal) = control(&addr, "POST", "/control/edit", Some(&admin), &edit(source.clone()));
    assert_eq!(status, 422, "{refusal}");
    assert_eq!(refusal["error"]["field"], "source");
    std::fs::copy(concat!(env!("CARGO_MANIFEST_DIR"), "/../contextful-wasm/tests/fixtures/probe.wasm"), root.join("probe.wasm")).unwrap();
    let mut invalid = source.clone();
    invalid["tables"] = json!(["absent"]);
    let (status, refusal) = control(&addr, "POST", "/control/edit", Some(&admin), &edit(invalid));
    assert_eq!(status, 422, "{refusal}");
    assert_eq!(refusal["error"]["field"], "tables");
    let (status, draft) = control(&addr, "POST", "/control/edit", Some(&admin), &edit(source));
    assert_eq!(status, 200, "{draft}");
    std::fs::remove_file(root.join("probe.wasm")).unwrap();
    let apply = json!({ "expected": 1, "nonce": draft["nonce"] }).to_string();
    let (status, refusal) = control(&addr, "POST", "/control/apply", Some(&admin), &apply);
    assert_eq!(status, 422, "{refusal}");
    let (_, view) = control(&addr, "GET", "/control/workflows", Some(&admin), "");
    assert_eq!(view["applied"], 1);
    std::fs::copy(concat!(env!("CARGO_MANIFEST_DIR"), "/../contextful-wasm/tests/fixtures/probe.wasm"), root.join("probe.wasm")).unwrap();
    let (status, view) = control(&addr, "POST", "/control/apply", Some(&admin), &apply);
    assert_eq!(status, 200, "{view}");
    assert_eq!(view["sources"][0]["state"], "pending");
    stdout(&run(root, &["pipeline", "run", "guest", "--applied", "2", "--project", "research", "--run-id", "guest-first", "--site-id", "site-a"]));
    let (_, view) = control(&addr, "GET", "/control/workflows", Some(&admin), "");
    assert_eq!(view["sources"][0]["first_run"]["status"], "success", "{view}");
    assert_eq!(view["sources"][0]["first_run"]["rows"], 3);
}

// spec: surface.open-console.source-preservation@3f369635
#[test]
fn admin_source_addition_preserves_other_work_and_refuses_incompatible_destination_keys() {
    let (dir, public) = project();
    let root = dir.path();
    let original = "[[pipeline]]\nid = \"parent\"\ntables = [\"items\"]\n[pipeline.source]\nname = \"http\"\nconfig = { endpoint = \"https://example.test/items\" }\n\n[[external]]\nname = \"review\"\ninputs = [\"pipeline:parent\"]\n";
    let snapshots = contextful_snapshot::SnapshotDir::open(&root.join(".contextful/control/research"));
    snapshots.import(original).unwrap();
    std::fs::write(root.join("incompatible.jsonl"), "{\"id\":1.5}\n").unwrap();
    stdout(&run(root, &["context", "land", "inbox_items", "--project", "research", "--rows", "incompatible.jsonl", "--run-id", "incompatible", "--site-id", "site-a"]));
    let admin = stdout(&run(root, &["token", "mint", "--issuer-key", ".contextful/issuer.seed", "--on-behalf-of", "user://dana@acme.example", "--zone", "on-prem:hq", "--action", "admin", "--table", "*", "--ttl", "900"]));
    let (_listener, addr) = serve(root, &["serve", "--http", "127.0.0.1:0", "--audience", AUD, "--max-in-flight", "2", "--project", "research", "--public-key", &public]);
    let mut source = json!({ "id": "inbox", "tables": [{ "name": "items", "primary_key": ["id"] }], "source": { "name": "http", "config": { "endpoint": "https://example.test/items" } } });
    let edit = |source: Value| json!({ "expected": 1, "source": source }).to_string();
    let (status, refusal) = control(&addr, "POST", "/control/edit", Some(&admin), &edit(source.clone()));
    assert_eq!(status, 422, "{refusal}");
    assert_eq!(refusal["error"]["field"], "tables");
    assert_eq!(snapshots.current().unwrap(), Some(1));
    source["tables"] = json!(["different"]);
    let (status, draft) = control(&addr, "POST", "/control/edit", Some(&admin), &edit(source));
    assert_eq!(status, 200, "{draft}");
    let (status, view) = control(&addr, "POST", "/control/apply", Some(&admin), &json!({ "expected": 1, "nonce": draft["nonce"] }).to_string());
    assert_eq!(status, 200, "{view}");
    assert_eq!(view["sources"].as_array().unwrap().len(), 2);
    assert_eq!(view["externals"][0]["name"], "review");
}

// spec: run.land.receive-applied@a7f67bf7
#[test]
fn source_first_receive_fire_uses_the_admin_applied_document() {
    let (dir, public) = project();
    let root = dir.path();
    let declaration = root.join("contextful.toml");
    let text = std::fs::read_to_string(&declaration).unwrap();
    std::fs::write(&declaration, format!("site_id = 'site-a'\n{text}")).unwrap();
    stdout(&run(root, &["pipeline", "import", "--project", "research"]));
    let admin = stdout(&run(root, &["token", "mint", "--issuer-key", ".contextful/issuer.seed", "--on-behalf-of", "user://dana@acme.example", "--zone", "on-prem:hq", "--action", "admin", "--table", "*", "--ttl", "900"]));
    let write = mint_write(root, "received_events");
    let (_listener, addr) = serve(root, &["serve", "--http", "127.0.0.1:0", "--audience", AUD, "--max-in-flight", "2", "--project", "research", "--public-key", &public]);
    let source = json!({ "id": "received", "tables": ["events"], "source": { "name": "receive", "config": { "format": "jsonl", "max_batch_bytes": 512, "max_queue_depth": 2, "key_retention_secs": 3600 } } });
    let (status, draft) = control(&addr, "POST", "/control/edit", Some(&admin), &json!({ "expected": 1, "source": source }).to_string());
    assert_eq!(status, 200, "{draft}");
    let (status, view) = control(&addr, "POST", "/control/apply", Some(&admin), &json!({ "expected": 1, "nonce": draft["nonce"] }).to_string());
    assert_eq!(status, 200, "{view}");
    let body = delivered("events", json!({ "id": "one" }));
    let (status, _, answer) = deliver_to(&addr, "received", Some(&write), Some("first-batch"), body.len(), &body);
    assert_eq!(status, 200, "{answer}");
    let (_, view) = control(&addr, "GET", "/control/workflows", Some(&admin), "");
    assert_eq!(view["sources"][0]["version"], 2);
    assert_eq!(view["sources"][0]["first_run"]["rows"], 1, "{view}");
    assert_eq!(view["sources"][0]["state"], "success");
    std::fs::create_dir_all(root.join("pipelines")).unwrap();
    std::fs::write(root.join("pipelines/unapplied.toml"), "[[pipeline]]\nid = 'unapplied'\ntables = ['events']\n[pipeline.source]\nname = 'receive'\nconfig = { format = 'jsonl', max_batch_bytes = 512, max_queue_depth = 2, key_retention_secs = 3600 }\n").unwrap();
    assert_eq!(deliver_to(&addr, "unapplied", Some(&write), Some("unapplied-batch"), body.len(), &body).0, 404);
    let (_, draft) = control(&addr, "POST", "/control/edit", Some(&admin), &json!({ "expected": 2, "document": "" }).to_string());
    let (status, view) = control(&addr, "POST", "/control/apply", Some(&admin), &json!({ "expected": 2, "nonce": draft["nonce"] }).to_string());
    assert_eq!(status, 200, "{view}");
    assert_eq!(deliver_to(&addr, "received", Some(&write), Some("removed-batch"), body.len(), &body).0, 404);
}

#[test]
fn source_outcomes_require_every_owned_destination_row_to_survive() {
    let (dir, public) = project();
    let root = dir.path();
    let document = "[[pipeline]]\nid = 'inbox'\ntables = ['items', 'archive']\n[pipeline.source]\nname = 'http'\nconfig = { endpoint = 'https://example.test/items' }\n";
    contextful_snapshot::SnapshotDir::open(&root.join(".contextful/control/research")).import(document).unwrap();
    let spec = contextful_core::pipeline::declare::collect(&[contextful_core::pipeline::declare::ManifestFile { path: "source.toml".into(), text: document.into() }]).unwrap().remove(0).spec;
    record_run(root, "surviving", "inbox", "success");
    record_run(root, "other-source", "another", "success");
    let audit = contextful_policy::audit::AuditLog::unanchored(root.join(".contextful/audit")).unwrap();
    for (fire, absent) in [("collected-destination", "missing"), ("foreign-destination", "other-source")] {
        audit.append(json!({ "contextful.control.version": 1, "contextful.pipeline.id": "inbox", "contextful.pipeline.hash": spec.content_hash(),
            "contextful.run.id": fire, "contextful.run.rows": ["surviving", absent], "contextful.run.failed": false })).unwrap();
    }
    let admin = stdout(&run(root, &["token", "mint", "--issuer-key", ".contextful/issuer.seed", "--on-behalf-of", "user://dana@acme.example", "--zone", "on-prem:hq", "--action", "admin", "--table", "*", "--ttl", "900"]));
    let (_listener, addr) = serve(root, &["serve", "--http", "127.0.0.1:0", "--audience", AUD, "--max-in-flight", "2", "--project", "research", "--public-key", &public]);
    let (status, view) = control(&addr, "GET", "/control/workflows", Some(&admin), "");
    assert_eq!(status, 200, "{view}");
    assert_eq!(view["sources"][0]["first_run"], Value::Null, "a surviving destination is not proof of the whole source fire: {view}");
    assert_eq!(view["sources"][0]["state"], "pending");
}

#[test]
fn source_first_and_latest_use_completion_order_for_a_retried_fire() {
    let (dir, public) = project();
    let root = dir.path();
    let document = "[[pipeline]]\nid = 'inbox'\ntables = ['items']\n[pipeline.source]\nname = 'http'\nconfig = { endpoint = 'https://example.test/items' }\n";
    contextful_snapshot::SnapshotDir::open(&root.join(".contextful/control/research")).import(document).unwrap();
    let spec = contextful_core::pipeline::declare::collect(&[contextful_core::pipeline::declare::ManifestFile { path: "source.toml".into(), text: document.into() }]).unwrap().remove(0).spec;
    let first = json!({ "run_id": "retried", "status": "failed", "rows": 0, "started_at": "2030-01-01T00:00:00Z", "ended_at": "2030-01-01T00:00:01Z", "error_kind": "Transient" });
    let latest = json!({ "run_id": "retried", "status": "success", "rows": 1, "started_at": "2030-01-01T00:00:00Z", "ended_at": "2030-01-01T00:00:02Z" });
    for (node, outcome) in [("a", first.clone()), ("b", latest)] {
        let path = root.join(format!(".contextful/context/research/nodes/{node}/run-state.json"));
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, serde_json::to_vec(&json!({ "format": 1, "node_id": node, "runs": {}, "cursors": [], "sources": [{ "pipeline_id": "inbox", "version": 1, "config_hash": spec.content_hash(), "first": first, "latest": outcome }] })).unwrap()).unwrap();
    }
    let admin = stdout(&run(root, &["token", "mint", "--issuer-key", ".contextful/issuer.seed", "--on-behalf-of", "user://dana@acme.example", "--zone", "on-prem:hq", "--action", "admin", "--table", "*", "--ttl", "900"]));
    let (_listener, addr) = serve(root, &["serve", "--http", "127.0.0.1:0", "--audience", AUD, "--max-in-flight", "2", "--project", "research", "--public-key", &public]);
    let (_, view) = control(&addr, "GET", "/control/workflows", Some(&admin), "");
    assert_eq!(view["sources"][0]["first_run"]["status"], "failed", "{view}");
    assert_eq!(view["sources"][0]["latest_run"]["status"], "success");
    assert_eq!(view["sources"][0]["latest_run"]["rows"], 1);
}

// spec: run.land.receive-version-group@89e4b98d
#[test]
fn applied_receive_groups_leave_an_older_versions_orphan_batch_unclaimed() {
    let (dir, public) = receiving_face();
    let root = dir.path();
    stdout(&run(root, &["pipeline", "import", "--project", "research"]));
    let admin = stdout(&run(root, &["token", "mint", "--issuer-key", ".contextful/issuer.seed", "--on-behalf-of", "user://dana@acme.example", "--zone", "on-prem:hq", "--action", "admin", "--table", "*", "--ttl", "900"]));
    let write = mint_write(root, "*");
    let args = ["serve", "--http", "127.0.0.1:0", "--audience", AUD, "--max-in-flight", "2", "--project", "research", "--public-key", &public];
    let (mut listener, addr) = serve(root, &args);
    let (_, view) = control(&addr, "GET", "/control/workflows", Some(&admin), "");
    let document = view["document"].as_str().unwrap();
    let queue = root.join(".contextful/run/research/receive/otel");
    std::fs::create_dir_all(&queue).unwrap();
    let held = std::fs::File::options().read(true).write(true).create(true).truncate(false).open(queue.join("land.lock")).unwrap();
    held.lock().unwrap();
    let old_body = delivered("spans", json!({ "id": "old" }));
    let mut waiting = TcpStream::connect(&addr).unwrap();
    write!(waiting, "POST /receive/otel HTTP/1.1\r\nAuthorization: Bearer {write}\r\nContextful-Batch-Key: older-version\r\nContent-Type: application/x-ndjson\r\nContent-Length: {}\r\n\r\n{old_body}", old_body.len()).unwrap();
    let spool = queue.join("spool");
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(15);
    loop {
        let paths = std::fs::read_dir(&spool).map(|entries| entries.flatten().map(|entry| entry.path()).collect::<Vec<_>>()).unwrap_or_default();
        let spooled = paths.iter().any(|path| path.extension().is_some_and(|ext| ext == "batch") ||
            (path.is_dir() && std::fs::read_dir(path).is_ok_and(|entries| entries.flatten().any(|entry| entry.path().extension().is_some_and(|ext| ext == "batch")))));
        if spooled { break; }
        assert!(std::time::Instant::now() < deadline, "the older version's request did not spool");
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    let (_, draft) = control(&addr, "POST", "/control/edit", Some(&admin), &json!({ "expected": 1, "document": document }).to_string());
    let (status, view) = control(&addr, "POST", "/control/apply", Some(&admin), &json!({ "expected": 1, "nonce": draft["nonce"] }).to_string());
    assert_eq!(status, 200, "{view}");
    listener.0.kill().unwrap();
    listener.0.wait().unwrap();
    drop(waiting);
    drop(held);
    let (_listener, addr) = serve(root, &args);
    let body = delivered("spans", json!({ "id": "current" }));
    let (status, _, answer) = deliver_to(&addr, "otel", Some(&write), Some("current-version"), body.len(), &body);
    assert_eq!(status, 200, "{answer}");
    let (_, view) = control(&addr, "GET", "/control/workflows", Some(&admin), "");
    assert_eq!(view["sources"][0]["version"], 2);
    assert_eq!(view["sources"][0]["first_run"]["rows"], 1, "an orphan decoded under another version stays outside this fire: {view}");
}

/// Three pipelines, two models, a build and a store-driven job, a publication and two
/// external steps around them.
const GRAPH_PIPELINES: &str = "[[pipeline]]\nid = \"ingest\"\nschedule = \"every 1h\"\ntables = [\"records\"]\n[pipeline.source]\nname = \"http\"\nconfig = { endpoint = \"https://example.test/ingest\" }\n\n\
[[pipeline]]\nid = \"normalize\"\nafter = \"ingest\"\ntables = [\"records\"]\n[pipeline.source]\nname = \"http\"\nconfig = { endpoint = \"https://example.test/normalize\" }\n\n\
[[pipeline]]\nid = \"enrich\"\nschedule = \"every 1d\"\ntables = [\"records\"]\n[pipeline.source]\nname = \"http\"\nconfig = { endpoint = \"https://example.test/enrich\" }\n";

const GRAPH_WORK: &str = "[[model]]\nid = \"daily_summary\"\nsql = \"SELECT id FROM normalize_records\"\nunique_key = [\"id\"]\npublish = false\n\n\
[[model]]\nid = \"weekly_rollup\"\nsql = \"SELECT id FROM daily_summary\"\nunique_key = [\"id\"]\npublish = false\n\n\
[[job]]\nname = \"summarize\"\nkind = \"build\"\ntarget = \"daily_summary\"\nafter = \"normalize\"\n\n\
[[job]]\nname = \"score\"\nkind = \"store-driven\"\nbody = { delegate = \"scorer\" }\nstatement = \"SELECT id FROM enrich_records\"\nmax_in_flight = 2\ntables = [\"scores\"]\n\n\
[[export]]\nname = \"scores-copy\"\ntable = \"scores\"\nendpoint = \"http://127.0.0.1:1/events\"\nformat = \"changes-v1\"\nkey = [\"id\"]\nschedule = \"every 30s\"\n\n\
[[publication]]\nname = \"scores-feed\"\njob = \"score\"\nmembers = [\"scores-copy\"]\n\n\
[[external]]\nname = \"review\"\nlabel = \"Analyst review\"\ninputs = [\"table:normalize_records\"]\noutputs = [\"pipeline:enrich\"]\n\n\
[[external]]\nname = \"dashboard\"\nlabel = \"Team dashboard\"\ninputs = [\"table:weekly_rollup\", \"job:score\"]\n";

// spec: surface.visualize.workflow-canvas@71481a6d
// spec: surface.visualize.dataflow-edges@1ee8d398
/// The published workflow lists every pipeline, table, model, job, publication and external
/// step as a node and joins them by an explicit edge list.
#[test]
fn published_workflow_is_a_typed_node_and_edge_graph_with_external_steps() {
    let (dir, public) = project();
    let root = dir.path();
    std::fs::create_dir_all(root.join("pipelines")).unwrap();
    std::fs::write(root.join("pipelines/flow.toml"), GRAPH_PIPELINES).unwrap();
    let declaration = root.join("contextful.toml");
    let owner = std::fs::read_to_string(&declaration).unwrap();
    std::fs::write(&declaration, format!("{owner}\n{GRAPH_WORK}")).unwrap();
    stdout(&run(root, &["pipeline", "import", "--project", "research"]));
    let snapshot: toml::Value = toml::from_str(&std::fs::read_to_string(root.join(".contextful/control/research/manifest@v1.toml")).unwrap()).unwrap();
    assert_eq!(snapshot["external"].as_array().unwrap().len(), 2, "import retains the external steps");
    let admin = stdout(&run(root, &["token", "mint", "--issuer-key", ".contextful/issuer.seed", "--on-behalf-of", "user://dana@acme.example", "--zone", "on-prem:hq", "--action", "admin", "--table", "*", "--ttl", "900"]));
    let (_listener, addr) = serve(root, &["serve", "--http", "127.0.0.1:0", "--audience", AUD, "--max-in-flight", "2", "--project", "research", "--public-key", &public]);
    let (status, view) = control(&addr, "GET", "/control/workflows", Some(&admin), "");
    assert_eq!(status, 200, "{view}");

    let names = |key: &str, field: &str| -> Vec<String> {
        view[key].as_array().unwrap_or_else(|| panic!("{key}: {view}")).iter().map(|entry| entry[field].as_str().unwrap().to_string()).collect()
    };
    let mut nodes: Vec<String> = Vec::new();
    nodes.extend(names("pipelines", "id").into_iter().map(|n| format!("pipeline:{n}")));
    nodes.extend(view["tables"].as_array().unwrap().iter().map(|n| format!("table:{}", n.as_str().unwrap())));
    nodes.extend(names("models", "id").into_iter().map(|n| format!("model:{n}")));
    nodes.extend(names("jobs", "name").into_iter().map(|n| format!("job:{n}")));
    nodes.extend(names("publications", "name").into_iter().map(|n| format!("publication:{n}")));
    nodes.extend(names("externals", "name").into_iter().map(|n| format!("external:{n}")));
    nodes.sort();
    assert_eq!(nodes, [
        "external:dashboard", "external:review", "job:score", "job:summarize",
        "model:daily_summary", "model:weekly_rollup", "pipeline:enrich", "pipeline:ingest", "pipeline:normalize",
        "publication:scores-feed", "table:enrich_records", "table:ingest_records", "table:normalize_records", "table:scores",
    ]);
    assert_eq!(view["externals"][0]["label"], "Analyst review");

    let mut edges: Vec<(String, String, String)> = view["edges"].as_array().unwrap().iter()
        .map(|e| (e["from"].as_str().unwrap().into(), e["to"].as_str().unwrap().into(), e["via"].as_str().unwrap().into()))
        .collect();
    edges.sort();
    let expected: Vec<(String, String, String)> = [
        ("external:review", "pipeline:enrich", "output"),
        ("job:score", "external:dashboard", "input"),
        ("job:score", "publication:scores-feed", "publishes"),
        ("job:score", "table:scores", "writes"),
        ("job:summarize", "model:daily_summary", "builds"),
        ("model:daily_summary", "model:weekly_rollup", "reads"),
        ("model:weekly_rollup", "external:dashboard", "input"),
        ("pipeline:enrich", "table:enrich_records", "lands"),
        ("pipeline:ingest", "pipeline:normalize", "after"),
        ("pipeline:ingest", "table:ingest_records", "lands"),
        ("pipeline:normalize", "job:summarize", "after"),
        ("pipeline:normalize", "table:normalize_records", "lands"),
        ("table:enrich_records", "job:score", "reads"),
        ("table:normalize_records", "external:review", "input"),
        ("table:normalize_records", "model:daily_summary", "reads"),
    ].into_iter().map(|(a, b, c)| (a.to_string(), b.to_string(), c.to_string())).collect();
    assert_eq!(edges, expected);
    for edge in &edges {
        assert!(nodes.contains(&edge.0) && nodes.contains(&edge.1), "{edge:?} joins two published nodes");
    }
    assert_eq!(view["runs"], json!({}), "nothing has run, and the engine records no run for an external step");
    let external_keys: Vec<&String> = view["externals"][1].as_object().unwrap().keys().collect();
    assert_eq!(external_keys, ["inputs", "label", "name", "outputs"], "an external step carries no kind or schedule");
}

// spec: surface.apply.external-snapshot@07ad058d
/// A pipeline-only apply keeps the snapshot's external steps, and the pipeline editor
/// refuses a store whose snapshot carries them.
#[test]
fn a_pipeline_only_apply_preserves_external_steps_and_the_editor_refuses_them() {
    let (dir, public) = project();
    let root = dir.path();
    std::fs::create_dir_all(root.join("pipelines")).unwrap();
    std::fs::write(root.join("pipelines/flow.toml"), GRAPH_PIPELINES).unwrap();
    let declaration = root.join("contextful.toml");
    let owner = std::fs::read_to_string(&declaration).unwrap();
    std::fs::write(&declaration, format!("{owner}\n{GRAPH_WORK}")).unwrap();
    stdout(&run(root, &["pipeline", "import", "--project", "research"]));
    std::fs::write(root.join("pipelines/flow.toml"), GRAPH_PIPELINES.replace("every 1d", "every 2d")).unwrap();
    std::fs::write(&declaration, format!("{owner}\n{}", GRAPH_WORK.split("[[external]]").next().unwrap())).unwrap();
    stdout(&run(root, &["pipeline", "apply", "enrich", "--project", "research"]));
    let snapshot: toml::Value = toml::from_str(&std::fs::read_to_string(root.join(".contextful/control/research/manifest@v2.toml")).unwrap()).unwrap();
    assert_eq!(snapshot["external"].as_array().unwrap().len(), 2, "a pipeline-only apply preserves the external steps");
    let admin = stdout(&run(root, &["token", "mint", "--issuer-key", ".contextful/issuer.seed", "--on-behalf-of", "user://dana@acme.example", "--zone", "on-prem:hq", "--action", "admin", "--table", "*", "--ttl", "900"]));
    let (_listener, addr) = serve(root, &["serve", "--http", "127.0.0.1:0", "--audience", AUD, "--max-in-flight", "2", "--project", "research", "--public-key", &public]);
    let edit = json!({ "expected": 2, "document": GRAPH_PIPELINES }).to_string();
    let (status, refused) = control(&addr, "POST", "/control/edit", Some(&admin), &edit);
    assert_eq!(status, 422, "{refused}");
    assert_eq!(refused["error"]["identifier"], "ApplyValidationRefused");
    // A full apply from declarations without external steps drops them.
    stdout(&run(root, &["pipeline", "apply", "--project", "research"]));
    let snapshot: toml::Value = toml::from_str(&std::fs::read_to_string(root.join(".contextful/control/research/manifest@v3.toml")).unwrap()).unwrap();
    assert!(snapshot.get("external").is_none());
}

/// Import refuses an external step naming undeclared work or sharing a step's name, and
/// claims no version.
#[test]
fn import_refuses_an_unbound_or_colliding_external_step() {
    for (external, identifier) in [
        ("[[external]]\nname = \"review\"\ninputs = [\"table:missing_records\"]\n", "ExternalStepUnbound"),
        ("[[external]]\nname = \"review\"\noutputs = [\"pipeline:normalize_records\"]\n", "ExternalStepUnbound"),
        ("[[external]]\nname = \"score\"\n", "ExternalStepCollision"),
        ("[[external]]\nname = \"ingest\"\n", "ExternalStepCollision"),
        ("[[external]]\nname = \"review\"\ncommand = \"make review\"\n", "ApplyValidationRefused"),
    ] {
        let (dir, _) = project();
        let root = dir.path();
        std::fs::create_dir_all(root.join("pipelines")).unwrap();
        std::fs::write(root.join("pipelines/flow.toml"), GRAPH_PIPELINES).unwrap();
        let declaration = root.join("contextful.toml");
        let owner = std::fs::read_to_string(&declaration).unwrap();
        let work = GRAPH_WORK.split("[[external]]").next().unwrap();
        std::fs::write(&declaration, format!("{owner}\n{work}\n{external}")).unwrap();
        let out = run(root, &["pipeline", "import", "--project", "research"]);
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(!out.status.success() && stderr.contains(identifier), "{external}: {stderr}");
        assert!(!root.join(".contextful/control/research/manifest@v1.toml").exists());
    }
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
    let mut shaped = claim.clone();
    shaped["claim"]["object"] = json!("AKIAIOSFODNN7EXAMPLE");
    let (status, refused) = post_claim(&addr, &alice, &shaped, false);
    assert_eq!((status, refused["error"]["identifier"].clone()), (400, json!("EnforceCredentialShapedValue")), "{refused}");
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

/// Introspection runs on the command line alone, as `contextful token introspect`; no serving face answers an introspection request.
// spec: authority.profile.introspection-surface@0e3623e6
#[test]
fn introspection_answers_on_the_command_line_and_on_no_served_route() {
    let (dir, public) = project();
    let p = dir.path();
    let token = mint(p, "read", "research/*");
    let card: Value = serde_json::from_str(&stdout(&run(p, &["token", "introspect", "--token", &token]))).unwrap();
    assert_eq!(card["authority"]["grants"][0]["tables"], json!(["research/*"]), "{card}");
    let (_listener, addr) = serve(p, &["serve", "--http", "127.0.0.1:0", "--audience", AUD,
        "--max-in-flight", "2", "--project", "research", "--public-key", &public]);
    let body = json!({ "token": token }).to_string();
    for path in ["/auth/introspect", "/introspect", "/oauth/introspect"] {
        let (status, answer) = control_headers(&addr, "POST", path, Some(&token), &body, "");
        assert_eq!(status, 404, "{path}: {answer}");
    }
    let listing = json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/list" });
    let (status, tools) = post(&addr, &listing, &token, None);
    assert_eq!(status, 200, "{tools}");
    let names: Vec<&str> = tools["result"]["tools"].as_array().unwrap().iter().map(|t| t["name"].as_str().unwrap()).collect();
    assert!(!names.is_empty(), "{tools}");
    assert!(!names.iter().any(|n| n.contains("introspect")), "{names:?}");
}

/// A face configured with no issuer key raises `IssuerKeyMissing`, prints the command that fixes it, and binds no port.
// spec: authority.issue.missing-key@23a8173f
#[test]
fn a_minting_face_with_no_issuer_key_names_the_keygen_command_and_binds_nothing() {
    let (dir, public) = project();
    let p = dir.path();
    std::fs::create_dir_all(p.join(".contextful/exchange")).unwrap();
    std::fs::write(p.join(".contextful/exchange/policy.toml"),
        "expected_iss = \"https://login.example.test/\"\nrole_claim = \"roles\"\n[[role_grants.reader]]\nactions = [\"read\"]\ntables = [\"research/*\"]\n").unwrap();
    std::fs::write(p.join(".contextful/exchange/verify.key"), "exchange-secret").unwrap();
    std::fs::remove_file(p.join(".contextful/issuer.seed")).unwrap();
    let out = run(p, &["serve", "--http", "127.0.0.1:0", "--audience", AUD, "--max-in-flight", "2", "--project", "research", "--public-key", &public]);
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(!out.status.success(), "{err}");
    assert!(err.contains("IssuerKeyMissing") && err.contains("contextful token keygen"), "{err}");
    assert!(!err.contains("listening on"), "{err}");
    assert!(!p.join(".contextful/issuer.seed").exists(), "no key is fabricated");
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
// spec: topology.publish-hostname.issuer-key@9b2ac860
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

/// The project's awakeable registry over its run directory, the one `serve` resolves through.
fn registry(root: &Path) -> contextful_engine::awake::Registry {
    let dir = root.join(".contextful/run/research");
    contextful_engine::awake::Registry::open(&dir, contextful_engine::Journal::open(&dir))
}

/// A credential of `subject`, admitted for reads of every table.
fn mint_for(root: &Path, subject: &str) -> String {
    stdout(&run(root, &["token", "mint", "--issuer-key", ".contextful/issuer.seed", "--on-behalf-of", subject, "--zone", "on-prem:hq", "--action", "read", "--table", "*", "--ttl", "900"]))
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

/// A subscription permits reads only. Stop requests use an authenticated route.
// spec: run.project.read-only-socket@193da0d1
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

/// `POST /awake/:token` on the served face answers `200` with the recorded payload, `404` for an unknown token, `409`
/// for a conflicting resolution and `410` past the deadline; `GET /awake/:token` reports state without resuming.
// spec: run.suspend.resume-route@4da5f97c
#[test]
fn the_served_resume_route_answers_the_recorded_payload_and_each_refusal() {
    use contextful_engine::awake::Awaited;
    let (dir, public) = project();
    let root = dir.path();
    let pending = registry(root).suspend("x-approval", "approval", "2030-01-01T00:00:00Z", 3600).unwrap();
    let expired = registry(root).suspend("x-approval", "late", "2020-01-01T00:00:00Z", 60).unwrap();
    let dana = mint_for(root, "user://dana@acme.example");
    let (_listener, addr) = serve(root, &["serve", "--http", "127.0.0.1:0", "--audience", AUD, "--max-in-flight", "2", "--project", "research", "--public-key", &public]);
    let path = format!("/awake/{}", pending.token);
    let payload = json!({ "approved": true }).to_string();

    assert_eq!(control(&addr, "POST", &path, None, &payload).0, 401, "a resume admits a credential first");
    let (status, state) = control(&addr, "GET", &path, Some(&dana), "");
    assert_eq!((status, state["state"].as_str()), (200, Some("pending")), "{state}");
    let (status, recorded) = control(&addr, "POST", &path, Some(&dana), &payload);
    assert_eq!((status, recorded), (200, json!({ "approved": true })));
    assert_eq!(control(&addr, "POST", &path, Some(&dana), &payload), (200, json!({ "approved": true })), "an identical repeat answers the recorded payload");
    let (status, refused) = control(&addr, "POST", &path, Some(&dana), &json!({ "approved": false }).to_string());
    assert_eq!((status, refused["error"]["identifier"].as_str()), (409, Some("AwakeableAlreadyResolved")), "{refused}");
    let (status, state) = control(&addr, "GET", &path, Some(&dana), "");
    assert_eq!((status, state["state"].as_str()), (200, Some("resolved")), "{state}");

    let (status, refused) = control(&addr, "POST", "/awake/00000000000000000000000000000000", Some(&dana), &payload);
    assert_eq!((status, refused["error"]["identifier"].as_str()), (404, Some("AwakeableUnknown")), "{refused}");
    let (status, refused) = control(&addr, "POST", &format!("/awake/{}", expired.token), Some(&dana), &payload);
    assert_eq!((status, refused["error"]["identifier"].as_str()), (410, Some("AwakeableTimedOut")), "{refused}");

    let now = Instant::parse("2030-01-01T00:01:00Z").unwrap();
    assert_eq!(registry(root).awaited("x-approval", &pending.token, now).unwrap(), Awaited::Resumed(payload.into_bytes()), "the payload is the awaited step's output");
}

/// An awakeable may bind a verified caller subject at mint; resolving it then takes the token and a credential for
/// that subject, and any other caller answers as an unknown token.
// spec: run.suspend.caller-binding@32d5c652
#[test]
fn a_bound_token_answers_unknown_to_every_other_subject() {
    let (dir, public) = project();
    let root = dir.path();
    let bound = registry(root).suspend_for("x-approval", "approval", "2030-01-01T00:00:00Z", 3600, Some("user://dana@acme.example")).unwrap();
    let dana = mint_for(root, "user://dana@acme.example");
    let eve = mint_for(root, "user://eve@acme.example");
    let (_listener, addr) = serve(root, &["serve", "--http", "127.0.0.1:0", "--audience", AUD, "--max-in-flight", "2", "--project", "research", "--public-key", &public]);
    let path = format!("/awake/{}", bound.token);
    let payload = json!({ "approved": true }).to_string();

    let (_, unknown) = control(&addr, "POST", "/awake/00000000000000000000000000000000", Some(&eve), &payload);
    for method in ["GET", "POST"] {
        let (status, refused) = control(&addr, method, &path, Some(&eve), &payload);
        assert_eq!((status, refused["error"]["identifier"].as_str()), (404, Some("AwakeableUnknown")), "{refused}");
        assert_eq!(refused.to_string().replace(&bound.token, "T"), unknown.to_string().replace("00000000000000000000000000000000", "T"), "a bound token discloses nothing past an unknown one");
    }
    let now = Instant::parse("2030-01-01T00:01:00Z").unwrap();
    assert_eq!(registry(root).state_as(&bound.token, Some("user://dana@acme.example"), now).unwrap().state, contextful_core::run::suspend::AwakeableState::Pending, "another subject changes nothing");
    let (status, state) = control(&addr, "GET", &path, Some(&dana), "");
    assert_eq!(status, 200, "{state}");
    assert!(state.get("caller").is_none(), "the route never echoes the binding: {state}");
    assert_eq!(control(&addr, "POST", &path, Some(&dana), &payload), (200, json!({ "approved": true })));
}

/// A project declaring a `receive` pipeline over two tables, its body bound 512 B and its queue 2 deep; the
/// issuer's public key.
fn receiving_face() -> (tempfile::TempDir, String) {
    receiving_face_with(2, 512)
}

/// [`receiving_face`], its queue `depth` deep and its body bound `bytes`.
pub(super) fn receiving_face_with(depth: u32, bytes: u64) -> (tempfile::TempDir, String) {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path();
    std::fs::create_dir_all(p.join(".contextful/context/research")).unwrap();
    std::fs::write(p.join(".contextful/context/research/config.toml"), "[node]\nid = \"ingest-a\"\n").unwrap();
    std::fs::write(p.join(".contextful/issuance.toml"), format!("default_audience = \"{AUD}\"\nmax_lifetime_secs = 86400\n")).unwrap();
    std::fs::write(p.join("contextful.toml"), format!("authoring_posture = \"per_request\"\nsite_id = \"site-a\"\n[[pipeline]]\nid = \"otel\"\ntables = [\"spans\", {{ name = \"events\", columns = {{ id = \"Utf8\", n = \"Int64\" }} }}]\n[pipeline.source]\nname = \"receive\"\nconfig = {{ format = \"jsonl\", max_batch_bytes = {bytes}, max_queue_depth = {depth}, key_retention_secs = 3600 }}\n")).unwrap();
    let public = stdout(&run(p, &["token", "keygen", "--out", ".contextful/issuer.seed"]));
    (dir, public)
}

/// `POST /receive/:pipeline` carrying `key` and `token`, its head announcing `length` body bytes of which only
/// `body` is sent; the status, the head and the JSON answer.
fn deliver(addr: &str, token: Option<&str>, key: Option<&str>, length: usize, body: &str) -> (u16, String, Value) {
    deliver_to(addr, "otel", token, key, length, body)
}

/// [`deliver`] addressed to `pipeline`.
fn deliver_to(addr: &str, pipeline: &str, token: Option<&str>, key: Option<&str>, length: usize, body: &str) -> (u16, String, Value) {
    let mut stream = TcpStream::connect(addr).unwrap();
    let auth = token.map(|token| format!("Authorization: Bearer {token}\r\n")).unwrap_or_default();
    let key = key.map(|key| format!("Contextful-Batch-Key: {key}\r\n")).unwrap_or_default();
    write!(stream, "POST /receive/{pipeline} HTTP/1.1\r\n{auth}{key}Content-Type: application/x-ndjson\r\nContent-Length: {length}\r\n\r\n{body}").unwrap();
    let mut raw = String::new();
    stream.read_to_string(&mut raw).unwrap();
    let (head, answer) = raw.split_once("\r\n\r\n").unwrap_or_else(|| panic!("no answer head: {raw:?}"));
    let status = head.split(' ').nth(1).and_then(|s| s.parse().ok()).unwrap_or_else(|| panic!("no status: {raw:?}"));
    (status, head.to_string(), serde_json::from_str(answer).unwrap_or_else(|e| panic!("{e}: {raw}")))
}

pub(super) fn delivered(table: &str, row: Value) -> String {
    format!("{}\n", json!({ "table": table, "row": row }))
}

pub(super) fn receive_rows(root: &Path, sql: &str) -> Value {
    let out: Value = serde_json::from_str(&stdout(&run(root, &["query", "--json", "--project", "research", sql]))).unwrap();
    out["rows"].clone()
}

pub(super) fn mint_write(root: &Path, table: &str) -> String {
    stdout(&run(root, &["token", "mint", "--issuer-key", ".contextful/issuer.seed", "--on-behalf-of", "user://dana@acme.example", "--action", "write", "--table", table, "--ttl", "900"]))
}

/// The served receive route admits the credential and its `write` grant over every table before reading the body,
/// and answers `200` with the receive answer once both tables commit; a resend under the key replays it.
// spec: run.land.receive-endpoint@9651966d
#[test]
fn the_receive_route_admits_before_the_body_and_answers_after_both_tables_commit() {
    let (dir, public) = receiving_face();
    let root = dir.path();
    let spans = mint_write(root, "otel_spans");
    let both = mint_write(root, "otel_*");
    let (_listener, addr) = serve(root, &["serve", "--http", "127.0.0.1:0", "--audience", AUD, "--max-in-flight", "4", "--project", "research", "--public-key", &public]);

    // Each refusal answers on the head alone: the announced body never arrives.
    let (status, head, _) = deliver(&addr, None, Some("flush-1"), 4096, "");
    assert_eq!(status, 401, "{head}");
    let (status, _, refused) = deliver(&addr, Some(&spans), Some("flush-1"), 100, "");
    assert_eq!((status, refused["error"]["identifier"].as_str()), (403, Some("ReceiveWriteNotGranted")), "{refused}");
    let (status, _, refused) = deliver(&addr, Some(&both), None, 100, "");
    assert_eq!((status, refused["error"]["identifier"].as_str()), (400, Some("ReceiveBatchKeyMissing")), "a delivery names its batch key: {refused}");

    let body = format!("{}{}{}", delivered("events", json!({ "id": "e1" })), delivered("spans", json!({ "id": "s1" })), delivered("spans", json!({ "id": "s2" })));
    let (status, _, ack) = deliver(&addr, Some(&both), Some("flush-1"), body.len(), &body);
    assert_eq!(status, 200, "{ack}");
    assert_eq!(ack["committed"], true, "{ack}");
    let run_id = ack["run_id"].as_str().unwrap().to_string();
    assert!(run_id.starts_with("receive-"), "{ack}");
    let outcomes: Vec<(&str, &str)> = ack["tables"].as_array().unwrap().iter().map(|t| (t["table"].as_str().unwrap(), t["outcome"].as_str().unwrap())).collect();
    assert_eq!(outcomes, [("otel_spans", "committed"), ("otel_events", "committed")], "{ack}");
    assert_eq!(receive_rows(root, "SELECT id FROM otel_spans ORDER BY id"), json!([["s1"], ["s2"]]));
    assert_eq!(receive_rows(root, "SELECT id FROM otel_events"), json!([["e1"]]));

    let (status, _, again) = deliver(&addr, Some(&both), Some("flush-1"), body.len(), &body);
    assert_eq!((status, again["run_id"].as_str()), (200, Some(run_id.as_str())), "{again}");
    let outcomes: Vec<&str> = again["tables"].as_array().unwrap().iter().map(|t| t["outcome"].as_str().unwrap()).collect();
    assert_eq!(outcomes, ["replayed", "replayed"], "{again}");
    assert_eq!(receive_rows(root, "SELECT id FROM otel_spans ORDER BY id"), json!([["s1"], ["s2"]]), "a resend lands no row");
}

/// A delivery without a batch key answers `400` before its body is read, and no table lands.
// spec: run.land.receive-key-missing@262167c8
#[test]
fn the_receive_route_refuses_a_delivery_without_a_batch_key() {
    let (dir, public) = receiving_face();
    let root = dir.path();
    let both = mint_write(root, "otel_*");
    let (_listener, addr) = serve(root, &["serve", "--http", "127.0.0.1:0", "--audience", AUD, "--max-in-flight", "4", "--project", "research", "--public-key", &public]);
    for key in [None, Some(" ")] {
        let (status, _, refused) = deliver(&addr, Some(&both), key, 100, "");
        assert_eq!((status, refused["error"]["identifier"].as_str()), (400, Some("ReceiveBatchKeyMissing")), "{refused}");
    }
    assert!(!root.join(".contextful/context/research/tables/otel_spans").exists(), "no table lands");
}

/// A delivery to an undeclared pipeline id, or to a pipeline whose source is not `receive`, answers `404` before
/// its body is read.
// spec: run.land.receive-pipeline-unknown@ba54fb00
#[test]
fn the_receive_route_answers_404_for_a_pipeline_that_receives_nothing() {
    let (dir, public) = receiving_face();
    let root = dir.path();
    let manifest = std::fs::read_to_string(root.join("contextful.toml")).unwrap();
    std::fs::write(root.join("contextful.toml"), format!("{manifest}[[pipeline]]\nid = \"pulled\"\ntables = [\"docs\"]\n[pipeline.source]\nname = \"http\"\nconfig = {{ endpoint = \"http://127.0.0.1:9/v1\" }}\n")).unwrap();
    let both = mint_write(root, "*");
    let (_listener, addr) = serve(root, &["serve", "--http", "127.0.0.1:0", "--audience", AUD, "--max-in-flight", "4", "--project", "research", "--public-key", &public]);
    for pipeline in ["nope", "pulled"] {
        let (status, _, refused) = deliver_to(&addr, pipeline, Some(&both), Some("flush-1"), 100, "");
        assert_eq!((status, refused["error"]["identifier"].as_str()), (404, Some("ReceivePipelineUnknown")), "{pipeline}: {refused}");
    }
}

/// The receive route answers `413` past `max_batch_bytes`, `503` with `Retry-After` while every queue slot is
/// held, `410` for a key past its retention and `422` for a batch failing a table on a non-retryable class.
// spec: run.land.receive-endpoint-status@ffd47ca5
#[test]
fn the_receive_route_maps_each_refusal_to_its_status() {
    let (dir, public) = receiving_face();
    let root = dir.path();
    let both = mint_write(root, "otel_*");
    // A key whose first attempt landed long before the face's clock.
    let mut early = Command::new(env!("CARGO_BIN_EXE_contextful"))
        .args(["pipeline", "receive", "otel", "--key", "flush-old", "--project", "research", "--now", "2020-01-01T00:00:00Z"])
        .current_dir(root)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    early.stdin.take().unwrap().write_all(delivered("spans", json!({ "id": "s0" })).as_bytes()).unwrap();
    let early = early.wait_with_output().unwrap();
    assert!(early.status.success(), "{}", String::from_utf8_lossy(&early.stderr));
    let (_listener, addr) = serve(root, &["serve", "--http", "127.0.0.1:0", "--audience", AUD, "--max-in-flight", "4", "--project", "research", "--public-key", &public]);

    let (status, _, refused) = deliver(&addr, Some(&both), Some("flush-1"), 513, "");
    assert_eq!((status, refused["error"]["identifier"].as_str()), (413, Some("ReceiveBatchTooLarge")), "{refused}");
    assert_eq!(refused["retryable"], false, "{refused}");

    let line = delivered("spans", json!({ "id": "s1" }));
    let (status, _, refused) = deliver(&addr, Some(&both), Some("flush-old"), line.len(), &line);
    assert_eq!((status, refused["error"]["identifier"].as_str()), (410, Some("ReceiveBatchKeyExpired")), "{refused}");

    let queue = root.join(".contextful/run/research/receive/otel");
    let held: Vec<std::fs::File> = (0..2)
        .map(|n| {
            let slot = std::fs::File::options().read(true).write(true).create(true).truncate(false).open(queue.join(format!("slot-{n}.lock"))).unwrap();
            slot.lock().unwrap();
            slot
        })
        .collect();
    let (status, head, refused) = deliver(&addr, Some(&both), Some("flush-1"), line.len(), &line);
    assert_eq!((status, refused["error"]["identifier"].as_str()), (503, Some("ReceiveBackpressure")), "{refused}");
    assert!(head.lines().any(|h| h.eq_ignore_ascii_case("retry-after: 1")), "{head}");
    drop(held);

    let broken = delivered("events", json!({ "id": "e1", "n": "many" }));
    let (status, _, failed) = deliver(&addr, Some(&both), Some("flush-2"), broken.len(), &broken);
    assert_eq!((status, failed["committed"].clone(), failed["retryable"].clone()), (422, json!(false), json!(false)), "{failed}");
    assert_eq!(receive_rows(root, "SELECT id FROM otel_spans"), json!([["s0"]]), "no refused batch lands");

    // A receiver killed while it waits on the held commit lock leaves the table's cursor lease live, so the
    // next delivery fails `otel_spans` on the `Transient` class.
    let table = root.join(".contextful/context/research/tables/otel_spans");
    let commit = std::fs::File::options().read(true).write(true).create(true).truncate(false).open(table.join("commit.lock")).unwrap();
    commit.lock().unwrap();
    let mut stalled = Command::new(env!("CARGO_BIN_EXE_contextful"))
        .args(["pipeline", "receive", "otel", "--key", "flush-3", "--project", "research"])
        .current_dir(root)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    stalled.stdin.take().unwrap().write_all(line.as_bytes()).unwrap();
    let staged_parts = || {
        let mut n = 0;
        let mut pending = vec![table.clone()];
        while let Some(path) = pending.pop() {
            if path.is_dir() {
                pending.extend(std::fs::read_dir(&path).unwrap().map(|e| e.unwrap().path()));
            } else if path.extension().is_some_and(|e| e == "parquet") {
                n += 1;
            }
        }
        n
    };
    let landed = staged_parts();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(25);
    while staged_parts() <= landed {
        assert!(std::time::Instant::now() < deadline, "the stalled receiver never staged");
        assert!(stalled.try_wait().unwrap().is_none(), "the stalled receiver ended before its commit");
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    stalled.kill().unwrap();
    stalled.wait().unwrap();
    commit.unlock().unwrap();
    drop(commit);
    let (status, head, failed) = deliver(&addr, Some(&both), Some("flush-4"), line.len(), &line);
    assert_eq!((status, failed["committed"].clone(), failed["retryable"].clone()), (503, json!(false), json!(true)), "{failed}");
    assert!(head.lines().any(|h| h.eq_ignore_ascii_case("retry-after: 1")), "a retryable failed table carries the retry interval: {head}");
}

/// The seed every daily bar of the served throughput schedule derives from.
const THROUGHPUT_SEED: u64 = 0x5eed_0102;

/// The rows of one delivered batch of daily bars.
const BAR_ROWS: u64 = 100;

/// The rounds each writer count delivers, one batch per writer each; the figure is their median round. A
/// 256-writer round spends about 90 s of a debug build retrying refusals, so that count runs three.
fn throughput_rounds(writers: u64) -> usize {
    if writers >= 256 { 3 } else { 5 }
}

/// `BAR_ROWS` daily bars for batch `batch`, their prices derived from `seed`.
fn bars(seed: u64, batch: u64) -> String {
    let mut state = seed ^ batch.wrapping_mul(0x9e37_79b9_7f4a_7c15);
    (0..BAR_ROWS)
        .map(|i| {
            state = state.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1_442_695_040_888_963_407);
            let open = 100 + (state >> 40) % 900;
            let close = open + (state >> 20) % 50;
            delivered("spans", json!({ "id": format!("b{batch}-{i}"), "symbol": format!("S{:03}", i % 64), "day": format!("2030-01-{:02}", 1 + i % 28), "open": open, "close": close, "volume": state >> 44 }))
        })
        .collect()
}

/// `POST /receive/otel` of `body` under `key`, resending after each `503` once its retry interval passes; the
/// `503` answers met before the batch committed.
fn deliver_retrying(addr: &str, token: &str, key: &str, body: &str) -> u64 {
    let mut refusals = 0;
    loop {
        let mut stream = TcpStream::connect(addr).unwrap();
        // A refusal answers on the head alone, so the body may meet a closed socket.
        let _ = write!(stream, "POST /receive/otel HTTP/1.1\r\nAuthorization: Bearer {token}\r\nContextful-Batch-Key: {key}\r\nContent-Type: application/x-ndjson\r\nContent-Length: {}\r\n\r\n{body}", body.len());
        let raw = exchange_response(&mut stream).unwrap_or_else(|e| panic!("batch `{key}`: {e}"));
        let (head, answer) = raw.split_once("\r\n\r\n").unwrap();
        match head.split(' ').nth(1) {
            Some("200") => return refusals,
            Some("503") => {
                refusals += 1;
                std::thread::sleep(std::time::Duration::from_secs(1));
            }
            other => panic!("batch `{key}` answered {other:?}: {answer}"),
        }
    }
}

/// The served receive route at 1, 16 and 256 concurrent writers of 100-row batches through a queue 64 deep: the
/// rows committed per second, the median round, and the `503` answers each 256-writer round meets. A run
/// outside a measure run drives the one-writer rounds alone.
#[test]
fn served_receive_throughput_at_one_sixteen_and_256_writers() {
    let measuring = std::env::var_os(contextful_eval::record::MEASURE_DIR_VAR).is_some();
    let (dir, public) = receiving_face_with(64, 65_536);
    let root = dir.path();
    let token = mint_write(root, "otel_*");
    let (_listener, addr) = serve(root, &["serve", "--http", "127.0.0.1:0", "--audience", AUD, "--max-in-flight", "320", "--project", "research", "--public-key", &public]);
    let writer_counts: &[u64] = if measuring { &[1, 16, 256] } else { &[1] };
    let mut next = 0u64;
    let mut rates = Vec::new();
    for &writers in writer_counts {
        let count = throughput_rounds(writers);
        let mut rounds: Vec<(f64, u64)> = (0..count)
            .map(|_| {
                let batches: Vec<(u64, String)> = (next..next + writers).map(|b| (b, bars(THROUGHPUT_SEED, b))).collect();
                next += writers;
                let gate = std::sync::Barrier::new(writers as usize + 1);
                let (refusals, wall) = std::thread::scope(|scope| {
                    let hands: Vec<_> = batches
                        .iter()
                        .map(|(b, body)| {
                            let (gate, addr, token) = (&gate, addr.as_str(), token.as_str());
                            scope.spawn(move || {
                                gate.wait();
                                deliver_retrying(addr, token, &format!("bars-{b}"), body)
                            })
                        })
                        .collect();
                    gate.wait();
                    let started = std::time::Instant::now();
                    let refusals: u64 = hands.into_iter().map(|h| h.join().unwrap()).sum();
                    (refusals, started.elapsed())
                });
                ((writers * BAR_ROWS) as f64 / wall.as_secs_f64(), refusals)
            })
            .collect();
        rounds.sort_unstable_by(|a, b| a.0.total_cmp(&b.0));
        let (rate, _) = rounds[count / 2];
        let mut refusals: Vec<u64> = rounds.iter().map(|r| r.1).collect();
        refusals.sort_unstable();
        let refused = refusals[count / 2];
        eprintln!("served receive, {writers} writer(s): {rate:.0} rows/s, {refused} 503 answers per round (median of {count} rounds)");
        let n = writers * count as u64;
        match writers {
            1 => contextful_eval::record::emit("receive-rows-per-s-w1", rate, n, THROUGHPUT_SEED),
            16 => contextful_eval::record::emit("receive-rows-per-s-w16", rate, n, THROUGHPUT_SEED),
            _ => {
                contextful_eval::record::emit("receive-backpressure-w256", refused as f64, n, THROUGHPUT_SEED);
                contextful_eval::record::emit("receive-rows-per-s-w256", rate, n, THROUGHPUT_SEED)
            }
        };
        rates.push(rate);
    }
    if let [one, .., many] = rates[..] {
        contextful_eval::record::emit("receive-rows-per-s-ratio", many / one, next, THROUGHPUT_SEED);
    }
    let landed = receive_rows(root, "SELECT count(*) FROM otel_spans");
    assert_eq!(landed[0][0].as_str().and_then(|n| n.parse::<u64>().ok()), Some(next * BAR_ROWS), "every committed batch lands its rows once: {landed}");
}
