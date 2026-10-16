//! A run continuing the caller's W3C trace and exporting its span, through the built binary.

use crate::pipeline::{ok, project, Vendor};
use serde_json::Value;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::path::Path;
use std::process::{Command, Output};
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

/// A run's trace id is the trace of the caller's `traceparent`, read on the command line from `TRACEPARENT` and
/// `TRACESTATE`, or a fresh trace; both run rows carry it.
// spec: run.record.trace-context@75b1da01
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
