//! A run continuing the caller's W3C trace and exporting its span, through the built binary.
#![cfg(feature = "data-plane")]

use crate::pipeline::{ok, project, Vendor};
use crate::serve::{run, serve_env, stdout, AUD};
use serde_json::Value;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::Path;
use std::process::{Command, Output, Stdio};
use std::sync::{Arc, Mutex};

const TRACE: &str = "4bf92f3577b34da6a3ce929d0e0e4736";
const PARENT: &str = "00f067aa0ba902b7";
const PROXIES: [&str; 6] = ["HTTPS_PROXY", "https_proxy", "HTTP_PROXY", "http_proxy", "ALL_PROXY", "all_proxy"];

/// The `(target, body)` of each request a collector received.
type Received = Arc<Mutex<Vec<(String, Vec<u8>)>>>;

/// A loopback collector answering every POST with 200, keeping each `(target, body)`.
struct Collector {
    url: String,
    seen: Received,
}

impl Collector {
    fn start() -> Collector {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://127.0.0.1:{}", listener.local_addr().unwrap().port());
        let seen: Received = Arc::default();
        let keep = seen.clone();
        std::thread::spawn(move || {
            for mut stream in listener.incoming().flatten() {
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                let mut line = String::new();
                if reader.read_line(&mut line).is_err() {
                    continue;
                }
                let target = line.split_whitespace().nth(1).unwrap_or_default().to_string();
                let mut length = 0;
                loop {
                    let mut h = String::new();
                    if reader.read_line(&mut h).unwrap_or(0) == 0 || h.trim().is_empty() {
                        break;
                    }
                    if let Some((k, v)) = h.split_once(':') {
                        if k.trim().eq_ignore_ascii_case("content-length") {
                            length = v.trim().parse().unwrap_or(0);
                        }
                    }
                }
                let mut body = vec![0; length];
                let _ = reader.read_exact(&mut body);
                keep.lock().unwrap().push((target, body));
                let _ = write!(stream, "HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{{}}");
            }
        });
        Collector { url, seen }
    }

    /// Every span the collector received at `/v1/traces`.
    fn spans(&self) -> Vec<Value> {
        let seen = self.seen.lock().unwrap();
        let mut out = Vec::new();
        for (target, body) in seen.iter() {
            assert_eq!(target, "/v1/traces");
            let v: Value = serde_json::from_slice(body).unwrap();
            for resource in v["resourceSpans"].as_array().unwrap() {
                for scope in resource["scopeSpans"].as_array().unwrap() {
                    out.extend(scope["spans"].as_array().unwrap().iter().cloned());
                }
            }
        }
        out
    }
}

fn manifest(vendor: &Vendor) -> String {
    format!("site_id = \"site-m\"\n\n[[pipeline]]\nid = \"shop\"\n\ntables = [\"orders\"]\n[pipeline.source]\nname = \"http\"\nconfig = {{ endpoint = \"{}\" }}\n", vendor.url("/v1/{table}"))
}

fn fire(dir: &Path, run: &str, env: &[(&str, &str)]) -> Output {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_contextful"));
    cmd.args(["pipeline", "run", "shop", "--project", "research", "--run-id", run, "--now", "2030-01-01T00:00:00Z"])
        .current_dir(dir)
        .env_remove("CONTEXTFUL_NODE_ID")
        .env_remove("CONTEXTFUL_SECRETS_BACKEND")
        .env_remove("TRACEPARENT")
        .env_remove("TRACESTATE")
        .env_remove("OTEL_EXPORTER_OTLP_ENDPOINT")
        .env_remove("OTEL_EXPORTER_OTLP_TRACES_ENDPOINT");
    for k in PROXIES {
        cmd.env_remove(k);
    }
    cmd.envs(env.iter().copied());
    cmd.output().unwrap()
}

fn trace_of(dir: &Path, run: &str) -> Value {
    let out = Command::new(env!("CARGO_BIN_EXE_contextful")).args(["run", "show", run, "--project", "research"]).current_dir(dir).output().unwrap();
    let shown: Value = serde_json::from_str(&ok(&out)).unwrap();
    shown["trace_id"].clone()
}

