//! The span exporter against a loopback collector, an absent configuration and a collector that never answers.

use crate::support::{proxy_env, Response, Server};
use contextful_acceptance::otlp::traces_json;
use contextful_core::trace::{otlp_traces, Span, SpanKind, SpanSink, TraceContext};
use contextful_outbound::telemetry::{fresh_trace, otlp_protobuf, span_id, SpanExporter, Wire, SPAN_QUEUE};
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::time::{Duration, Instant};

fn span(context: &TraceContext, name: &str) -> Span {
    let mut s = Span::at(&context.child(span_id()), Some(context.span_id), name, SpanKind::Internal).with("contextful.rows", 3u64);
    s.start_unix_nanos = 1_000;
    s.end_unix_nanos = 2_000;
    s
}

/// With `OTEL_EXPORTER_OTLP_ENDPOINT` set, spans leave over OTLP/HTTP to its `/v1/traces` path, or to
/// `OTEL_EXPORTER_OTLP_TRACES_ENDPOINT` as given, carrying the `OTEL_EXPORTER_OTLP_HEADERS` headers and
/// `OTEL_SERVICE_NAME` as `service.name`.
// spec: disclosure.record.span-export@27f71dd5
#[test]
fn spans_leave_as_otlp_json_to_the_configured_collector() {
    let _env = proxy_env();
    let collector = Server::start(|_| Response::json(200, "{}"));
    let vars = BTreeMap::from([
        ("OTEL_EXPORTER_OTLP_ENDPOINT", collector.url("/otel/")),
        ("OTEL_EXPORTER_OTLP_HEADERS", "x-tenant=acme,authorization=Basic%20abc".to_string()),
        ("OTEL_SERVICE_NAME", "evals-substrate".to_string()),
    ]);
    let exporter = SpanExporter::from_env(|k| vars.get(k).cloned(), Wire::Json).unwrap().expect("a configured endpoint builds an exporter");
    let caller = fresh_trace();
    exporter.record(span(&caller, "contextful.run"));
    exporter.shutdown(Duration::from_secs(10));
    assert_eq!((exporter.exported(), exporter.dropped()), (1, 0));

    let sent = collector.received("/otel/v1/traces");
    assert_eq!(sent.len(), 1);
    assert_eq!(sent[0].method, "POST");
    assert_eq!(sent[0].header("x-tenant"), Some("acme"));
    assert_eq!(sent[0].header("authorization"), Some("Basic abc"));
    assert_eq!(sent[0].header("content-type"), Some("application/json"));
    let body: Value = serde_json::from_slice(&sent[0].body).unwrap();
    let resource = &body["resourceSpans"][0];
    assert_eq!(resource["resource"]["attributes"][0], json!({ "key": "service.name", "value": { "stringValue": "evals-substrate" } }));
    let exported = &resource["scopeSpans"][0]["spans"][0];
    assert_eq!(exported["traceId"], json!(caller.trace_hex()));
    assert_eq!(exported["parentSpanId"], json!(caller.span_hex()));
    assert_eq!(exported["name"], json!("contextful.run"));
    assert_eq!((exported["startTimeUnixNano"].as_str(), exported["endTimeUnixNano"].as_str()), (Some("1000"), Some("2000")));
    assert_eq!(exported["attributes"][0], json!({ "key": "contextful.rows", "value": { "intValue": "3" } }));

    // A traces endpoint is used as given.
    let traces = BTreeMap::from([("OTEL_EXPORTER_OTLP_TRACES_ENDPOINT", collector.url("/exact")), ("OTEL_EXPORTER_OTLP_ENDPOINT", collector.url("/ignored"))]);
    let exporter = SpanExporter::from_env(|k| traces.get(k).cloned(), Wire::Json).unwrap().unwrap();
    exporter.record(span(&caller, "contextful.query"));
    exporter.shutdown(Duration::from_secs(10));
    assert_eq!(collector.received("/exact").len(), 1);
    assert!(collector.received("/ignored/v1/traces").is_empty());

    // Headers carry credentials, so a cleartext collector off loopback refuses the exporter at construction.
    let err = SpanExporter::new("http://collector.example/v1/traces", vec![("a".into(), "b".into())], "svc", Wire::Json).err().expect("a cleartext collector with headers builds no exporter");
    assert!(err.starts_with("SecretCleartextEndpoint"), "{err}");
    assert!(SpanExporter::new("http://collector.example/v1/traces", Vec::new(), "svc", Wire::Json).is_ok());
}

/// With neither endpoint set, no exporter exists: no span leaves the process and telemetry opens no socket.
// spec: disclosure.record.span-unconfigured@c203735e
#[test]
fn no_endpoint_builds_no_exporter() {
    assert!(SpanExporter::from_env(|_| None, Wire::Json).unwrap().is_none());
    let blank = |k: &str| (k == "OTEL_EXPORTER_OTLP_ENDPOINT" || k == "OTEL_SERVICE_NAME").then(|| "  ".to_string());
    assert!(SpanExporter::from_env(blank, Wire::Json).unwrap().is_none());
    let headers_alone = |k: &str| (k == "OTEL_EXPORTER_OTLP_HEADERS").then(|| "a=b".to_string());
    assert!(SpanExporter::from_env(headers_alone, Wire::Json).unwrap().is_none());
}

