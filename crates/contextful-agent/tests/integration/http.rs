#![cfg(feature = "http")]

//! The network transport over a loopback listener: MCP Streamable HTTP at `POST /mcp`,
//! per-request admission of short-lived bearers and of holder-bound credentials, each
//! request under its own proof, the in-flight ceiling and concurrency.

use contextful_agent::http::{audience, ceiling, read_request, Admitting, HttpFace, HttpRequest, Revocation};
use contextful_agent::mcp::Server;
use contextful_context::land::{land, Batch, RunContext};
use contextful_context::read::Face;
use contextful_context::Store;
use contextful_core::grant::{Action, Grant, TablePattern};
use contextful_core::identify::Subject;
use contextful_core::issue::{IssuancePolicy, Lifetime, MintContext, MintRequest, NodeRole, SignatureAlgorithm};
use contextful_core::ports::FixedClock;
use contextful_core::store::declare::TableDecl;
use contextful_core::store::lay_out::NodeId;
use contextful_core::store::reserve::Injection;
use contextful_core::time::Instant;
use contextful_policy::enforce::mask::Pepper;
use contextful_policy::issue::{mint, MintClaims, SeedSigner};
use contextful_policy::keyset::{KeyCheckpoint, KeySource, StaticPins};
use contextful_policy::possession::{jwk_thumbprint, sign_proof, ProofRequest};
use contextful_policy::revoke::{parse_denylist, RevocationState};
use contextful_core::AuthorityError;
use contextful_policy::verify::{verify_network, Admission, AdmittedAuthority};
use ed25519_dalek::SigningKey;
use serde_json::{json, Value};
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Mutex;

const AUD: &str = "contextful://acme-research";
const MANIFEST: &str = r#"[[pipeline.tables]]
name = "research/notes"

[[pipeline.tables]]
name = "research/nums"

[[pipeline.tables]]
name = "hr/salaries"

[[query_templates]]
id = "note"
sql = "SELECT note_id FROM \"research/notes\" WHERE note_id = ?"
parameters = ["id:string"]
"#;
const NOW: &str = "2030-01-01T00:05:00Z";

/// A statement over 1000 × 1000 × 50 rows: long enough that a point read beside it
/// finishes first by a wide margin.
const SLOW: &str = r#"SELECT count(*) AS n FROM "research/nums" a, "research/nums" b, "research/nums" c WHERE c.x < 50 AND a.x * b.x = c.x - 7"#;
const FAST: &str = r#"SELECT note_id FROM "research/notes" ORDER BY note_id"#;

fn at(s: &str) -> Instant {
    Instant::parse(s).unwrap()
}

fn put(store: &Store, table: &str, rows: Vec<Value>) {
    let rows = rows.iter().map(|r| r.as_object().unwrap().clone()).collect();
    let ctx = RunContext {
        node: NodeId::parse("ingest-a").unwrap(),
        injection: Injection { run_id: "run-0001".into(), site_id: "site-a".into(), batch_seq: Some(0), authored_by: None, taint: None },
        committed_at: at("2030-01-01T00:00:00Z"),
    };
    land(store, &TableDecl::named(table), &Batch { rows, types: Default::default() }, &ctx).unwrap();
}

/// A store and its issuer.
struct Fixture {
    _dir: tempfile::TempDir,
    face: Face,
    signer: SeedSigner,
    checkpoint: KeyCheckpoint<FixedClock>,
}

fn fixture() -> Fixture {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path(), "research").unwrap();
    put(&store, "research/notes", vec![json!({ "note_id": "n1", "title": "Solar" }), json!({ "note_id": "n2", "title": "Hiring" })]);
    put(&store, "research/nums", (0..1000).map(|x| json!({ "x": x })).collect());
    put(&store, "hr/salaries", vec![json!({ "employee": "e1" })]);
    let face = Face::open(store, MANIFEST, Pepper::resolve(|_| None)).unwrap();
    let signer = SeedSigner::generate(SignatureAlgorithm::Ed25519);
    let checkpoint = KeyCheckpoint::start(Box::new(StaticPins::parse(&signer.public_key_text()).unwrap()), FixedClock(at(NOW))).unwrap();
    Fixture { _dir: dir, face, signer, checkpoint }
}