/// A run a command opens takes as trace id the trace of the caller's `traceparent`, read from `TRACEPARENT` and
/// `TRACESTATE`, or a fresh trace; both run rows carry it.
// spec: run.record.trace-context@307606cc
#[test]
fn a_run_records_the_callers_trace_or_a_fresh_one() {
    let vendor = Vendor::start(|_| (200, "[{\"id\":\"a\"}]".into()));
    let dir = project(&manifest(&vendor));
    let parent = format!("00-{TRACE}-{PARENT}-01");
    ok(&fire(dir.path(), "traced", &[("TRACEPARENT", &parent), ("TRACESTATE", "vendor=1")]));
    assert_eq!(trace_of(dir.path(), "traced"), Value::from(TRACE));

    // An untraced run and a run under a malformed carrier each start a fresh trace.
    ok(&fire(dir.path(), "fresh", &[]));
    ok(&fire(dir.path(), "malformed", &[("TRACEPARENT", "00-not-a-trace-01")]));
    let (fresh, malformed) = (trace_of(dir.path(), "fresh"), trace_of(dir.path(), "malformed"));
    for id in [&fresh, &malformed] {
        let id = id.as_str().unwrap_or_default();
        assert!(id.len() == 32 && id.bytes().all(|b| b.is_ascii_hexdigit()) && id != TRACE, "{id}");
    }
    assert_ne!(fresh, malformed);

    // A pushed batch's run continues the caller's trace too.
    let dir = project("[[pipeline]]\nid = \"otel\"\ntables = [\"spans\"]\n[pipeline.source]\nname = \"receive\"\nconfig = { format = \"jsonl\", max_batch_bytes = 4096, max_queue_depth = 2, key_retention_secs = 3600 }\n");
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_contextful"));
    cmd.args(["pipeline", "receive", "otel", "--key", "k1", "--project", "research", "--site-id", "site-a", "--now", "2030-01-01T00:00:00Z"])
        .current_dir(dir.path())
        .env_remove("CONTEXTFUL_NODE_ID")
        .env_remove("CONTEXTFUL_SECRETS_BACKEND")
        .env_remove("OTEL_EXPORTER_OTLP_ENDPOINT")
        .env_remove("OTEL_EXPORTER_OTLP_TRACES_ENDPOINT")
        .env("TRACEPARENT", &parent)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = cmd.spawn().unwrap();
    child.stdin.take().unwrap().write_all(b"{\"table\":\"spans\",\"row\":{\"id\":\"s1\"}}\n").unwrap();
    let ack: Value = serde_json::from_str(&ok(&child.wait_with_output().unwrap())).unwrap();
    let run = ack["tables"][0]["run_id"].as_str().unwrap().to_string();
    assert_eq!(trace_of(dir.path(), &run), Value::from(TRACE));
}

/// With an exporter configured, a run exports one span, child of the caller's span, carrying run id, pipeline, table,
/// status, row and byte counts, and the failure tag.
// spec: run.record.run-span@fa339f1b
#[test]
fn a_run_exports_its_span_under_the_callers_span() {
    let vendor = Vendor::start(|_| (200, "[{\"id\":\"a\"},{\"id\":\"b\"}]".into()));
    let collector = Collector::start();
    let dir = project(&manifest(&vendor));
    let parent = format!("00-{TRACE}-{PARENT}-01");
    let out = fire(dir.path(), "exported", &[("TRACEPARENT", &parent), ("OTEL_EXPORTER_OTLP_ENDPOINT", &collector.url), ("OTEL_SERVICE_NAME", "substrate")]);
    ok(&out);

    let spans = collector.spans();
    assert_eq!(spans.len(), 1, "{spans:?}");
    let span = &spans[0];
    assert_eq!((span["traceId"].as_str(), span["parentSpanId"].as_str()), (Some(TRACE), Some(PARENT)));
    assert_eq!(span["name"], "contextful.run");
    let attr = |key: &str| span["attributes"].as_array().unwrap().iter().find(|a| a["key"] == key).map(|a| a["value"].clone());
    assert_eq!(attr("contextful.run.id"), Some(serde_json::json!({ "stringValue": "exported" })));
    assert_eq!(attr("contextful.pipeline"), Some(serde_json::json!({ "stringValue": "shop" })));
    assert_eq!(attr("contextful.table"), Some(serde_json::json!({ "stringValue": "shop_orders" })));
    assert_eq!(attr("contextful.status"), Some(serde_json::json!({ "stringValue": "success" })));
    assert_eq!(attr("contextful.rows"), Some(serde_json::json!({ "intValue": "2" })));
    assert!(attr("contextful.bytes").is_some() && attr("error.type").is_none());
    assert_eq!(span["status"]["code"], 0);

    // A failing run carries its failure tag.
    let failing = Vendor::start(|_| (500, "{}".into()));
    let dir = project(&manifest(&failing));
    let failed = fire(dir.path(), "failing", &[("OTEL_EXPORTER_OTLP_ENDPOINT", &collector.url)]);
    assert!(!failed.status.success());
    let spans = collector.spans();
    let span = spans.iter().find(|s| s["attributes"].as_array().unwrap().iter().any(|a| a["value"]["stringValue"] == "failing")).expect("the failed run exports its span");
    assert!(span.get("parentSpanId").is_none(), "an untraced run roots its own trace");
    assert!(span["attributes"].as_array().unwrap().iter().any(|a| a["key"] == "error.type"), "{span}");
    assert_eq!(span["status"]["code"], 2);
}

