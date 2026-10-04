//! One consumer flow carries an S3 source through a published model to a second node.

use contextful_acceptance::s3::{S3Server, ACCESS_KEY, SECRET_KEY};
use contextful_acceptance::{bin, GitRepo};
use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Read, Write};
use std::process::{Child, Output, Stdio};

const PROJECT: &str = "research";
const AUDIENCE: &str = "contextful://consumer-e2e";

struct Listener(Child);

impl Drop for Listener {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn serve(cf: &std::path::Path, p: &GitRepo, public: &str) -> (Listener, String) {
    let mut child = std::process::Command::new(cf)
        .args(["serve", "--http", "127.0.0.1:0", "--audience", AUDIENCE, "--max-in-flight", "2", "--project", PROJECT, "--public-key", public, "--denylist", ".contextful/denylist"])
        .current_dir(&p.root)
        .env_remove("CARGO_TARGET_DIR")
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut err = BufReader::new(child.stderr.take().unwrap());
    let listener = Listener(child);
    let mut heard = String::new();
    let address = loop {
        let mut line = String::new();
        assert!(err.read_line(&mut line).unwrap() > 0, "the HTTP read face never listened: {heard}");
        if let Some(address) = line.trim().strip_prefix("listening on http://").and_then(|s| s.strip_suffix("/mcp")) {
            break address.to_string();
        }
        heard.push_str(&line);
    };
    (listener, address)
}

fn bearer_call(address: &str, token: &str, tool: &str, arguments: Value) -> Value {
    let body = json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/call", "params": { "name": tool, "arguments": arguments } }).to_string();
    let mut stream = std::net::TcpStream::connect(address).unwrap();
    write!(stream, "POST /mcp HTTP/1.1\r\nHost: {address}\r\nAuthorization: Bearer {token}\r\nContent-Type: application/json\r\nAccept: application/json, text/event-stream\r\nContent-Length: {}\r\n\r\n{body}", body.len()).unwrap();
    let mut raw = Vec::new();
    stream.read_to_end(&mut raw).unwrap();
    let split = raw.windows(4).position(|part| part == b"\r\n\r\n").unwrap();
    let head = String::from_utf8_lossy(&raw[..split]);
    assert!(head.starts_with("HTTP/1.1 200 "), "{head}");
    let response: Value = serde_json::from_slice(&raw[split + 4..]).unwrap();
    assert_ne!(response["result"]["isError"], json!(true), "{response}");
    response["result"]["structuredContent"].clone()
}

fn ok(out: &Output) -> String {
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

fn manifest(endpoint: &str) -> String {
    format!(
        "authoring_posture = \"per_request\"\n\
         [[pipeline]]\nid = \"feed\"\n\
         [pipeline.source]\nname = \"s3\"\n\
         config = {{ bucket = \"source\", endpoint = \"{endpoint}\", key = \"notes.jsonl\", format = \"jsonl\", access_key_id = \"secret://source-key-id\", secret_access_key = \"secret://source-secret\" }}\n\
         [[pipeline.tables]]\nname = \"notes\"\n\
         [[model]]\nid = \"research/titles\"\nsql = \"SELECT note_id, title FROM feed_notes\"\nunique_key = [\"note_id\"]\n\
         [model.contract]\nversion = \"1.0.0\"\ncolumns = [{{ name = \"note_id\", type = \"utf8\", nullable = false }}, {{ name = \"title\", type = \"utf8\" }}]\n"
    )
}

fn node(p: &GitRepo, source: &str, shared: &str, id: &str) {
    p.write("contextful.toml", &manifest(source));
    p.write(
        ".contextful/context/research/config.toml",
        &format!(
            "[node]\nid = \"{id}\"\n\n[sync]\nendpoint = \"{shared}\"\nbucket = \"shared\"\nprefix = \"team\"\ncoordination = \"cas\"\naccess_key_id = \"env://CONTEXTFUL_SYNC_ACCESS_KEY_ID\"\nsecret_access_key = \"env://CONTEXTFUL_SYNC_SECRET_ACCESS_KEY\"\n"
        ),
    );
}

fn run(p: &GitRepo, cf: &std::path::Path, args: &[&str]) -> String {
    let out = p.run_env(
        cf,
        args,
        &[
            ("CONTEXTFUL_SECRETS_ALLOW_ENV_TEMPLATES", "1"),
            ("SOURCE_KEY_ID", ACCESS_KEY),
            ("SOURCE_SECRET", SECRET_KEY),
            ("CONTEXTFUL_SYNC_ACCESS_KEY_ID", ACCESS_KEY),
            ("CONTEXTFUL_SYNC_SECRET_ACCESS_KEY", SECRET_KEY),
        ],
    );
    assert!(out.status.success(), "{args:?}: {}", String::from_utf8_lossy(&out.stderr));
    ok(&out)
}

fn query(p: &GitRepo, cf: &std::path::Path) -> Value {
    serde_json::from_str(&run(p, cf, &["query", "--json", "--project", PROJECT, "SELECT note_id, title FROM \"research/titles\" ORDER BY note_id"])).unwrap()
}

#[test]
fn e2e_consumer_round_trip() {
    let cf = bin("contextful");
    let source = S3Server::start("source");
    source.seed("notes.jsonl", b"{\"note_id\":\"n1\",\"title\":\"Solar battery storage\"}\n");
    let shared = S3Server::start("shared");
    let first = GitRepo::init();
    node(&first, &source.endpoint, &shared.endpoint, "node-a");
    run(&first, &cf, &["pipeline", "run", "feed", "--project", PROJECT, "--run-id", "run-0001", "--site-id", "site-a"]);
    let built: Value = serde_json::from_str(&run(&first, &cf, &["build", "research/titles", "--project", PROJECT, "--site-id", "site-a", "--json"])).unwrap();
    assert_eq!(built["published"], json!(true), "{built}");
    let before = query(&first, &cf);
    assert_eq!(before["rows"], json!([["n1", "Solar battery storage"]]));

    first.write(".contextful/issuance.toml", &format!("default_audience = \"{AUDIENCE}\"\nmax_lifetime_secs = 3600\n"));
    first.write(".contextful/denylist", "");
    let public = run(&first, &cf, &["token", "keygen", "--out", ".contextful/issuer.seed"]);
    let token = run(&first, &cf, &[
        "token", "mint", "--issuer-key", ".contextful/issuer.seed", "--on-behalf-of", "user://ada@acme.example",
        "--agent", "agent://consumer", "--zone", "on-prem:hq", "--action", "read", "--table", "research/*", "--ttl", "3600",
    ]);
    let (_listener, address) = serve(&cf, &first, &public);
    let build_id = built["build_id"].as_str().unwrap();
    let pinned = bearer_call(
        &address,
        &token,
        "context.query",
        json!({ "sql": "SELECT note_id, title FROM \"research/titles\" ORDER BY note_id", "pin": { "research/titles": build_id } }),
    );
    assert_eq!(pinned["rows"], before["rows"], "{pinned}");
    assert_eq!(pinned["contextful.resolved"]["research/titles"]["build_id"], json!(build_id), "{pinned}");
    run(&first, &cf, &["sync", "push", "--project", PROJECT]);

    let second = GitRepo::init();
    node(&second, &source.endpoint, &shared.endpoint, "node-b");
    run(&second, &cf, &["sync", "pull", "--project", PROJECT]);
    let after = query(&second, &cf);
    assert_eq!(after, before, "the second node reads the published model byte for byte");
    assert!(source.gets() > 0, "the source bucket was read");
    assert!(shared.keys().iter().any(|k| k == "team/manifest.json"));
}
