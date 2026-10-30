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

pub(super) const TRACE: &str = "4bf92f3577b34da6a3ce929d0e0e4736";
pub(super) const PARENT: &str = "00f067aa0ba902b7";
const PROXIES: [&str; 6] = ["HTTPS_PROXY", "https_proxy", "HTTP_PROXY", "http_proxy", "ALL_PROXY", "all_proxy"];

/// The `(target, content type, body)` of each request a collector received.
type Received = Arc<Mutex<Vec<(String, String, Vec<u8>)>>>;

/// A loopback collector answering every POST with 200, keeping each `(target, content type, body)`.
pub(super) struct Collector {
    pub(super) url: String,
    seen: Received,
}

impl Collector {
    pub(super) fn start() -> Collector {
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
                let (mut length, mut content_type) = (0, String::new());
                loop {
                    let mut h = String::new();
                    if reader.read_line(&mut h).unwrap_or(0) == 0 || h.trim().is_empty() {
                        break;
                    }
                    if let Some((k, v)) = h.split_once(':') {
                        if k.trim().eq_ignore_ascii_case("content-length") {
                            length = v.trim().parse().unwrap_or(0);
                        } else if k.trim().eq_ignore_ascii_case("content-type") {
                            content_type = v.trim().to_string();
                        }
                    }
                }
                let mut body = vec![0; length];
                let _ = reader.read_exact(&mut body);
                keep.lock().unwrap().push((target, content_type, body));
                let _ = write!(stream, "HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{{}}");
            }
        });
        Collector { url, seen }
    }

    /// Every span the collector received at `/v1/traces`, a protobuf body read in its OTLP JSON shape.
    pub(super) fn spans(&self) -> Vec<Value> {
        let seen = self.seen.lock().unwrap();
        let mut out = Vec::new();
        for (target, content_type, body) in seen.iter() {
            assert_eq!(target, "/v1/traces");
            let v: Value = match content_type.as_str() {
                "application/x-protobuf" => contextful_acceptance::otlp::traces_json(body).unwrap(),
                _ => serde_json::from_slice(body).unwrap(),
            };
            for resource in v["resourceSpans"].as_array().unwrap() {
                for scope in resource["scopeSpans"].as_array().unwrap() {
                    out.extend(scope["spans"].as_array().unwrap().iter().cloned());
                }
            }
        }
        out
    }

    /// The content type of each request the collector received.
    fn content_types(&self) -> Vec<String> {
        self.seen.lock().unwrap().iter().map(|(_, content_type, _)| content_type.clone()).collect()
    }
}

fn manifest(vendor: &Vendor) -> String {
    format!("site_id = \"site-m\"\n\n[[pipeline]]\nid = \"shop\"\n\ntables = [\"orders\"]\n[pipeline.source]\nname = \"http\"\nconfig = {{ endpoint = \"{}\" }}\n", vendor.url("/v1/{table}"))
}

fn fire(dir: &Path, run: &str, env: &[(&str, &str)]) -> Output {
    traced(dir, &["pipeline", "run", "shop", "--project", "research", "--run-id", run, "--now", "2030-01-01T00:00:00Z"], env)
}

/// The binary run with `args` in `dir` under exactly the trace and exporter variables `env` sets, no proxy reaching
/// the loopback collector.
pub(super) fn traced(dir: &Path, args: &[&str], env: &[(&str, &str)]) -> Output {
    traced_by(Path::new(env!("CARGO_BIN_EXE_contextful")), dir, args, env)
}