const SQL: &str = r#"SELECT note_id, title FROM "research/notes" ORDER BY note_id"#;

/// A project holding one landed table, an issuer and a credential reading it; the issuer's public key and the
/// credential.
fn read_project() -> (tempfile::TempDir, String, String) {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path();
    std::fs::create_dir_all(p.join(".contextful")).unwrap();
    std::fs::write(p.join(".contextful/issuance.toml"), format!("default_audience = \"{AUD}\"\nmax_lifetime_secs = 3600\n")).unwrap();
    std::fs::write(p.join("contextful.toml"), "authoring_posture = \"per_request\"\n[[pipeline.tables]]\nname = \"research/notes\"\n").unwrap();
    std::fs::write(p.join("notes.jsonl"), "{\"note_id\":\"n1\",\"title\":\"Solar battery storage\"}\n{\"note_id\":\"n2\",\"title\":\"Hiring plan\"}\n").unwrap();
    stdout(&run(p, &["context", "land", "research/notes", "--project", "research", "--rows", "notes.jsonl", "--run-id", "run-0001", "--site-id", "site-a"]));
    let public = stdout(&run(p, &["token", "keygen", "--out", ".contextful/issuer.seed"]));
    let token = stdout(&run(
        p,
        &["token", "mint", "--issuer-key", ".contextful/issuer.seed", "--on-behalf-of", "user://dana@acme.example", "--zone", "on-prem:hq", "--table", "research/*", "--ttl", "600"],
    ));
    (dir, public, token)
}

fn read_call(meta: Option<&str>) -> Value {
    let mut params = serde_json::json!({ "name": "context.query", "arguments": { "sql": SQL } });
    if let Some(parent) = meta {
        params["_meta"] = serde_json::json!({ "traceparent": parent });
    }
    serde_json::json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/call", "params": params })
}

