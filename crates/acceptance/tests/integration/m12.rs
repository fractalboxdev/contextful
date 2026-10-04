//! Milestone 12 — the operator console.
//!
//! Reach: Query answers from governed store rows with sources, while Admin admits only
//! an operator holding its separate page grant.

use contextful_acceptance::http::{Response, Server};
use contextful_acceptance::{bin, workspace_root, GitRepo};
use jsonwebtoken::{encode, Algorithm, EncodingKey, Header};
use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::process::{Child, Command, Output, Stdio};

const STORE_AUDIENCE: &str = "contextful://console-acceptance";
const QUERY_ACCESS_AUDIENCE: &str = "query-app-acceptance";
const ADMIN_ACCESS_AUDIENCE: &str = "admin-app-acceptance";
const ACCESS_ISSUER: &str = "https://access.example.test";
const ACCESS_KEY: &str = include_str!("../../../contextful-policy/tests/fixtures/exchange/idp.key.pem");
const ACCESS_JWKS: &str = include_str!("../../../contextful-policy/tests/fixtures/exchange/jwks.json");

fn ok(output: &Output) -> String {
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    String::from_utf8_lossy(&output.stdout).trim().to_owned()
}

fn access_token(audience: &str) -> String {
    let mut header = Header::new(Algorithm::RS256);
    header.kid = Some("idp-2030".into());
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs();
    encode(
        &header,
        &json!({
            "iss": ACCESS_ISSUER, "aud": [audience], "sub": "operator-1",
            "email": "operator@example.test", "iat": now, "exp": now + 300,
        }),
        &EncodingKey::from_rsa_pem(ACCESS_KEY.as_bytes()).unwrap(),
    )
    .unwrap()
}

struct Listener(Child);