fn holder(n: u8) -> SigningKey {
    SigningKey::from_bytes(&[n; 32])
}

/// A credential reading `tables` for `ttl` seconds, bound to `holder` when one is given.
fn credential(signer: &SeedSigner, tables: &str, ttl: u64, holder: Option<&SigningKey>) -> String {
    let policy = IssuancePolicy::parse(&format!("default_audience = \"{AUD}\"\nmax_lifetime_secs = 86400\n")).unwrap();
    let subject = Subject { on_behalf_of: Some("user://dana@acme.example".into()), zone: Some("on-prem:hq".into()), ..Subject::default() };
    let grant = Grant {
        actions: vec![Action::Read],
        tables: vec![TablePattern::parse(tables).unwrap()],
        tenant: None,
        aggregate: None,
        templates: Some(vec!["*".into()]),
        max_rows: None,
    };
    let mut req = MintRequest::custody(subject, vec![grant]);
    req.lifetime = Lifetime::Requested(ttl);
    let clock = FixedClock(at("2030-01-01T00:00:00Z"));
    let plan = policy.check(&req, &MintContext { node: NodeRole::Primary, signer, clock: &clock }).unwrap();
    let confirmation = holder.map(|k| jwk_thumbprint(k.verifying_key().as_bytes()));
    mint(&plan, &MintClaims { confirmation, ..MintClaims::default() }, signer).unwrap()
}

fn call(tool: &str, arguments: Value) -> Value {
    json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/call", "params": { "name": tool, "arguments": arguments } })
}

fn query(sql: &str) -> Value {
    call("context.query", json!({ "sql": sql }))
}

static NONCE: AtomicU64 = AtomicU64::new(0);

/// A `method` request to `target` carrying `body`, presenting `token` with a proof from
/// `key` over exactly that request.
fn proven(method: &str, target: &str, body: Vec<u8>, token: &str, key: &SigningKey) -> HttpRequest {
    let nonce = format!("nonce-{}", NONCE.fetch_add(1, Ordering::SeqCst));
    let proof = sign_proof(key, &ProofRequest { method, target, body: &body }, at(NOW), &nonce);
    HttpRequest {
        method: method.into(),
        target: target.into(),
        headers: vec![("Authorization".into(), format!("DPoP {token}")), ("DPoP".into(), proof), ("Content-Type".into(), "application/json".into())],
        body,
    }
}

/// `POST /mcp` carrying `message`, presenting `token` with a proof from `key`.
fn signed(message: &Value, token: &str, key: &SigningKey) -> HttpRequest {
    proven("POST", "/mcp", message.to_string().into_bytes(), token, key)
}

/// `POST /mcp` carrying `message` under `scheme`, with no proof.
fn unproven(message: &Value, scheme: &str, token: &str) -> HttpRequest {
    HttpRequest {
        method: "POST".into(),
        target: "/mcp".into(),
        headers: vec![("Authorization".into(), format!("{scheme} {token}")), ("Content-Type".into(), "application/json".into())],
        body: message.to_string().into_bytes(),
    }
}

fn wire(r: &HttpRequest) -> Vec<u8> {
    let mut out = format!("{} {} HTTP/1.1\r\nHost: localhost\r\nContent-Length: {}\r\n", r.method, r.target, r.body.len());
    for (k, v) in &r.headers {
        out.push_str(&format!("{k}: {v}\r\n"));
    }
    out.push_str("\r\n");
    let mut bytes = out.into_bytes();
    bytes.extend_from_slice(&r.body);
    bytes
}

/// Send `r` over a new connection; the status, head and body of the answer.
fn send(addr: SocketAddr, r: &HttpRequest) -> (u16, String, Vec<u8>) {
    let mut s = TcpStream::connect(addr).unwrap();
    s.write_all(&wire(r)).unwrap();
    let mut raw = Vec::new();
    s.read_to_end(&mut raw).unwrap();
    let split = raw.windows(4).position(|w| w == b"\r\n\r\n").unwrap();
    let head = String::from_utf8(raw[..split].to_vec()).unwrap();
    let status = head.split(' ').nth(1).unwrap().parse().unwrap();
    (status, head, raw[split + 4..].to_vec())
}