/// The spans the collector holds once `n` arrived, waiting at most 30 s.
fn awaited(collector: &Collector, n: usize) -> Vec<Value> {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
    loop {
        let spans = collector.spans();
        if spans.len() >= n || std::time::Instant::now() > deadline {
            return spans;
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
}

/// The one read span the collector received: a child of `PARENT` in `TRACE` carrying the statement's SHA-256 and the
/// row count, and a payload holding no statement text, subject member or row value.
fn assert_one_read_span(collector: &Collector) {
    let spans = awaited(collector, 1);
    assert!(!spans.is_empty(), "the read exports a span");
    assert_eq!(spans.len(), 1, "{spans:?}");
    let span = &spans[0];
    assert_eq!(span["name"], "contextful.read");
    assert_eq!((span["traceId"].as_str(), span["parentSpanId"].as_str()), (Some(TRACE), Some(PARENT)));
    assert_ne!(span["spanId"].as_str(), Some(PARENT));
    let attr = |key: &str| span["attributes"].as_array().unwrap().iter().find(|a| a["key"] == key).map(|a| a["value"].clone());
    let hash = format!("{:x}", <sha2::Sha256 as sha2::Digest>::digest(SQL.as_bytes()));
    assert_eq!(attr("contextful.query.hash"), Some(serde_json::json!({ "stringValue": hash })));
    assert_eq!(attr("contextful.result.rows"), Some(serde_json::json!({ "intValue": "2" })));
    assert_eq!(attr("contextful.tool"), Some(serde_json::json!({ "stringValue": "context.query" })));
    let payload = collector.seen.lock().unwrap().iter().map(|(_, body)| String::from_utf8_lossy(body).into_owned()).collect::<String>();
    for secret in ["SELECT", "note_id", "Solar battery storage", "Hiring plan", "dana@acme.example", "on-prem:hq"] {
        assert!(!payload.contains(secret), "the exported payload discloses `{secret}`: {payload}");
    }
}

/// A `context.query` over `serve` carrying the `traceparent` header exports one `contextful.read` span, child of the
/// caller's span, with the statement's SHA-256 and the row count and without the statement text.
// spec: disclosure.record.face-carrier@9ca5cd00
// spec: disclosure.record.read-span@af58a898
// spec: disclosure.record.read-span-content@88bf0597
#[test]
fn a_served_read_under_a_traceparent_header_exports_one_child_span() {
    let (dir, public, token) = read_project();
    let collector = Collector::start();
    let mut env = vec![("OTEL_EXPORTER_OTLP_ENDPOINT", collector.url.as_str())];
    env.extend(PROXIES.iter().map(|k| (*k, "")));
    let (_listener, addr) = serve_env(dir.path(), &["serve", "--http", "127.0.0.1:0", "--audience", AUD, "--max-in-flight", "2", "--project", "research", "--public-key", &public], &env);

    let body = read_call(None).to_string();
    let mut s = TcpStream::connect(&addr).unwrap();
    write!(s, "POST /mcp HTTP/1.1\r\nAuthorization: Bearer {token}\r\ntraceparent: 00-{TRACE}-{PARENT}-01\r\ntracestate: vendor=1\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}", body.len()).unwrap();
    let mut raw = String::new();
    s.read_to_string(&mut raw).unwrap();
    let (head, answer) = raw.split_once("\r\n\r\n").unwrap();
    assert!(head.starts_with("HTTP/1.1 200"), "{raw}");
    let answer: Value = serde_json::from_str(answer).unwrap();
    assert_eq!(answer["result"]["structuredContent"]["rows"].as_array().map(Vec::len), Some(2), "{answer}");
    assert_one_read_span(&collector);
}

/// A `context.query` over `mcp` stdio carrying `params._meta.traceparent` exports one `contextful.read` span, child of
/// the caller's span, with the statement's SHA-256 and the row count and without the statement text.
// spec: disclosure.record.face-carrier@9ca5cd00
// spec: disclosure.record.read-span@af58a898
// spec: disclosure.record.read-span-content@88bf0597
#[test]
fn a_stdio_read_under_a_meta_traceparent_exports_one_child_span() {
    let (dir, public, token) = read_project();
    let collector = Collector::start();
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_contextful"));
    cmd.args(["mcp", "--project", "research", "--public-key", &public, "--audience", AUD])
        .current_dir(dir.path())
        .env("CONTEXTFUL_TOKEN", &token)
        .env("OTEL_EXPORTER_OTLP_ENDPOINT", &collector.url)
        .env_remove("TRACEPARENT")
        .env_remove("TRACESTATE")
        .env_remove("OTEL_EXPORTER_OTLP_TRACES_ENDPOINT")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    for k in PROXIES {
        cmd.env_remove(k);
    }
    let mut child = cmd.spawn().unwrap();
    writeln!(child.stdin.take().unwrap(), "{}", read_call(Some(&format!("00-{TRACE}-{PARENT}-01")))).unwrap();
    let out = child.wait_with_output().unwrap();
    let answer: Value = serde_json::from_str(ok(&out).lines().next().unwrap()).unwrap();
    assert_eq!(answer["result"]["structuredContent"]["rows"].as_array().map(Vec::len), Some(2), "{answer}");
    assert_one_read_span(&collector);
}
