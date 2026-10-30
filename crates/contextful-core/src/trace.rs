//! W3C trace context and the spans an operation leaves: the `traceparent` carrier a caller
//! hands in (`disclosure.record.trace-carrier`), the span a run or a model call records,
//! and the OTLP/HTTP JSON body a batch of spans becomes (`disclosure.record.span-export`).
//! Identifiers arrive from the caller; this module draws no randomness and opens no socket.

use serde_json::{json, Value};

/// The W3C carrier naming the caller's span.
pub const TRACEPARENT: &str = "traceparent";
/// The W3C carrier holding vendor trace state beside it.
pub const TRACESTATE: &str = "tracestate";
/// Longest `tracestate` carried on; a longer value is dropped whole.
const MAX_TRACESTATE: usize = 512;

/// One position in a trace: the trace, the span acting as parent of the next, its
/// sampling flags and any vendor state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TraceContext {
    pub trace_id: [u8; 16],
    pub span_id: [u8; 8],
    pub flags: u8,
    pub state: Option<String>,
}

impl TraceContext {
    /// The caller's context from its `traceparent` and `tracestate` carriers, or `None`
    /// for a malformed `traceparent`, which the operation ignores and starts a fresh trace.
    pub fn parse(traceparent: &str, tracestate: Option<&str>) -> Option<TraceContext> {
        let s = traceparent.trim();
        let version = s.get(0..2)?;
        if !lower_hex(version) || version == "ff" {
            return None;
        }
        // Version 00 is exactly 55 characters; a later version may append fields after a dash.
        let fits = if version == "00" { s.len() == 55 } else { s.len() == 55 || s.as_bytes().get(55) == Some(&b'-') };
        if !fits || s.as_bytes().get(2) != Some(&b'-') || s.as_bytes().get(35) != Some(&b'-') || s.as_bytes().get(52) != Some(&b'-') {
            return None;
        }
        let trace_id: [u8; 16] = decode(s.get(3..35)?)?;
        let span_id: [u8; 8] = decode(s.get(36..52)?)?;
        let [flags]: [u8; 1] = decode(s.get(53..55)?)?;
        if trace_id == [0; 16] || span_id == [0; 8] {
            return None;
        }
        let state = tracestate.map(str::trim).filter(|t| !t.is_empty() && t.len() <= MAX_TRACESTATE).map(str::to_string);
        Some(TraceContext { trace_id, span_id, flags, state })
    }

    /// A fresh, sampled trace rooted at `span_id`, for an operation no caller traced.
    pub fn root(trace_id: [u8; 16], span_id: [u8; 8]) -> TraceContext {
        TraceContext { trace_id, span_id, flags: 1, state: None }
    }

    /// The context of a child span `span_id` in the same trace.
    pub fn child(&self, span_id: [u8; 8]) -> TraceContext {
        TraceContext { span_id, ..self.clone() }
    }

    /// The `traceparent` value naming this context's span.
    pub fn traceparent(&self) -> String {
        format!("00-{}-{}-{:02x}", hex(&self.trace_id), hex(&self.span_id), self.flags)
    }

    /// The trace id as 32 lowercase hex digits, as a run row carries it.
    pub fn trace_hex(&self) -> String {
        hex(&self.trace_id)
    }

    /// The span id as 16 lowercase hex digits.
    pub fn span_hex(&self) -> String {
        hex(&self.span_id)
    }
}

/// A trace context at rest: the W3C `traceparent` and `tracestate` values alone, as a queued
/// delivery's message carries its route span (`disclosure.record.queued-content`).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Carrier {
    pub traceparent: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tracestate: Option<String>,
}

impl Carrier {
    /// The carriers naming `context`'s span.
    pub fn of(context: &TraceContext) -> Carrier {
        Carrier { traceparent: context.traceparent(), tracestate: context.state.clone() }
    }

    /// The context the carriers name, or `None` for a malformed `traceparent`
    /// (`disclosure.record.trace-carrier`).
    pub fn context(&self) -> Option<TraceContext> {
        TraceContext::parse(&self.traceparent, self.tracestate.as_deref())
    }
}

/// Lowercase hex of `bytes`.
pub fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn lower_hex(s: &str) -> bool {
    !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

fn decode<const N: usize>(s: &str) -> Option<[u8; N]> {
    if s.len() != N * 2 || !lower_hex(s) {
        return None;
    }
    let mut out = [0u8; N];
    for (i, byte) in out.iter_mut().enumerate() {
        *byte = u8::from_str_radix(s.get(i * 2..i * 2 + 2)?, 16).ok()?;
    }
    Some(out)
}

/// The OTLP span kind.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SpanKind {
    Internal,
    Server,
    Client,
}

impl SpanKind {
    fn code(self) -> u8 {
        match self {
            SpanKind::Internal => 1,
            SpanKind::Server => 2,
            SpanKind::Client => 3,
        }
    }
}