/// Spans leave off the operation's path through a queue of 2048 spans; a span past the cap, or lost to a failed
/// export, is dropped and counted.
// spec: disclosure.record.span-queue@8b6503a2
#[test]
fn a_collector_that_never_answers_adds_no_latency_and_drops_count() {
    let _env = proxy_env();
    // A collector that accepts and never answers holds the exporter's one request open.
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://127.0.0.1:{}", listener.local_addr().unwrap().port());
    std::thread::spawn(move || {
        let mut held = Vec::new();
        for stream in listener.incoming().flatten() {
            held.push(stream);
        }
    });
    let exporter = SpanExporter::new(&format!("{url}/v1/traces"), Vec::new(), "svc", Wire::Json).unwrap();
    let caller = fresh_trace();
    let total = 5_000;
    let started = Instant::now();
    for i in 0..total {
        exporter.record(span(&caller, &format!("s{i}")));
    }
    let spent = started.elapsed();
    assert!(spent < Duration::from_secs(1), "recording {total} spans took {spent:?}");
    // The queue refuses only when full, so it accepts at least its 2048; the worker holds at most one batch of 512
    // more; the rest are counted.
    assert_eq!(SPAN_QUEUE, 2048);
    let (floor, ceiling) = ((total - 2048 - 512) as u64, (total - 2048) as u64);
    assert!((floor..=ceiling).contains(&exporter.dropped()), "dropped {} of {total}, {floor}..={ceiling} expected", exporter.dropped());
    assert_eq!(exporter.exported(), 0);

    // An unreachable collector loses its batch, counted.
    let refused = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let gone = format!("http://127.0.0.1:{}/v1/traces", refused.local_addr().unwrap().port());
    drop(refused);
    let exporter = SpanExporter::new(&gone, Vec::new(), "svc", Wire::Json).unwrap();
    exporter.record(span(&caller, "lost"));
    exporter.shutdown(Duration::from_secs(10));
    assert_eq!((exporter.exported(), exporter.dropped()), (0, 1));
}

/// The spans a protobuf body carries: a failed child with every attribute kind and a span link holding trace state,
/// a root span whose interval runs backwards, and a span starting before the epoch with an integer past `i64`.
fn varied(caller: &TraceContext) -> Vec<Span> {
    let mut failed = span(caller, "contextful.pull")
        .with("contextful.table", "orders")
        .with("contextful.truncated", true)
        .with("contextful.tables", vec!["a".to_string(), "b".to_string()])
        .linked(&TraceContext { state: Some("vendor=1".into()), ..fresh_trace() });
    failed.error = Some("SourceUnreachable".into());
    let mut root = Span::at(&fresh_trace(), None, "contextful.run", SpanKind::Server);
    (root.start_unix_nanos, root.end_unix_nanos) = (5_000, 4_000);
    let mut model = Span::at(&caller.child(span_id()), Some(caller.span_id), "chat", SpanKind::Client).with("gen_ai.usage.input_tokens", u64::MAX);
    model.start_unix_nanos = -1;
    vec![failed, root, model]
}

/// A build linking the `otlp` feature sends each batch as an OTLP protobuf `ExportTraceServiceRequest` under
/// `Content-Type: application/x-protobuf`; a build without it sends the OTLP JSON mapping under `application/json`.
// spec: disclosure.record.span-encoding@bf09336e
#[test]
fn a_protobuf_body_carries_what_the_json_body_carries() {
    let caller = fresh_trace();
    let spans = varied(&caller);
    assert_eq!(spans.len(), 3);
    let decoded = traces_json(&otlp_protobuf("svc", &spans)).expect("the body is one protobuf ExportTraceServiceRequest");
    assert_eq!(decoded, otlp_traces("svc", &spans));
    assert_eq!(traces_json(&otlp_protobuf("svc", &[])).unwrap(), otlp_traces("svc", &[]));
}

/// A build linking the `otlp` feature sends each batch as an OTLP protobuf `ExportTraceServiceRequest` under
/// `Content-Type: application/x-protobuf`; a build without it sends the OTLP JSON mapping under `application/json`.
// spec: disclosure.record.span-encoding@bf09336e
#[test]
fn a_protobuf_exporter_posts_protobuf_to_the_collector() {
    let _env = proxy_env();
    let collector = Server::start(|_| Response::json(200, "{}"));
    let vars = BTreeMap::from([("OTEL_EXPORTER_OTLP_ENDPOINT", collector.url("")), ("OTEL_SERVICE_NAME", "evals-substrate".to_string())]);
    let exporter = SpanExporter::from_env(|k| vars.get(k).cloned(), Wire::Protobuf).unwrap().expect("a configured endpoint builds an exporter");
    let caller = fresh_trace();
    exporter.record(span(&caller, "contextful.run"));
    exporter.shutdown(Duration::from_secs(10));
    assert_eq!((exporter.exported(), exporter.dropped()), (1, 0));

    let sent = collector.received("/v1/traces");
    assert_eq!(sent.len(), 1);
    assert_eq!(sent[0].header("content-type"), Some("application/x-protobuf"));
    let body = traces_json(&sent[0].body).expect("the collector decodes a protobuf ExportTraceServiceRequest");
    let resource = &body["resourceSpans"][0];
    assert_eq!(resource["resource"]["attributes"][0], json!({ "key": "service.name", "value": { "stringValue": "evals-substrate" } }));
    let exported = &resource["scopeSpans"][0]["spans"][0];
    assert_eq!((&exported["traceId"], &exported["parentSpanId"], &exported["name"]), (&json!(caller.trace_hex()), &json!(caller.span_hex()), &json!("contextful.run")));
    assert_eq!(exported["attributes"][0], json!({ "key": "contextful.rows", "value": { "intValue": "3" } }));
}
