//! A read tool call continuing the caller's W3C trace and recording one span through the
//! sink the binary hands in.

use crate::mcp::{current, fixture};
use contextful_agent::mcp::{Caller, Carrier, ReadSpans, Server, Tools};
use contextful_core::ports::FixedClock;
use contextful_core::run::journal::sha256_hex;
use contextful_core::time::Instant;
use contextful_core::trace::{otlp_traces, Attr, Span, SpanSink, TraceContext};
use serde_json::{json, Value};
use std::sync::{Arc, Mutex};

const TRACE: &str = "4bf92f3577b34da6a3ce929d0e0e4736";
const META_PARENT: &str = "00f067aa0ba902b7";
const HEADER_PARENT: &str = "b7ad6b7169203331";
const SQL: &str = r#"SELECT note_id, title FROM "research/notes" ORDER BY note_id"#;

/// A sink keeping every span it records.
#[derive(Default)]
struct Kept(Mutex<Vec<Span>>);

impl SpanSink for Kept {
    fn record(&self, span: Span) {
        self.0.lock().unwrap().push(span);
    }
}

impl Kept {
    fn taken(&self) -> Vec<Span> {
        std::mem::take(&mut self.0.lock().unwrap())
    }
}

fn spans(kept: &Arc<Kept>) -> ReadSpans {
    ReadSpans { sink: kept.clone(), fresh: || TraceContext::root([9; 16], [3; 8]), now: || 42 }
}

fn clock() -> FixedClock {
    FixedClock(Instant::parse("2030-01-01T00:06:00Z").unwrap())
}

fn traceparent(parent: &str) -> String {
    format!("00-{TRACE}-{parent}-01")
}

fn query(meta: Option<&str>) -> Value {
    let mut params = json!({ "name": "context.query", "arguments": { "sql": SQL } });
    if let Some(parent) = meta {
        params["_meta"] = json!({ "traceparent": traceparent(parent), "tracestate": "vendor=meta" });
    }
    json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/call", "params": params })
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// With an exporter configured, each read tool call exports one `contextful.read` span carrying the tool, the tables,
/// the row count, the outcome, a refused read's error identifier, and the statement's SHA-256; the span carries no
/// statement text, subject member or row value.
// spec: disclosure.record.read-span@af58a898
// spec: disclosure.record.read-span-content@88bf0597
#[test]
fn a_read_records_one_span_with_its_digest_and_counts_and_no_statement() {
    let f = fixture();
    let clock = clock();
    let kept = Arc::new(Kept::default());
    let server = Server::new(&f.face, f.authority.clone(), &current, &clock, &f.audit).unwrap().with_spans(spans(&kept));

    let answer = server.handle(&query(Some(META_PARENT)).to_string()).unwrap();
    assert_eq!(answer["result"]["structuredContent"]["rows"].as_array().map(Vec::len), Some(2), "{answer}");
    let recorded = kept.taken();
    assert!(!recorded.is_empty(), "the read records a span");
    assert_eq!(recorded.len(), 1, "{recorded:?}");
    let span = &recorded[0];
    assert_eq!((span.name.as_str(), hex(&span.trace_id), span.parent.map(|p| hex(&p))), ("contextful.read", TRACE.to_string(), Some(META_PARENT.to_string())));
    assert_eq!(span.attribute("contextful.tool"), Some(&Attr::from("context.query")));
    assert_eq!(span.attribute("contextful.tables"), Some(&Attr::List(vec!["research/notes".into()])));
    assert_eq!(span.attribute("contextful.result.rows"), Some(&Attr::Int(2)));
    assert_eq!(span.attribute("contextful.read.outcome"), Some(&Attr::from("served")));
    assert_eq!(span.attribute("contextful.query.hash"), Some(&Attr::from(sha256_hex(SQL.as_bytes()))));
    assert_eq!((span.attribute("error.type"), &span.error), (None, &None));
    let exported = otlp_traces("svc", &recorded).to_string();
    for secret in ["SELECT", "note_id", "Solar battery storage", "Hiring plan", "dana@acme.example", "research-loop", "on-prem:hq"] {
        assert!(!exported.contains(secret), "the span discloses `{secret}`: {exported}");
    }

    // A refused read records its error identifier and returns no rows.
    let refused = json!({ "jsonrpc": "2.0", "id": 2, "method": "tools/call", "params": { "name": "context.query", "arguments": { "sql": r#"SELECT * FROM "hr/salaries""# } } });
    let answer = server.handle(&refused.to_string()).unwrap();
    let identifier = answer["result"]["structuredContent"]["error"]["identifier"].as_str().map(str::to_string).expect("the read is refused");
    let recorded = kept.taken();
    assert!(!recorded.is_empty(), "the refused read records a span");
    assert_eq!(recorded.len(), 1, "{recorded:?}");
    let span = &recorded[0];
    assert_eq!(span.attribute("contextful.read.outcome"), Some(&Attr::from("refused")));
    assert_eq!(span.attribute("error.type"), Some(&Attr::from(identifier.as_str())));
    assert_eq!(span.attribute("contextful.result.rows"), Some(&Attr::Int(0)));
    assert_eq!(span.attribute("contextful.tables"), Some(&Attr::List(vec!["hr/salaries".into()])));

    // A read no caller traced roots a fresh trace; `tools/list` records no span.
    server.handle(&query(None).to_string()).unwrap();
    server.handle(&json!({ "jsonrpc": "2.0", "id": 3, "method": "tools/list" }).to_string()).unwrap();
    let recorded = kept.taken();
    assert!(!recorded.is_empty(), "the untraced read records a span");
    assert_eq!(recorded.len(), 1, "{recorded:?}");
    assert_eq!((recorded[0].trace_id, recorded[0].span_id, recorded[0].parent), ([9; 16], [3; 8], None));
}

/// A read tool call takes its trace context from the HTTP request's `traceparent` and `tracestate` headers, or from
/// `params._meta`; a `_meta` `traceparent` wins over the header.
// spec: disclosure.record.face-carrier@9ca5cd00
#[test]
fn a_read_takes_its_parent_from_the_header_or_from_meta_which_wins() {
    let f = fixture();
    let clock = clock();
    let kept = Arc::new(Kept::default());
    let tools = Tools::new(&f.face, &clock, &f.audit).unwrap().with_spans(spans(&kept));
    let header = traceparent(HEADER_PARENT);
    let caller = Caller { authority: &f.authority, boundary: &current, carrier: Carrier { traceparent: Some(&header), tracestate: Some("vendor=header") } };

    for (meta, parent) in [(None, HEADER_PARENT), (Some(META_PARENT), META_PARENT)] {
        let answer = tools.handle(caller, &query(meta)).unwrap();
        assert!(answer["result"].get("isError").is_none(), "{answer}");
        let recorded = kept.taken();
        assert!(!recorded.is_empty(), "the read records a span");
        assert_eq!(recorded.len(), 1, "{recorded:?}");
        assert_eq!((hex(&recorded[0].trace_id), recorded[0].parent.map(|p| hex(&p))), (TRACE.to_string(), Some(parent.to_string())));
        // The span is a fresh child, never the caller's own span.
        assert_eq!(recorded[0].span_id, [3; 8]);
    }

    // A malformed header is ignored and the read roots a fresh trace.
    let caller = Caller { carrier: Carrier { traceparent: Some("00-not-a-trace-01"), tracestate: None }, ..caller };
    tools.handle(caller, &query(None)).unwrap();
    let recorded = kept.taken();
    assert!(!recorded.is_empty(), "the read records a span");
    assert_eq!((recorded[0].trace_id, recorded[0].parent), ([9; 16], None));
}