/// One attribute value a span carries.
#[derive(Debug, Clone, PartialEq)]
pub enum Attr {
    Str(String),
    Int(i64),
    Bool(bool),
    /// A list of strings, exported as one OTLP array value.
    List(Vec<String>),
}

impl From<&str> for Attr {
    fn from(s: &str) -> Attr {
        Attr::Str(s.to_string())
    }
}

impl From<String> for Attr {
    fn from(s: String) -> Attr {
        Attr::Str(s)
    }
}

impl From<u64> for Attr {
    fn from(n: u64) -> Attr {
        Attr::Int(i64::try_from(n).unwrap_or(i64::MAX))
    }
}

impl From<Vec<String>> for Attr {
    fn from(items: Vec<String>) -> Attr {
        Attr::List(items)
    }
}

impl From<bool> for Attr {
    fn from(b: bool) -> Attr {
        Attr::Bool(b)
    }
}

/// One finished span: identifiers, name, kind, interval, attributes, the spans it links
/// and, for a failure, its reason.
#[derive(Debug, Clone, PartialEq)]
pub struct Span {
    pub trace_id: [u8; 16],
    pub span_id: [u8; 8],
    pub parent: Option<[u8; 8]>,
    pub name: String,
    pub kind: SpanKind,
    pub start_unix_nanos: i128,
    pub end_unix_nanos: i128,
    pub attributes: Vec<(String, Attr)>,
    /// Spans in other traces this span relates to without parenting under them.
    pub links: Vec<TraceContext>,
    pub error: Option<String>,
}

impl Span {
    /// A span named `name` at `context`'s span, child of `parent` when the caller traced one.
    pub fn at(context: &TraceContext, parent: Option<[u8; 8]>, name: &str, kind: SpanKind) -> Span {
        Span {
            trace_id: context.trace_id,
            span_id: context.span_id,
            parent,
            name: name.to_string(),
            kind,
            start_unix_nanos: 0,
            end_unix_nanos: 0,
            attributes: Vec::new(),
            links: Vec::new(),
            error: None,
        }
    }

    /// The span with a link to `linked`'s span.
    pub fn linked(mut self, linked: &TraceContext) -> Span {
        self.links.push(linked.clone());
        self
    }

    /// The span with attribute `key` set to `value`.
    pub fn with(mut self, key: &str, value: impl Into<Attr>) -> Span {
        self.attributes.push((key.to_string(), value.into()));
        self
    }

    /// The attribute `key`, where the span carries it.
    pub fn attribute(&self, key: &str) -> Option<&Attr> {
        self.attributes.iter().find(|(k, _)| k == key).map(|(_, v)| v)
    }
}

/// Where finished spans go. Recording never blocks the operation that made the span.
pub trait SpanSink: Send + Sync {
    fn record(&self, span: Span);
}

fn any_value(v: &Attr) -> Value {
    match v {
        Attr::Str(s) => json!({ "stringValue": s }),
        Attr::Int(n) => json!({ "intValue": n.to_string() }),
        Attr::Bool(b) => json!({ "boolValue": b }),
        Attr::List(items) => json!({ "arrayValue": { "values": items.iter().map(|s| json!({ "stringValue": s })).collect::<Vec<_>>() } }),
    }
}

fn link(linked: &TraceContext) -> Value {
    let mut link = json!({ "traceId": linked.trace_hex(), "spanId": linked.span_hex() });
    if let Some(state) = &linked.state {
        link["traceState"] = json!(state);
    }
    link
}

/// The OTLP/HTTP JSON `ExportTraceServiceRequest` body for `spans`, under resource
/// attribute `service.name` = `service`.
pub fn otlp_traces(service: &str, spans: &[Span]) -> Value {
    let spans: Vec<Value> = spans
        .iter()
        .map(|s| {
            let mut span = json!({
                "traceId": hex(&s.trace_id),
                "spanId": hex(&s.span_id),
                "name": s.name,
                "kind": s.kind.code(),
                "startTimeUnixNano": s.start_unix_nanos.max(0).to_string(),
                "endTimeUnixNano": s.end_unix_nanos.max(s.start_unix_nanos).max(0).to_string(),
                "attributes": s.attributes.iter().map(|(k, v)| json!({ "key": k, "value": any_value(v) })).collect::<Vec<_>>(),
                "status": match &s.error { Some(m) => json!({ "code": 2, "message": m }), None => json!({ "code": 0 }) },
            });
            if let Some(parent) = s.parent {
                span["parentSpanId"] = json!(hex(&parent));
            }
            if !s.links.is_empty() {
                span["links"] = s.links.iter().map(link).collect();
            }
            span
        })
        .collect();
    json!({
        "resourceSpans": [{
            "resource": { "attributes": [{ "key": "service.name", "value": { "stringValue": service } }] },
            "scopeSpans": [{ "scope": { "name": "contextful" }, "spans": spans }],
        }]
    })
}