/// `binary` run as [`traced`] runs the stock one.
fn traced_by(binary: &Path, dir: &Path, args: &[&str], env: &[(&str, &str)]) -> Output {
    let mut cmd = Command::new(binary);
    cmd.args(args)
        .current_dir(dir)
        .env_remove("CONTEXTFUL_ISSUER_PUBKEY")
        .env_remove("CONTEXTFUL_AUDIENCE")
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

/// With an exporter configured, a run exports one `contextful.run` span, child of the caller's span, carrying run id,
/// pipeline, table, status, row and byte counts, and the failure tag.
// spec: run.record.run-span@10f15139
#[test]
fn a_run_exports_its_span_under_the_callers_span() {
    let vendor = Vendor::start(|_| (200, "[{\"id\":\"a\"},{\"id\":\"b\"}]".into()));
    let collector = Collector::start();
    let dir = project(&manifest(&vendor));
    let parent = format!("00-{TRACE}-{PARENT}-01");
    let out = fire(dir.path(), "exported", &[("TRACEPARENT", &parent), ("OTEL_EXPORTER_OTLP_ENDPOINT", &collector.url), ("OTEL_SERVICE_NAME", "substrate")]);
    ok(&out);

    let spans = named(&collector.spans(), "contextful.run");
    assert_eq!(spans.len(), 1, "{spans:?}");
    let span = &spans[0];
    assert_eq!((span["traceId"].as_str(), span["parentSpanId"].as_str()), (Some(TRACE), Some(PARENT)));
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
    let spans = named(&collector.spans(), "contextful.run");
    let span = spans.iter().find(|s| s["attributes"].as_array().unwrap().iter().any(|a| a["value"]["stringValue"] == "failing")).expect("the failed run exports its span");
    assert!(span.get("parentSpanId").is_none(), "an untraced run roots its own trace");
    assert!(span["attributes"].as_array().unwrap().iter().any(|a| a["key"] == "error.type"), "{span}");
    assert_eq!(span["status"]["code"], 2);
}

/// A traced run's spans as `collector` receives them: every request's content type, and the run span.
fn exported_run(collector: &Collector) -> (Vec<String>, Value) {
    let vendor = Vendor::start(|_| (200, "[{\"id\":\"a\"}]".into()));
    let dir = project(&manifest(&vendor));
    let parent = format!("00-{TRACE}-{PARENT}-01");
    ok(&fire(dir.path(), "encoded", &[("TRACEPARENT", &parent), ("OTEL_EXPORTER_OTLP_ENDPOINT", &collector.url)]));
    let spans = named(&collector.spans(), "contextful.run");
    assert_eq!(spans.len(), 1, "{spans:?}");
    (collector.content_types(), spans[0].clone())
}

/// A build linking the `otlp` feature sends each batch as an OTLP protobuf `ExportTraceServiceRequest` under
/// `Content-Type: application/x-protobuf`; a build without it sends the OTLP JSON mapping under `application/json`.
// spec: disclosure.record.span-encoding@bf09336e
#[cfg(feature = "otlp")]
#[test]
fn a_build_linking_otlp_exports_the_run_span_as_protobuf() {
    let collector = Collector::start();
    let (content_types, span) = exported_run(&collector);
    assert!(!content_types.is_empty());
    assert!(content_types.iter().all(|c| c == "application/x-protobuf"), "{content_types:?}");
    assert_eq!((span["traceId"].as_str(), span["parentSpanId"].as_str()), (Some(TRACE), Some(PARENT)));
    assert_eq!(attribute(&span, "contextful.run.id"), Some(serde_json::json!({ "stringValue": "encoded" })));
    assert_eq!(attribute(&span, "contextful.pipeline"), Some(serde_json::json!({ "stringValue": "shop" })));
    assert_eq!(attribute(&span, "contextful.rows"), Some(serde_json::json!({ "intValue": "1" })));
}

/// A build linking the `otlp` feature sends each batch as an OTLP protobuf `ExportTraceServiceRequest` under
/// `Content-Type: application/x-protobuf`; a build without it sends the OTLP JSON mapping under `application/json`.
// spec: disclosure.record.span-encoding@bf09336e
#[cfg(not(feature = "otlp"))]
#[test]
fn a_build_without_otlp_exports_the_run_span_as_json() {
    let collector = Collector::start();
    let (content_types, span) = exported_run(&collector);
    assert!(!content_types.is_empty());
    assert!(content_types.iter().all(|c| c == "application/json"), "{content_types:?}");
    assert_eq!(attribute(&span, "contextful.run.id"), Some(serde_json::json!({ "stringValue": "encoded" })));
}

/// The spans named `name`.
pub(super) fn named(spans: &[Value], name: &str) -> Vec<Value> {
    spans.iter().filter(|s| s["name"] == name).cloned().collect()
}

/// The value of `span`'s attribute `key`.
pub(super) fn attribute(span: &Value, key: &str) -> Option<Value> {
    span["attributes"].as_array().and_then(|a| a.iter().find(|a| a["key"] == key)).map(|a| a["value"].clone())
}

/// The spans the collector holds once `done` holds of them, waiting at most 30 s.
pub(super) fn awaited_until(collector: &Collector, done: impl Fn(&[Value]) -> bool) -> Vec<Value> {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
    loop {
        let spans = collector.spans();
        if done(&spans) || std::time::Instant::now() > deadline {
            return spans;
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
}

/// A run exports one child span of its run span per pull, landing, commit and journaled step, each carrying its
/// stage, its table or step label, its outcome and the rows or bytes it counts, and no row value.
// spec: disclosure.record.stage-span@5b32d1b1
// spec: disclosure.record.stage-attributes@32230807
// spec: disclosure.record.stage-content@2b811f08
#[test]
fn a_run_exports_one_child_span_per_stage_under_its_run_span() {
    let vendor = Vendor::start(|_| (200, "[{\"id\":\"zebra-canary\"},{\"id\":\"yak-canary\"}]".into()));
    let collector = Collector::start();
    let dir = project(&manifest(&vendor));
    let parent = format!("00-{TRACE}-{PARENT}-01");
    ok(&fire(dir.path(), "staged", &[("TRACEPARENT", &parent), ("OTEL_EXPORTER_OTLP_ENDPOINT", &collector.url)]));

    let spans = collector.spans();
    assert!(!spans.is_empty(), "the run exports spans");
    let runs = named(&spans, "contextful.run");
    assert_eq!(runs.len(), 1, "{spans:?}");
    let run = runs[0]["spanId"].as_str().unwrap().to_string();
    let children: Vec<&Value> = spans.iter().filter(|s| s["name"] != "contextful.run").collect();
    assert!(!children.is_empty(), "the run exports its stage spans");
    for child in &children {
        assert_eq!((child["traceId"].as_str(), child["parentSpanId"].as_str()), (Some(TRACE), Some(run.as_str())), "{child}");
    }
    for (stage, name) in [("pull", "contextful.pull"), ("land", "contextful.land"), ("commit", "contextful.commit"), ("step", "contextful.step")] {
        let of = named(&spans, name);
        assert!(!of.is_empty(), "the run exports a `{name}` span: {spans:?}");
        for span in &of {
            assert_eq!(attribute(span, "contextful.stage"), Some(serde_json::json!({ "stringValue": stage })), "{span}");
            assert_eq!(attribute(span, "contextful.table"), Some(serde_json::json!({ "stringValue": "shop_orders" })), "{span}");
            assert!(attribute(span, "contextful.outcome").is_some() && attribute(span, "error.type").is_none(), "{span}");
        }
    }
    for name in ["contextful.pull", "contextful.step"] {
        let of = named(&spans, name);
        assert!(!of.is_empty());
        assert!(of.iter().all(|s| attribute(s, "contextful.step") == Some(serde_json::json!({ "stringValue": "pull-0" }))), "{spans:?}");
    }
    let pulls = named(&spans, "contextful.pull");
    assert!(!pulls.is_empty());
    assert!(pulls.iter().all(|s| attribute(s, "contextful.bytes").is_some()), "{pulls:?}");
    assert_eq!(attribute(&named(&spans, "contextful.commit")[0], "contextful.rows"), Some(serde_json::json!({ "intValue": "2" })));
    assert_eq!(attribute(&named(&spans, "contextful.land")[0], "contextful.rows"), Some(serde_json::json!({ "intValue": "2" })));
    let payload = collector.seen.lock().unwrap().iter().map(|(_, _, body)| String::from_utf8_lossy(body).into_owned()).collect::<String>();
    for value in ["zebra-canary", "yak-canary"] {
        assert!(!payload.contains(value), "the exported payload discloses `{value}`");
    }
}

/// A delivery to the served receive route under a `traceparent` header exports one server span under the caller's
/// span, and each run it lands continues that trace with its run span a child of the route span.
// spec: disclosure.record.route-carrier@6d61a8b7
// spec: disclosure.record.route-span@bbc5bbe4
// spec: disclosure.record.route-parent@feca35a5
#[test]
fn a_served_delivery_continues_the_callers_trace_through_its_route_span() {
    let (dir, public) = crate::serve::receiving_face_with(2, 4096);
    let root = dir.path();
    let token = crate::serve::mint_write(root, "otel_*");
    let collector = Collector::start();
    let mut env = vec![("OTEL_EXPORTER_OTLP_ENDPOINT", collector.url.as_str())];
    env.extend(PROXIES.iter().map(|k| (*k, "")));
    let (_listener, addr) = serve_env(root, &["serve", "--http", "127.0.0.1:0", "--audience", AUD, "--max-in-flight", "2", "--project", "research", "--public-key", &public], &env);
    let ack = deliver_traced(&addr, &token);
    assert_eq!(ack["committed"], true, "{ack}");

    let spans = awaited_until(&collector, |spans| !named(spans, "POST /receive/:pipeline").is_empty() && named(spans, "contextful.run").len() >= 2);
    let routes = named(&spans, "POST /receive/:pipeline");
    assert_eq!(routes.len(), 1, "{spans:?}");
    let route = &routes[0];
    assert_eq!((route["traceId"].as_str(), route["parentSpanId"].as_str()), (Some(TRACE), Some(PARENT)));
    assert_eq!(attribute(route, "http.route"), Some(serde_json::json!({ "stringValue": "/receive/:pipeline" })));
    assert_eq!(attribute(route, "http.response.status_code"), Some(serde_json::json!({ "intValue": "200" })));
    let runs = named(&spans, "contextful.run");
    assert_eq!(runs.len(), 2, "{spans:?}");
    for run in &runs {
        assert_eq!((run["traceId"].as_str(), run["parentSpanId"].as_str()), (Some(TRACE), route["spanId"].as_str()), "{run}");
    }
}

/// With no exporter configured the face hands the receive route the caller's own `traceparent`, so each run it lands
/// records the caller's trace, and no span leaves the process.
// spec: disclosure.record.stage-unconfigured@682fe4bd
// spec: disclosure.record.route-carrier@6d61a8b7
#[test]
fn without_an_exporter_a_served_delivery_records_the_callers_trace_and_exports_nothing() {
    let (dir, public) = crate::serve::receiving_face_with(2, 4096);
    let root = dir.path();
    let token = crate::serve::mint_write(root, "otel_*");
    let collector = Collector::start();
    let env: Vec<(&str, &str)> = PROXIES.iter().map(|k| (*k, "")).collect();
    let (_listener, addr) = serve_env(root, &["serve", "--http", "127.0.0.1:0", "--audience", AUD, "--max-in-flight", "2", "--project", "research", "--public-key", &public], &env);
    let ack = deliver_traced(&addr, &token);
    let tables = ack["tables"].as_array().cloned().unwrap_or_default();
    assert!(!tables.is_empty(), "the delivery lands its tables: {ack}");
    for table in &tables {
        assert_eq!(trace_of(root, table["run_id"].as_str().unwrap()), Value::from(TRACE), "{ack}");
    }
    assert!(collector.seen.lock().unwrap().is_empty(), "no span leaves the process");
}

/// `POST /receive/otel` of one row per table under a `traceparent` naming `PARENT`; the JSON answer.
fn deliver_traced(addr: &str, token: &str) -> Value {
    deliver_traced_as(addr, token, "")
}

/// [`deliver_traced`] with the extra header lines `extra`, answering `202` when they ask for the queued answer.
fn deliver_traced_as(addr: &str, token: &str, extra: &str) -> Value {
    let body = format!("{}{}", crate::serve::delivered("spans", serde_json::json!({ "id": "s1" })), crate::serve::delivered("events", serde_json::json!({ "id": "e1" })));
    let mut stream = TcpStream::connect(addr).unwrap();
    write!(
        stream,
        "POST /receive/otel HTTP/1.1\r\nAuthorization: Bearer {token}\r\nContextful-Batch-Key: traced-1\r\ntraceparent: 00-{TRACE}-{PARENT}-01\r\n{extra}Content-Type: application/x-ndjson\r\nContent-Length: {}\r\n\r\n{body}",
        body.len()
    )
    .unwrap();
    let mut raw = String::new();
    stream.read_to_string(&mut raw).unwrap();
    let (head, answer) = raw.split_once("\r\n\r\n").unwrap_or_else(|| panic!("no answer head: {raw:?}"));
    let status = if extra.contains("respond-async") { "HTTP/1.1 202" } else { "HTTP/1.1 200" };
    assert!(head.starts_with(status), "{raw}");
    serde_json::from_str(answer).unwrap()
}

/// The `receive` messages waiting in the project's local spool.
fn queued_messages(root: &Path) -> Vec<Value> {
    let Ok(entries) = std::fs::read_dir(root.join(".contextful/queue/research")) else { return Vec::new() };
    entries
        .flatten()
        .filter(|e| e.path().extension().is_some_and(|x| x == "json"))
        .filter_map(|e| serde_json::from_slice::<Value>(&std::fs::read(e.path()).ok()?).ok())
        .filter(|m| m["message"]["kind"] == "receive")
        .map(|m| m["message"].clone())
        .collect()
}

/// A delivery queued under `Prefer: respond-async` carries its route span's `traceparent` and `tracestate` on its
/// message and no other request header; each run a later drain lands continues the request's trace, its run span a
/// child of the route span and its stage spans children of the run span.
// spec: disclosure.record.queued-parent@a6340388
// spec: disclosure.record.queued-content@a8b093ef
#[test]
fn a_queued_delivery_lands_under_its_route_span_when_drained() {
    let (dir, public) = crate::serve::receiving_face_with(2, 4096);
    let root = dir.path();
    let token = crate::serve::mint_write(root, "otel_*");
    let collector = Collector::start();
    let mut env = vec![("OTEL_EXPORTER_OTLP_ENDPOINT", collector.url.as_str())];
    env.extend(PROXIES.iter().map(|k| (*k, "")));
    let (_listener, addr) = serve_env(root, &["serve", "--http", "127.0.0.1:0", "--audience", AUD, "--max-in-flight", "2", "--project", "research", "--public-key", &public], &env);
    let receipt = deliver_traced_as(&addr, &token, "tracestate: vendor=1\r\nPrefer: respond-async\r\nX-Canary: zebra-canary\r\n");
    assert_eq!(receipt["queued"], true, "{receipt}");

    let messages = queued_messages(root);
    assert_eq!(messages.len(), 1, "{messages:?}");
    let trace = &messages[0]["trace"];
    let keys: Vec<&str> = trace.as_object().map(|o| o.keys().map(String::as_str).collect()).unwrap_or_default();
    assert_eq!(keys, ["traceparent", "tracestate"], "{trace}");
    assert_eq!(trace["tracestate"], "vendor=1");
    let carried = trace["traceparent"].as_str().unwrap_or_default().to_string();
    assert!(carried.starts_with(&format!("00-{TRACE}-")) && !carried.contains(PARENT), "the message names the route span: {carried}");
    assert!(!messages[0].to_string().contains("zebra-canary"), "the message carries no other request header");

    let drained = traced(root, &["queue", "drain", "--project", "research", "--public-key", &public, "--audience", AUD], &[("OTEL_EXPORTER_OTLP_ENDPOINT", &collector.url)]);
    assert!(stdout(&drained).contains("1 batch"), "{}", String::from_utf8_lossy(&drained.stdout));
    let spans = awaited_until(&collector, |spans| !named(spans, "POST /receive/:pipeline").is_empty() && named(spans, "contextful.run").len() >= 2);
    let routes = named(&spans, "POST /receive/:pipeline");
    assert_eq!(routes.len(), 1, "{spans:?}");
    let route = &routes[0];
    assert_eq!((route["traceId"].as_str(), route["parentSpanId"].as_str()), (Some(TRACE), Some(PARENT)));
    assert_eq!(attribute(route, "http.response.status_code"), Some(serde_json::json!({ "intValue": "202" })));
    assert_eq!(carried, format!("00-{TRACE}-{}-01", route["spanId"].as_str().unwrap_or_default()));
    let runs = named(&spans, "contextful.run");
    assert_eq!(runs.len(), 2, "{spans:?}");
    for run in &runs {
        assert_eq!((run["traceId"].as_str(), run["parentSpanId"].as_str()), (Some(TRACE), route["spanId"].as_str()), "{run}");
    }
    let commits = named(&spans, "contextful.commit");
    assert!(!commits.is_empty(), "each drained run exports its stage spans: {spans:?}");
    let ids: Vec<&str> = runs.iter().filter_map(|r| r["spanId"].as_str()).collect();
    assert!(commits.iter().all(|c| c["parentSpanId"].as_str().is_some_and(|p| ids.contains(&p))), "{commits:?}");
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
    let payload = collector.seen.lock().unwrap().iter().map(|(_, _, body)| String::from_utf8_lossy(body).into_owned()).collect::<String>();
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

/// The `contextful.run` span run `run` exported.
fn run_span(spans: &[Value], run: &str) -> Value {
    named(spans, "contextful.run")
        .into_iter()
        .find(|s| attribute(s, "contextful.run.id") == Some(serde_json::json!({ "stringValue": run })))
        .unwrap_or_else(|| panic!("run `{run}` exported no span: {spans:?}"))
}

/// The store-driven embedding binary run as [`traced`], holding `token` under `public`.
fn hosted(dir: &Path, public: &str, token: &str, args: &[&str], env: &[(&str, &str)]) -> Output {
    let mut vars = vec![("CONTEXTFUL_TOKEN", token), ("CONTEXTFUL_ISSUER_PUBKEY", public), ("CONTEXTFUL_AUDIENCE", crate::job::AUD)];
    vars.extend_from_slice(env);
    traced_by(&crate::job::host_binary(), dir, args, &vars)
}

/// A job fire's execution and its output table run take the fire caller's trace, the table run's span a child of the
/// execution's run span; an untraced fire starts a fresh trace.
// spec: run.record.job-trace@8c92821a
#[test]
fn a_job_fire_continues_its_callers_trace_or_starts_a_fresh_one() {
    let (dir, public, token) = crate::job::project(&crate::job::job("max_in_flight = 1\n"));
    let p = dir.path();
    let collector = Collector::start();
    let parent = format!("00-{TRACE}-{PARENT}-01");
    let fire = |run: &str, now: &str, env: &[(&str, &str)]| {
        let mut vars = vec![("OTEL_EXPORTER_OTLP_ENDPOINT", collector.url.as_str())];
        vars.extend_from_slice(env);
        let args = ["job", "fire", "score-documents", "--project", "research", "--run-id", run, "--site-id", "site", "--now", now];
        hosted(p, &public, &token, &args, &vars)
    };
    ok(&fire("traced", "2030-01-01T00:01:00Z", &[("TRACEPARENT", &parent)]));
    assert_eq!(trace_of(p, "traced"), Value::from(TRACE));
    assert_eq!(trace_of(p, "traced.scores"), Value::from(TRACE));
    let spans = collector.spans();
    let execution = run_span(&spans, "traced");
    assert_eq!((execution["traceId"].as_str(), execution["parentSpanId"].as_str()), (Some(TRACE), Some(PARENT)));
    let table = run_span(&spans, "traced.scores");
    assert_eq!((table["traceId"].as_str(), table["parentSpanId"].as_str()), (Some(TRACE), execution["spanId"].as_str()));

    ok(&fire("fresh", "2030-01-01T00:02:00Z", &[]));
    let fresh = trace_of(p, "fresh");
    let id = fresh.as_str().unwrap_or_default().to_string();
    assert!(id.len() == 32 && id.bytes().all(|b| b.is_ascii_hexdigit()) && id != TRACE, "{id}");
    assert_eq!(trace_of(p, "fresh.scores"), fresh);
    let spans = collector.spans();
    let execution = run_span(&spans, "fresh");
    assert_eq!(execution["traceId"].as_str(), Some(id.as_str()));
    assert!(execution.get("parentSpanId").is_none() && execution.get("links").is_none(), "{execution}");
    assert_eq!(run_span(&spans, "fresh.scores")["parentSpanId"], execution["spanId"]);
}

/// The run spans of the job executions a fire opened: a run id carrying no table suffix.
fn executions(spans: &[Value]) -> Vec<Value> {
    named(spans, "contextful.run")
        .into_iter()
        .filter(|s| attribute(s, "contextful.run.id").and_then(|v| v["stringValue"].as_str().map(|r| !r.contains('.'))).unwrap_or(false))
        .collect()
}

/// An import under a trace context records its W3C carriers with the job it adds, a pipeline-only apply keeps them,
/// and an untraced apply changing the job records none. Each scheduled fire starts a fresh trace whatever trace serve
/// runs under, its execution's run span linking the job's recorded carrier when one is recorded.
// spec: disclosure.record.job-carrier@2cbf1367
// spec: disclosure.record.job-link@42684955
// spec: disclosure.record.scheduled-trace@39a43178
#[test]
fn a_scheduled_fire_starts_a_fresh_trace_linked_to_its_submitter() {
    let declaration = format!("site_id = \"site\"\n{}", crate::job::job("max_in_flight = 1\nschedule = \"every 1m\"\n"));
    let (dir, public, token) = crate::job::project(&declaration);
    let p = dir.path();
    std::fs::create_dir(p.join("pipelines")).unwrap();
    let feed = "id=\"feed\"\ntables=[\"items\"]\n[source]\nname=\"http\"\nconfig={endpoint=\"https://example.org/items\"}\n";
    std::fs::write(p.join("pipelines/feed.toml"), feed).unwrap();
    let collector = Collector::start();
    let submitter = format!("00-{TRACE}-{PARENT}-01");
    let serving = "00-0af7651916cd43dd8448eb211c80319c-b7ad6b7169203331-01";
    let host = |args: &[&str], env: &[(&str, &str)]| hosted(p, &public, &token, args, env);
    ok(&host(&["pipeline", "import", "--project", "research"], &[("TRACEPARENT", &submitter), ("TRACESTATE", "vendor=1")]));
    let recorded = std::fs::read_to_string(p.join(".contextful/control/research/trace@v1.json")).unwrap();
    assert_eq!(
        serde_json::from_str::<Value>(&recorded).unwrap(),
        serde_json::json!({ "score-documents": { "traceparent": submitter, "tracestate": "vendor=1" } })
    );
    std::fs::write(p.join("pipelines/feed.toml"), feed.replace("example.org/items", "example.org/updated")).unwrap();
    let partial = ok(&host(&["pipeline", "apply", "feed", "--project", "research"], &[]));
    assert!(partial.contains("applied v2"), "{partial}");

    let cycle = |now: &str| {
        let env = [("TRACEPARENT", serving), ("OTEL_EXPORTER_OTLP_ENDPOINT", collector.url.as_str())];
        let answer: Value = serde_json::from_str(&ok(&host(&["pipeline", "serve", "--cycle", "--project", "research", "--now", now], &env))).unwrap();
        assert_eq!(answer["fired"], serde_json::json!(["job:score-documents"]), "{answer}");
    };
    cycle("2030-01-01T00:01:00Z");
    let spans = collector.spans();
    let first = executions(&spans);
    assert_eq!(first.len(), 1, "{spans:?}");
    let first = &first[0];
    let trace = first["traceId"].as_str().unwrap().to_string();
    assert!(trace != TRACE && !serving.contains(&trace), "{first}");
    assert!(first.get("parentSpanId").is_none(), "{first}");
    assert_eq!(first["links"], serde_json::json!([{ "traceId": TRACE, "spanId": PARENT, "traceState": "vendor=1" }]));
    let tables: Vec<Value> = named(&spans, "contextful.run").into_iter().filter(|s| s["parentSpanId"] == first["spanId"]).collect();
    assert_eq!(tables.len(), 1, "{spans:?}");
    assert_eq!(tables[0]["traceId"].as_str(), Some(trace.as_str()));

    let changed = declaration.replace("max_in_flight = 1", "max_in_flight = 2");
    std::fs::write(p.join("contextful.toml"), format!("authoring_posture = \"per_request\"\n{changed}")).unwrap();
    let full = ok(&host(&["pipeline", "apply", "--project", "research"], &[]));
    assert!(full.contains("applied v3"), "{full}");
    cycle("2030-01-01T00:02:00Z");
    let spans = collector.spans();
    let fires = executions(&spans);
    assert_eq!(fires.len(), 2, "{spans:?}");
    let second = fires.iter().find(|s| s["spanId"] != first["spanId"]).unwrap();
    assert!(second["traceId"].as_str() != Some(trace.as_str()) && second["traceId"].as_str() != Some(TRACE), "{second}");
    assert!(second.get("parentSpanId").is_none() && second.get("links").is_none(), "{second}");
}
