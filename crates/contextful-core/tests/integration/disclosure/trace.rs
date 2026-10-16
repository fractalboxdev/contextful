//! The W3C trace carrier a caller hands in, and the OTLP body a span becomes.

use contextful_core::trace::{otlp_traces, Span, SpanKind, TraceContext};
use serde_json::json;

const TRACE: &str = "4bf92f3577b34da6a3ce929d0e0e4736";
const PARENT: &str = "00f067aa0ba902b7";

/// A well-formed W3C `traceparent` names the trace and the parent of the operation's span; a malformed one is ignored
/// and the operation starts a fresh trace.
// spec: disclosure.record.trace-carrier@763fa9ff
#[test]
fn a_well_formed_traceparent_names_the_parent_and_a_malformed_one_is_ignored() {
    let caller = TraceContext::parse(&format!("00-{TRACE}-{PARENT}-01"), Some(" vendor=a,other=b ")).unwrap();
    assert_eq!((caller.trace_hex(), caller.span_hex(), caller.flags), (TRACE.to_string(), PARENT.to_string(), 1));
    assert_eq!(caller.state.as_deref(), Some("vendor=a,other=b"));
    let child = caller.child([7; 8]);
    assert_eq!(child.traceparent(), format!("00-{TRACE}-0707070707070707-01"));
    let span = Span::at(&child, Some(caller.span_id), "contextful.run", SpanKind::Internal);
    assert_eq!((span.trace_id, span.parent), (caller.trace_id, Some(caller.span_id)));

    // A later version may append fields after a dash.
    assert!(TraceContext::parse(&format!("01-{TRACE}-{PARENT}-00-extra"), None).is_some());
    for malformed in [
        String::new(),
        "garbage".into(),
        format!("00-{TRACE}-{PARENT}-01-extra"),
        format!("ff-{TRACE}-{PARENT}-01"),
        format!("00-{}-{PARENT}-01", "0".repeat(32)),
        format!("00-{TRACE}-{}-01", "0".repeat(16)),
        format!("00-{}-{PARENT}-01", TRACE.to_uppercase()),
        format!("00-{TRACE}-{PARENT}-1"),
        format!("00_{TRACE}_{PARENT}_01"),
        format!("00-{}-{PARENT}-01", &TRACE[..31]),
    ] {
        assert_eq!(TraceContext::parse(&malformed, None), None, "`{malformed}` parsed");
    }
    // An oversized trace state is dropped whole; the parent still holds.
    let long = "k=".to_string() + &"v".repeat(600);
    assert_eq!(TraceContext::parse(&format!("00-{TRACE}-{PARENT}-01"), Some(&long)).unwrap().state, None);
}

#[test]
fn a_span_becomes_one_otlp_json_span_under_its_service() {
    let caller = TraceContext::parse(&format!("00-{TRACE}-{PARENT}-01"), None).unwrap();
    let mut span = Span::at(&caller.child([1; 8]), Some(caller.span_id), "contextful.run", SpanKind::Internal).with("contextful.status", "failed").with("contextful.rows", 0u64);
    span.error = Some("transient".into());
    span.start_unix_nanos = 5;
    span.end_unix_nanos = 9;
    let body = otlp_traces("svc", &[span]);
    let out = &body["resourceSpans"][0]["scopeSpans"][0]["spans"][0];
    assert_eq!(out["traceId"], json!(TRACE));
    assert_eq!(out["parentSpanId"], json!(PARENT));
    assert_eq!(out["spanId"], json!("0101010101010101"));
    assert_eq!(out["kind"], json!(1));
    assert_eq!(out["status"], json!({ "code": 2, "message": "transient" }));
    assert_eq!(out["attributes"], json!([
        { "key": "contextful.status", "value": { "stringValue": "failed" } },
        { "key": "contextful.rows", "value": { "intValue": "0" } },
    ]));
    let root = Span::at(&caller, None, "root", SpanKind::Server);
    assert!(otlp_traces("svc", &[root])["resourceSpans"][0]["scopeSpans"][0]["spans"][0].get("parentSpanId").is_none());
}
