//! Write-time spans, canonical Json and row-selected masks through public surfaces.

use contextful_acceptance::{bin, GitRepo, stdio::Session};
use parquet::file::reader::{FileReader, SerializedFileReader};
use parquet::record::Field;
use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Write};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::Output;

fn ok(output: Output) -> String {
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    String::from_utf8(output.stdout).unwrap().trim().to_owned()
}

fn project(manifest: &str) -> GitRepo {
    let project = GitRepo::init();
    project.write("contextful.toml", &format!("authoring_posture = \"per_request\"\n{manifest}"));
    project.write(".contextful/context/research/config.toml", "[node]\nid = \"ingest-a\"\n");
    project
}

fn vendor(rows: Value) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    std::thread::spawn(move || {
        let body = rows.to_string();
        for stream in listener.incoming() {
            let mut stream = stream.unwrap();
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            loop {
                let mut line = String::new();
                if reader.read_line(&mut line).unwrap() == 0 || line.trim().is_empty() { break; }
            }
            let _ = write!(stream, "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len());
        }
    });
    format!("http://127.0.0.1:{port}/rows")
}

fn part(project: &GitRepo, table: &str, run: &str) -> PathBuf {
    let path = project.root.join(format!(".contextful/context/research/tables/{table}/data/runs/{run}/ingest-a"));
    std::fs::read_dir(path).unwrap().map(|entry| entry.unwrap().path()).find(|path| path.extension().is_some_and(|extension| extension == "parquet")).unwrap()
}

fn text(path: &Path, column: &str) -> String {
    let reader = SerializedFileReader::new(std::fs::File::open(path).unwrap()).unwrap();
    let row = reader.get_row_iter(None).unwrap().next().unwrap().unwrap();
    match row.get_column_iter().find(|(name, _)| name.as_str() == column).unwrap().1 {
        Field::Str(text) => text.clone(),
        other => panic!("{column}: {other:?}"),
    }
}

fn land(project: &GitRepo, binary: &Path, table: &str, rows: &Value, run: &str) {
    project.write("rows.jsonl", &rows.as_array().unwrap().iter().map(Value::to_string).collect::<Vec<_>>().join("\n"));
    ok(project.run(binary, &["context", "land", table, "--project", "research", "--rows", "rows.jsonl", "--run-id", run, "--site-id", "site-a", "--now", "2030-01-01T00:00:00Z"]));
}

fn pull(project: &GitRepo, binary: &Path, id: &str, run: &str) {
    ok(project.run(binary, &["pipeline", "validate"]));
    ok(project.run(binary, &["pipeline", "run", id, "--project", "research", "--run-id", run, "--site-id", "site-a", "--now", "2030-01-01T00:00:00Z"]));
}