fn body(b: &[u8]) -> Value {
    serde_json::from_slice(b).unwrap()
}

/// The tool result a JSON-RPC answer carries.
fn result(b: &[u8]) -> Value {
    body(b)["result"]["structuredContent"].clone()
}

fn no_revocation() -> Result<RevocationState, String> {
    Ok(RevocationState::default())
}

/// A listener serving `f` with `ceiling`, revoking through `revocation`, for the rest
/// of the test process: the fixture and face are leaked so the accept loop outlives
/// the test body.
fn listen(f: Fixture, ceiling: usize, revocation: &'static Revocation<'static>) -> (&'static Fixture, SocketAddr) {
    let f: &'static Fixture = Box::leak(Box::new(f));
    let clock: &'static FixedClock = Box::leak(Box::new(FixedClock(at(NOW))));
    let admitting = Admitting { checkpoint: &f.checkpoint, audience: AUD, revocation };
    let face = Box::leak(Box::new(HttpFace::new(&f.face, clock, admitting, Some(ceiling)).unwrap()));
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    std::thread::spawn(move || face.serve(listener));
    (f, addr)
}

/// The same credential's answer from the tool server the stdio transport runs, its
/// holder proof taken as checked.
fn stdio_answer(f: &Fixture, token: &str, message: &Value) -> Value {
    let keys = StaticPins::parse(&f.signer.public_key_text()).unwrap().keys().unwrap();
    let revocation = RevocationState::default();
    let admission = Admission::new(at(NOW), &revocation).expecting(AUD);
    let authority = verify_network(token, &keys, &admission, |_| Ok::<(), AuthorityError>(())).unwrap();
    let current = |_: &AdmittedAuthority| Ok(());
    let clock = FixedClock(at(NOW));
    let server = Server::new(&f.face, authority, &current, &clock).unwrap();
    server.handle(&message.to_string()).unwrap()
}

