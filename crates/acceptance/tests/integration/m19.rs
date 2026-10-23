//! Milestone 19 — Drive connections.
//!
//! Reach: a Picker-selected file creates one tenant-bound Drive pipeline, lands scoped rows,
//! pauses without another fire, and deletes with its credential revoked.

use contextful_acceptance::http::{Response, Server};
use contextful_acceptance::{bin, GitRepo};
use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::process::{Child, Command, Output, Stdio};
use std::time::{Duration, Instant};

const AUD: &str = "contextful://drive-connections";
const PRINCIPAL: &str = "user://dana@acme.example";

fn ok(out: &Output) -> String {
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

/// A recorded Drive: one token endpoint and one picked text file.
fn drive(request: &contextful_acceptance::http::Request) -> Response {
    match request.path() {
        "/token" => Response::json(200, r#"{"access_token":"ya29.acceptance","expires_in":3600,"token_type":"Bearer"}"#),
        "/drive/v3/files/doc-plan" => Response::json(200, r#"{"id":"doc-plan","name":"Plan","mimeType":"text/plain","modifiedTime":"2031-01-02T09:00:00.000Z","version":"5","size":"4"}"#),
        _ => Response::json(404, r#"{"error":{"code":404}}"#),
    }
}

struct Serve(Child);

impl Drop for Serve {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn serve(cf: &std::path::Path, root: &std::path::Path, public: &str) -> (Serve, String) {
    let mut child = Command::new(cf)
        .args(["serve", "--http", "127.0.0.1:0", "--audience", AUD, "--project", "research", "--public-key", public])
        .current_dir(root)
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut err = BufReader::new(child.stderr.take().unwrap());
    let guard = Serve(child);
    let mut line = String::new();
    let mut addr = None;
    while err.read_line(&mut line).unwrap() > 0 {
        if let Some(a) = line.trim().strip_prefix("listening on http://").and_then(|a| a.strip_suffix("/mcp")) {
            addr = Some(a.to_string());
        }
        if line.starts_with("hydrated:") {
            break;
        }
        line.clear();
    }
    (guard, addr.expect("the face listened"))
}

fn call(addr: &str, method: &str, path: &str, token: &str, body: &str) -> (u16, Value) {
    let mut stream = TcpStream::connect(addr).unwrap();
    write!(stream, "{method} {path} HTTP/1.1\r\nAuthorization: Bearer {token}\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}", body.len()).unwrap();
    let mut raw = String::new();
    stream.read_to_string(&mut raw).unwrap();
    let (head, answer) = raw.split_once("\r\n\r\n").unwrap();
    (head.split(' ').nth(1).unwrap().parse().unwrap(), serde_json::from_str(answer).unwrap_or(Value::Null))
}

/// Wait until `done` holds, at most 30 s.
fn eventually(what: &str, done: impl Fn() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(30);
    while !done() {
        assert!(Instant::now() < deadline, "{what}");
        std::thread::sleep(Duration::from_millis(100));
    }
}

#[test]
#[ignore = "milestone 19 is open: served connections do not yet arm a Drive pipeline"]
fn m19_drive_connections() {
    let cf = bin("contextful");
    let google = Server::start(drive);
    let p = GitRepo::init();
    p.write(".contextful/issuance.toml", &format!("default_audience = \"{AUD}\"\nmax_lifetime_secs = 3600\n"));
    p.write(
        "contextful.toml",
        &format!(
            "authoring_posture = \"per_request\"\n[[pipeline.tables]]\nname = \"research/notes\"\n\n\
             [connections.gdrive-picks]\napi_base = \"{}\"\ntoken_url = \"{}\"\n\
             oauth = {{ client_id = \"${{secret://drive-client-id}}\", client_secret = \"${{secret://drive-client-secret}}\" }}\n",
            google.url(""),
            google.url("/token")
        ),
    );
    p.write("notes.jsonl", "{\"note_id\":\"n1\"}\n");
    ok(&p.run(&cf, &["context", "land", "research/notes", "--project", "research", "--rows", "notes.jsonl", "--run-id", "run-0001", "--site-id", "site-a"]));
    ok(&p.run(&cf, &["pipeline", "import", "--project", "research"]));
    let public = ok(&p.run(&cf, &["token", "keygen", "--out", ".contextful/issuer.seed"]));
    let tenant = ok(&p.run(&cf, &["token", "mint", "--issuer-key", ".contextful/issuer.seed", "--on-behalf-of", PRINCIPAL, "--zone", "on-prem:hq",
        "--action", "write", "--table", "research/notes", "--tenant", "research/notes=tenant-a", "--ttl", "900"]));
    let (_serve, addr) = serve(&cf, &p.root, &public);

    // One Picker-selected file creates one tenant-bound pipeline, and its first fire reads it.
    let create = json!({"id": "conn-a", "tenant": "tenant-a", "principal": PRINCIPAL, "file_ids": ["doc-plan"],
        "refresh_secret": "drive-refresh-dana", "schedule": "every 1s"}).to_string();
    let (status, made) = call(&addr, "POST", "/connections", &tenant, &create);
    assert_eq!(status, 201, "{made}");
    eventually("the connection fires its Drive pipeline", || !google.received("/drive/v3/files/doc-plan").is_empty());
    let landed: Value = serde_json::from_str(&ok(&p.run(&cf, &["query", "--json", "--project", "research",
        "SELECT file_id, tenant, principal, connection_id FROM files"]))).unwrap();
    assert_eq!(landed["rows"], json!([["doc-plan", "tenant-a", PRINCIPAL, "conn-a"]]));

    // Paused, it fires no more.
    let (status, paused) = call(&addr, "POST", "/connections/conn-a/pause", &tenant, "{}");
    assert_eq!(status, 200, "{paused}");
    let reads = google.received("/drive/v3/files/doc-plan").len();
    std::thread::sleep(Duration::from_secs(3));
    assert_eq!(google.received("/drive/v3/files/doc-plan").len(), reads, "a paused connection starts no fire");

    // Deleted, its credential name is revoked and a later fire cannot hydrate it.
    let (status, deleted) = call(&addr, "DELETE", "/connections/conn-a", &tenant, "{}");
    assert_eq!(status, 200, "{deleted}");
    assert_eq!(deleted["state"], "deleted");
    assert_eq!(deleted["credential"], "revoked", "{deleted}");
}