#[test]
fn declared_removal_and_requested_row_classes_cover_the_six_consumer_cases() {
    let binary = bin("contextful");
    let rows = json!([{"body":"call 415-555-0100 now","public":"keep"}]);
    let rule = r#"{ table = "messages", column = "body", match = { pattern = "[0-9]{3}-[0-9]{3}-[0-9]{4}" }, operation = "replace", argument = "phone" }"#;
    let direct = project(&format!("[[pipeline.tables]]\nname = \"messages\"\nredaction = [{rule}]\n"));
    let endpoint = vendor(rows.clone());
    let pulled = project(&format!("[[pipeline]]\nid = \"feed\"\ntables = [\"messages\"]\njournal = false\nredaction = [{rule}]\n[pipeline.source]\nname = \"http\"\nconfig = {{ endpoint = \"{endpoint}\" }}\n"));
    land(&direct, &binary, "messages", &rows, "run-a");
    pull(&pulled, &binary, "feed", "run-a");
    let direct_part = part(&direct, "messages", "run-a");
    let pulled_part = part(&pulled, "feed_messages", "run-a");
    assert_eq!(text(&direct_part, "body"), "call [REDACTED:phone] now");
    assert_eq!(text(&direct_part, "public"), "keep");
    assert_eq!(std::fs::read(direct_part).unwrap(), std::fs::read(pulled_part).unwrap());

    let json_store = project(r#"
[[pipeline.tables]]
name = "jsoncells"
columns = { body = "json" }
redaction = [{ table = "jsoncells", column = "body", match = { pattern = "[0-9]{3}-[0-9]{3}-[0-9]{4}" }, json_path = "$.parts[*].text", operation = "replace", argument = "phone" }]
"#);
    land(&json_store, &binary, "jsoncells", &json!([{"body":{"z":"public 415-555-0100","parts":[{"text":"call 415-555-0100 now","kind":"prompt"}]}}]), "json-a");
    assert_eq!(text(&part(&json_store, "jsoncells", "json-a"), "body"), "{\"parts\":[{\"kind\":\"prompt\",\"text\":\"call [REDACTED:phone] now\"}],\"z\":\"public 415-555-0100\"}");

    let masked = project(r#"
[[pipeline.tables]]
name = "messages"
[pipeline.tables.policy.columns]
body = { class = { from = "kind" }, strategies = { prompt = "drop", completion = "hash" }, fallback = "drop" }
[[pipeline.tables]]
name = "registered"
[pipeline.tables.policy.columns]
prompt = { class = "prompt", strategy = "drop" }
completion = { class = "completion", strategy = "hash" }
"#);
    masked.write(".contextful/issuance.toml", "default_audience = \"contextful://acceptance\"\nmax_lifetime_secs = 3600\n");
    land(&masked, &binary, "messages", &json!([{"id":"1","kind":"prompt","body":"private prompt"},{"id":"2","kind":"completion","body":"private completion"},{"id":"3","kind":"unknown","body":"private unknown"}]), "mask-a");
    land(&masked, &binary, "registered", &json!([{"prompt":"private prompt","completion":"private completion"}]), "registered-a");
    let public = ok(masked.run(&binary, &["token", "keygen", "--out", ".contextful/issuer.seed"]));
    let token = ok(masked.run(&binary, &["token", "mint", "--issuer-key", ".contextful/issuer.seed", "--on-behalf-of", "user://acceptance", "--agent", "agent://acceptance", "--zone", "on-prem:hq", "--action", "read", "--table", "messages", "--ttl", "3600"]));
    let mut session = Session::spawn(&binary, &["mcp", "--project", "research", "--public-key", &public, "--audience", "contextful://acceptance"], &masked.root, &[("CONTEXTFUL_TOKEN", &token)]);
    let exchange = |session: &mut Session, id: u64, method: &str, params: Value| -> Value {
        serde_json::from_str(&session.exchange(&json!({"jsonrpc":"2.0","id":id,"method":method,"params":params}).to_string())).unwrap()
    };
    let initialized = exchange(&mut session, 1, "initialize", json!({"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"acceptance","version":"0"}}));
    assert_eq!(initialized["result"]["serverInfo"]["name"], "contextful");
    session.send(&json!({"jsonrpc":"2.0","method":"notifications/initialized"}).to_string());
    let answer = exchange(&mut session, 2, "tools/call", json!({"name":"context.query","arguments":{"sql":"SELECT id, body FROM messages ORDER BY id"}}));
    assert_ne!(answer["result"]["isError"], true, "{answer}");
    let result = &answer["result"]["structuredContent"]["rows"];
    assert_eq!(result[0], json!(["1", ""]));
    let digest = result[1][1].as_str().unwrap();
    assert_eq!(digest.len(), 32);
    assert!(digest.bytes().all(|byte| byte.is_ascii_hexdigit()));
    assert_ne!(digest, "private completion");
    assert_eq!(result[2], json!(["3", ""]));

    let large = project(r#"
[[pipeline.tables]]
name = "large"
redaction = [{ table = "large", column = "body", match = { pattern = ".*[^a]|a" }, operation = "drop" }]
"#);
    let bytes = 1024 * 1024;
    let start = std::time::Instant::now();
    land(&large, &binary, "large", &json!([{"body":"a".repeat(bytes)}]), "large-a");
    assert_eq!(text(&part(&large, "large", "large-a"), "body"), "");
    eprintln!("built landing repeated-a input={bytes} elapsed={:?}", start.elapsed());
}