/// `contextful serve --http <addr> --audience <aud> --max-in-flight <n>` answers MCP Streamable HTTP at `POST /mcp`: one JSON-RPC message per request, answered as `application/json` by the tool server the stdio transport runs.
// spec: read.register.network-transport@38ec986b
#[test]
fn post_mcp_answers_each_message_as_the_stdio_tool_server_does() {
    let key = holder(1);
    let f = fixture();
    let token = credential(&f.signer, "research/*", 900, Some(&key));
    let messages = [
        json!({ "jsonrpc": "2.0", "id": 0, "method": "initialize", "params": { "protocolVersion": "2025-06-18" } }),
        json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/list" }),
        query(FAST),
        call("note", json!({ "id": "n2" })),
        query(r#"SELECT * FROM "hr/salaries""#),
    ];
    let stdio: Vec<Value> = messages.iter().map(|m| stdio_answer(&f, &token, m)).collect();
    let (_f, addr) = listen(f, 4, &no_revocation);

    for (message, expected) in messages.iter().zip(&stdio) {
        let (status, head, answer) = send(addr, &signed(message, &token, &key));
        assert_eq!(status, 200, "{head}");
        assert!(head.contains("Content-Type: application/json"), "{head}");
        // The whole JSON-RPC answer, tool text included, is byte-identical to stdio's.
        assert_eq!(String::from_utf8(answer).unwrap(), expected.to_string());
    }
    assert_eq!(stdio[2]["result"]["structuredContent"]["rows"], json!([["n1"], ["n2"]]));
    assert_eq!(stdio[3]["result"]["structuredContent"]["rows"], json!([["n2"]]));
    // A read refusal arrives in-band, under transport success.
    assert_eq!(stdio[4]["result"]["isError"], json!(true));
    assert_eq!(stdio[0]["result"]["contextful.build"]["faces"], json!(["http"]));
}

/// A notification answers `202` with no body. The face holds no protocol session and opens no server stream: a `GET` or `DELETE` on `/mcp` answers `405`, any other path `404`.
// spec: read.register.stateless-session@7d9b76e1
#[test]
fn a_notification_answers_202_and_the_face_holds_no_session_or_stream() {
    let key = holder(1);
    let f = fixture();
    let token = credential(&f.signer, "research/*", 900, Some(&key));
    let (_f, addr) = listen(f, 4, &no_revocation);
    let (status, head, answer) = send(addr, &signed(&json!({ "jsonrpc": "2.0", "method": "notifications/initialized" }), &token, &key));
    assert_eq!(status, 202, "{head}");
    assert!(answer.is_empty() && !head.to_ascii_lowercase().contains("mcp-session-id"), "{head}");
    for method in ["GET", "DELETE"] {
        let (status, head, _) = send(addr, &proven(method, "/mcp", Vec::new(), &token, &key));
        assert_eq!(status, 405, "{head}");
        assert!(head.contains("Allow: POST"), "{head}");
    }
    let (status, _, _) = send(addr, &proven("POST", "/context.query", query(FAST).to_string().into_bytes(), &token, &key));
    assert_eq!(status, 404);
    // A body holding no message object answers a JSON-RPC error.
    let (status, _, answer) = send(addr, &proven("POST", "/mcp", b"[1,2]".to_vec(), &token, &key));
    assert_eq!((status, body(&answer)["error"]["code"].clone()), (400, json!(-32600)));
}

/// Each request carries `Authorization: Bearer <credential>`, or `DPoP <credential>` with a `DPoP` proof header, admitted per request under {{authority.verify.possession-binding}} and {{authority.verify.network-bearer}}, so one listener serves many credentials, each reading its own grants.
// spec: read.register.per-request-admission@b845e0c5
#[test]
fn one_listener_admits_bearers_and_holder_bound_credentials_each_on_its_own_grants() {
    let (dana, lee) = (holder(1), holder(2));
    let f = fixture();
    let research = credential(&f.signer, "research/*", 900, Some(&dana));
    let hr = credential(&f.signer, "hr/*", 900, Some(&lee));
    let hour = credential(&f.signer, "research/*", 3600, None);
    let longer = credential(&f.signer, "research/*", 3601, None);
    let (_f, addr) = listen(f, 4, &no_revocation);

    // A holder-bound credential under its holder's proof reads its own grants.
    let (status, _, notes) = send(addr, &signed(&query(FAST), &research, &dana));
    assert_eq!((status, result(&notes)["rows"].clone()), (200, json!([["n1"], ["n2"]])));
    let (status, _, salaries) = send(addr, &signed(&query(r#"SELECT employee FROM "hr/salaries""#), &hr, &lee));
    assert_eq!((status, result(&salaries)["rows"].clone()), (200, json!([["e1"]])));
    // Each credential reads its own grants alone.
    let (_, _, crossed) = send(addr, &signed(&query(FAST), &hr, &lee));
    assert_eq!(body(&crossed)["result"]["isError"], json!(true));

    // A bearer living 3600 s admits with no proof; one living 3601 s admits nothing.
    let (status, _, bearer_notes) = send(addr, &unproven(&query(FAST), "Bearer", &hour));
    assert_eq!((status, result(&bearer_notes)["rows"].clone()), (200, json!([["n1"], ["n2"]])));
    let (status, head, answer) = send(addr, &unproven(&query(FAST), "Bearer", &longer));
    let answer = body(&answer);
    assert_eq!((status, answer["error"]["identifier"].clone()), (401, json!("BearerLifetimeExceeded")), "{answer}");
    assert!(head.contains("Bearer"), "{head}");
    assert!(!answer.to_string().contains("n1"));

    // A holder-bound credential without its proof, or under another key's, admits nothing.
    for scheme in ["DPoP", "Bearer"] {
        let (status, head, answer) = send(addr, &unproven(&query(FAST), scheme, &research));
        assert_eq!((status, body(&answer)["error"]["identifier"].clone()), (401, json!("PossessionProofInvalid")), "{head}");
        assert!(head.contains("WWW-Authenticate: DPoP"), "{head}");
    }
    let (status, _, stolen) = send(addr, &signed(&query(FAST), &research, &lee));
    assert_eq!((status, body(&stolen)["error"]["identifier"].clone()), (401, json!("PossessionProofInvalid")));
    // A proof covering another body admits nothing.
    let mut swapped = signed(&query(FAST), &research, &dana);
    swapped.body = query(r#"SELECT note_id FROM "research/notes" WHERE note_id = 'n1'"#).to_string().into_bytes();
    assert_eq!(send(addr, &swapped).0, 401);
}

/// A served face admits a verified capability credential, presented as a bearer or bound to a holder key, and nothing else: no static bearer, no gateway shared secret, no unauthenticated owner path.
// spec: authority.issue.one-credential@4390c17b
#[test]
fn a_static_secret_or_a_foreign_credential_admits_nothing() {
    let key = holder(1);
    let f = fixture();
    let foreign = credential(&SeedSigner::generate(SignatureAlgorithm::Ed25519), "research/*", 900, Some(&key));
    let foreign_bearer = credential(&SeedSigner::generate(SignatureAlgorithm::Ed25519), "research/*", 900, None);
    let verified = credential(&f.signer, "research/*", 900, None);
    let (_f, addr) = listen(f, 4, &no_revocation);
    for token in ["s3cr3t-gateway-key", "owner", foreign.as_str(), foreign_bearer.as_str()] {
        for request in [unproven(&query(FAST), "Bearer", token), signed(&query(FAST), token, &key)] {
            let (status, _, answer) = send(addr, &request);
            assert_eq!((status, body(&answer)["error"]["identifier"].clone()), (401, json!("SignatureInvalid")), "{token}");
        }
    }
    // The issuer's own bearer credential admits.
    assert_eq!(send(addr, &unproven(&query(FAST), "Bearer", &verified)).0, 200);
}

/// A request its admission refuses answers `401` carrying the refusal's identifier, except a full nonce cache, which answers `503` under {{authority.verify.nonce-cache}}.
// spec: read.register.admission-refused@210d483c
#[test]
fn a_refused_admission_answers_401_with_its_identifier() {
    let key = holder(1);
    let f = fixture();
    let lapsed = credential(&f.signer, "research/*", 60, Some(&key));
    let token = credential(&f.signer, "research/*", 900, Some(&key));
    let (_f, addr) = listen(f, 4, &no_revocation);
    let (status, head, answer) = send(addr, &signed(&query(FAST), &lapsed, &key));
    assert_eq!((status, body(&answer)["error"]["identifier"].clone()), (401, json!("AuthorityExpired")), "{head}");
    assert!(head.contains("WWW-Authenticate"), "{head}");
    // A proof replayed under its nonce refuses as a replay.
    let request = signed(&query(FAST), &token, &key);
    assert_eq!(send(addr, &request).0, 200);
    let (status, _, answer) = send(addr, &request);
    assert_eq!((status, body(&answer)["error"]["identifier"].clone()), (401, json!("PossessionProofReplayed")));
}

/// A request carrying no `Authorization` credential raises `HttpCredentialMissing` with `401` and reads no row.
// spec: read.register.credential-missing@b828dbd1
#[test]
fn a_request_without_a_credential_is_refused_401() {
    let (_f, addr) = listen(fixture(), 4, &no_revocation);
    for headers in [vec![], vec![("Authorization".to_string(), "Basic dXNlcjpwYXNz".to_string())], vec![("Authorization".to_string(), "DPoP ".to_string())]] {
        let r = HttpRequest { method: "POST".into(), target: "/mcp".into(), headers, body: query(FAST).to_string().into_bytes() };
        let (status, head, answer) = send(addr, &r);
        assert_eq!(status, 401, "{head}");
        let answer = body(&answer);
        assert_eq!(answer["error"]["identifier"], json!("HttpCredentialMissing"));
        assert!(!answer.to_string().contains("n1"));
    }
}

static DENYLIST: Mutex<String> = Mutex::new(String::new());

fn revocation_from_denylist() -> Result<RevocationState, String> {
    let text = DENYLIST.lock().unwrap().clone();
    if text == "unreadable" {
        return Err("the denylist file is gone".into());
    }
    Ok(RevocationState { denylist: parse_denylist(&text, "static"), ..RevocationState::default() })
}

/// Each request re-reads the `--denylist` file, so a credential revoked between two requests is refused on the next with no restart; an unreadable denylist answers `503` and admits nothing.
// spec: read.register.per-request-revocation@ddafa468
#[test]
fn a_credential_revoked_between_requests_is_refused_on_the_next() {
    let key = holder(1);
    let f = fixture();
    let token = credential(&f.signer, "research/*", 900, Some(&key));
    let other = credential(&f.signer, "research/*", 900, Some(&key));
    let rev_id = contextful_policy::verify::introspect(&token).unwrap().rev_id;
    let (_f, addr) = listen(f, 4, &revocation_from_denylist);
    assert_eq!(send(addr, &signed(&query(FAST), &token, &key)).0, 200);
    *DENYLIST.lock().unwrap() = format!("{rev_id}\n");
    let (status, _, answer) = send(addr, &signed(&query(FAST), &token, &key));
    assert_eq!((status, body(&answer)["error"]["identifier"].clone()), (401, json!("AuthorityRevoked")));
    assert_eq!(send(addr, &signed(&query(FAST), &other, &key)).0, 200);
    *DENYLIST.lock().unwrap() = "unreadable".into();
    let (status, head, _) = send(addr, &signed(&query(FAST), &other, &key));
    assert_eq!(status, 503, "{head}");
}

/// Requests on any number of connections run concurrently, each statement on its own engine connection under {{read.cache.pool-connections}}; a slow statement delays no other request.
// spec: read.register.concurrent-statements@95a71dba
#[test]
fn a_fast_statement_answers_while_a_slow_one_runs() {
    let key = holder(1);
    let f = fixture();
    let token = credential(&f.signer, "research/*", 900, Some(&key));
    let (_f, addr) = listen(f, 4, &no_revocation);
    let slow_request = signed(&query(SLOW), &token, &key);
    let slow_done = AtomicBool::new(false);
    std::thread::scope(|s| {
        let slow = s.spawn(|| {
            let (status, _, answer) = send(addr, &slow_request);
            slow_done.store(true, Ordering::SeqCst);
            assert_eq!(status, 200);
            assert!(result(&answer)["rows"].is_array(), "{}", String::from_utf8_lossy(&answer));
            std::time::Instant::now()
        });
        std::thread::sleep(std::time::Duration::from_millis(200));
        assert!(!slow_done.load(Ordering::SeqCst), "the slow statement is still in flight when the fast one is sent");
        let (status, _, answer) = send(addr, &signed(&query(FAST), &token, &key));
        let fast_answered = std::time::Instant::now();
        // The fast answer arrives while the slow request is still open: a face that ran
        // requests one at a time would answer it only after the slow one completed.
        assert!(!slow_done.load(Ordering::SeqCst), "the fast statement answered only after the slow one completed");
        assert_eq!((status, result(&answer)["rows"].clone()), (200, json!([["n1"], ["n2"]])));
        let slow_completed = slow.join().unwrap();
        let margin = slow_completed.checked_duration_since(fast_answered);
        assert!(margin.is_some_and(|m| m >= std::time::Duration::from_millis(100)), "the slow statement completed {margin:?} after the fast answer");
    });
}

/// A request arriving while the ceiling's count of requests is in flight answers `503` with a `Retry-After` of 1 s and admits nothing.
// spec: read.register.past-ceiling@52803a80
#[test]
fn past_the_ceiling_a_request_answers_503_with_retry_after() {
    let key = holder(1);
    let f = fixture();
    let token = credential(&f.signer, "research/*", 900, Some(&key));
    let (_f, addr) = listen(f, 1, &no_revocation);
    let slow_request = signed(&query(SLOW), &token, &key);
    std::thread::scope(|s| {
        let slow = s.spawn(|| send(addr, &slow_request).0);
        std::thread::sleep(std::time::Duration::from_millis(200));
        let (status, head, _) = send(addr, &signed(&query(FAST), &token, &key));
        assert_eq!(status, 503, "{head}");
        assert!(head.contains("Retry-After: 1\r\n") || head.ends_with("Retry-After: 1"), "{head}");
        assert_eq!(slow.join().unwrap(), 200);
    });
    assert_eq!(send(addr, &signed(&query(FAST), &token, &key)).0, 200);
}

/// An accepted connection holds one of the ceiling's slots from accept until its answer is written; a connection accepted with every slot held answers as {{read.register.past-ceiling}} before its request is parsed, and starts no thread.
// spec: read.register.connection-ceiling@9e698a3d
#[test]
fn a_stalled_request_head_holds_a_slot_and_a_connection_past_the_ceiling_is_shed() {
    let (_f, addr) = listen(fixture(), 1, &no_revocation);
    let health = HttpRequest { method: "GET".into(), target: "/health".into(), headers: Vec::new(), body: Vec::new() };
    let mut stalled = TcpStream::connect(addr).unwrap();
    stalled.write_all(b"POST /mcp HTTP/1.1\r\n").unwrap();
    std::thread::sleep(std::time::Duration::from_millis(200));
    let started = std::time::Instant::now();
    let (status, head, _) = send(addr, &health);
    assert_eq!(status, 503, "a connection whose head never completes holds the one slot: {head}");
    assert!(head.contains("Retry-After: 1\r\n") || head.ends_with("Retry-After: 1"), "{head}");
    assert!(started.elapsed() < std::time::Duration::from_secs(5), "the shed answer waits on no read timeout");
    drop(stalled);
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    loop {
        let (status, head, _) = send(addr, &health);
        if status == 200 {
            break;
        }
        assert!(std::time::Instant::now() < deadline, "the slot frees once the stalled connection closes: {head}");
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
}

/// A serve with no `--audience`, or no positive `--max-in-flight`, raises `ServeDeclarationMissing` naming the flag and binds no listener; neither ships a default.
// spec: read.register.serve-declaration@39474b03
#[test]
fn a_missing_audience_or_ceiling_refuses_the_face() {
    for bad in [None, Some(0)] {
        let e = ceiling(bad).unwrap_err();
        assert!(e.starts_with("ServeDeclarationMissing") && e.contains("--max-in-flight"), "{e}");
    }
    assert_eq!(ceiling(Some(3)), Ok(3));
    for bad in [None, Some(""), Some("  ")] {
        let e = audience(bad).unwrap_err();
        assert!(e.starts_with("ServeDeclarationMissing") && e.contains("--audience"), "{e}");
    }
    let f = fixture();
    let clock = FixedClock(at(NOW));
    let admitting = Admitting { checkpoint: &f.checkpoint, audience: AUD, revocation: &no_revocation };
    assert!(HttpFace::new(&f.face, &clock, admitting, None).is_err());
}

/// A request body over 1 MiB answers `413` and is read no further.
// spec: read.register.request-body@960a28a1
#[test]
fn a_body_over_one_mebibyte_answers_413() {
    let head = format!("POST /mcp HTTP/1.1\r\nContent-Length: {}\r\n\r\n", 1024 * 1024 + 1);
    let err = read_request(&mut head.as_bytes()).unwrap_err();
    assert_eq!(err.status, 413);
    let exact = "POST /mcp HTTP/1.1\r\nContent-Length: 4\r\n\r\n{}  ";
    assert_eq!(read_request(&mut exact.as_bytes()).unwrap().body, b"{}  ");
}

/// `GET /health` answers `200` with the build identity of {{read.embed.build-identity}}, admitting no credential and reading no row.
// spec: read.register.health@78ab82e3
#[test]
fn health_answers_the_build_identity_without_a_credential() {
    let (_f, addr) = listen(fixture(), 4, &no_revocation);
    let (status, _, answer) = send(addr, &HttpRequest { method: "GET".into(), target: "/health".into(), headers: vec![], body: vec![] });
    assert_eq!(status, 200);
    assert_eq!(body(&answer)["contextful.build"]["faces"], json!(["http"]));
}