impl Drop for Listener {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn listening(mut child: Child, suffix: &str) -> (Listener, String) {
    let mut stderr = BufReader::new(child.stderr.take().unwrap());
    let listener = Listener(child);
    let mut diagnostics = String::new();
    loop {
        let mut line = String::new();
        assert!(stderr.read_line(&mut line).unwrap() > 0, "listener exited before announcing its address: {diagnostics}");
        if let Some(address) = line.trim().strip_prefix("listening on http://").and_then(|s| s.strip_suffix(suffix)) {
            return (listener, address.to_owned());
        }
        diagnostics.push_str(&line);
    }
}

fn request(address: &str, method: &str, path: &str, token: Option<&str>, body: Option<&Value>) -> (u16, Vec<u8>) {
    let mut stream = TcpStream::connect(address).unwrap();
    let data = body.map(Value::to_string).unwrap_or_default();
    let auth = token.map(|t| format!("Cf-Access-Jwt-Assertion: {t}\r\n")).unwrap_or_default();
    write!(
        stream,
        "{method} {path} HTTP/1.1\r\nHost: {address}\r\n{auth}Content-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{data}",
        data.len()
    )
    .unwrap();
    let mut raw = Vec::new();
    stream.read_to_end(&mut raw).unwrap();
    let split = raw.windows(4).position(|window| window == b"\r\n\r\n").unwrap();
    let status = String::from_utf8_lossy(&raw[..split]).split_whitespace().nth(1).unwrap().parse().unwrap();
    (status, raw[split + 4..].to_vec())
}

#[test]
#[ignore = "milestone 12 is open: the console server and client library are absent"]
fn m12_console() {
    let cf = bin("contextful");
    let repo = GitRepo::init();
    repo.write("contextful.toml", "authoring_posture = \"per_request\"\n[[pipeline]]\nid = \"filings-flow\"\n[[pipeline.tables]]\nname = \"filings\"\n");
    repo.write("filings.jsonl", &json!({
        "filing_id": "filing-1", "publisher": "Northwind", "summary": "Northwind filed on Monday",
        "source_url": "https://example.test/filing-1"
    }).to_string());
    ok(&repo.run(&cf, &["context", "land", "filings", "--project", "research", "--rows", "filings.jsonl", "--run-id", "load-1", "--site-id", "site-a"]));
    repo.write(".contextful/issuance.toml", &format!("default_audience = \"{STORE_AUDIENCE}\"\nmax_lifetime_secs = 3600\n"));
    let public = ok(&repo.run(&cf, &["token", "keygen", "--out", ".contextful/issuer.seed"]));
    let read_token = ok(&repo.run(&cf, &[
        "token", "mint", "--issuer-key", ".contextful/issuer.seed", "--on-behalf-of", "user://operator@example.test",
        "--zone", "on-prem:hq", "--action", "read", "--table", "filings", "--ttl", "3600",
    ]));
    let read = Command::new(&cf)
        .args(["serve", "--http", "127.0.0.1:0", "--max-in-flight", "2", "--project", "research", "--public-key", &public, "--audience", STORE_AUDIENCE])
        .current_dir(&repo.root)
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let (_read, read_address) = listening(read, "/mcp");

    let jwks = Server::start(|req| {
        assert_eq!(req.path(), "/certs");
        Response::json(200, ACCESS_JWKS)
    });
    let model = Server::start(|req| {
        assert_eq!(req.path(), "/v1/chat/completions");
        let prompt = String::from_utf8_lossy(&req.body);
        let content = if prompt.contains("Northwind filed on Monday") {
            "Northwind filed on Monday."
        } else {
            "The supplied rows do not answer this question."
        };
        Response::json(200, &json!({ "choices": [{ "message": { "role": "assistant", "content": content } }] }).to_string())
    });
    let stores = json!([{ "id": "field-notes", "label": "Field notes", "endpoint": format!("http://{read_address}"), "auth": "shared" }]);
    let console = Command::new("node")
        .arg(workspace_root().join("apps/console/dist/server.js"))
        .args(["--http", "127.0.0.1:0"])
        .env("CONTEXTFUL_STORES_JSON", stores.to_string())
        .env("FIELD_NOTES_QUERY_TOKEN", &read_token)
        .env("CONTEXTFUL_ACCESS_JWKS_URL", jwks.url("/certs"))
        .env("CONTEXTFUL_ACCESS_ISSUER", ACCESS_ISSUER)
        .env("CONTEXTFUL_QUERY_ACCESS_AUDIENCE", QUERY_ACCESS_AUDIENCE)
        .env("CONTEXTFUL_ADMIN_ACCESS_AUDIENCE", ADMIN_ACCESS_AUDIENCE)
        .env("CONTEXTFUL_MODEL_ENDPOINT", model.url("/v1"))
        .env("CONTEXTFUL_MODEL_ID", "fixture")
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let (_console, console_address) = listening(console, "");

    assert_eq!(request(&console_address, "GET", "/query", None, None).0, 401);
    assert_eq!(request(&console_address, "GET", "/admin", Some(&access_token(QUERY_ACCESS_AUDIENCE)), None).0, 403);
    let (status, answer) = request(
        &console_address, "POST", "/query/api/ask", Some(&access_token(QUERY_ACCESS_AUDIENCE)),
        Some(&json!({ "store": "field-notes", "question": "Which filing arrived?" })),
    );
    assert_eq!(status, 200, "{}", String::from_utf8_lossy(&answer));
    let answer: Value = serde_json::from_slice(&answer).unwrap();
    assert!(answer["answer"].as_str().unwrap().contains("Northwind filed on Monday"), "{answer}");
    assert!(
        answer["sources"].as_array().is_some_and(|sources| {
            sources.iter().any(|source| source.to_string().contains("filing-1"))
        }),
        "the sources must identify the filing behind the answer: {answer}"
    );
    assert!(!model.received("/v1/chat/completions").is_empty(), "the answer uses the model endpoint");
    assert_eq!(request(&console_address, "GET", "/admin/api/workflows", Some(&access_token(QUERY_ACCESS_AUDIENCE)), None).0, 403);
    let (status, workflows) = request(&console_address, "GET", "/admin/api/workflows", Some(&access_token(ADMIN_ACCESS_AUDIENCE)), None);
    assert_eq!(status, 200, "{}", String::from_utf8_lossy(&workflows));
    let workflows = String::from_utf8(workflows).unwrap();
    assert!(workflows.contains("filings-flow") && workflows.contains("filings"), "{workflows}");
}
