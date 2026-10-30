//! A third-party decoder for the OTLP/HTTP protobuf trace body: the
//! `ExportTraceServiceRequest` subset a span exporter sends, decoded by `prost` with no
//! engine code, and mapped onto the OTLP JSON shape so a stub collector reads either
//! encoding through one set of assertions.

use prost::Message;
use serde_json::{json, Map, Value};

/// `opentelemetry.proto.collector.trace.v1.ExportTraceServiceRequest`.
#[derive(Clone, PartialEq, Message)]
pub struct ExportTraceServiceRequest {
    #[prost(message, repeated, tag = "1")]
    pub resource_spans: Vec<ResourceSpans>,
}

#[derive(Clone, PartialEq, Message)]
pub struct ResourceSpans {
    #[prost(message, optional, tag = "1")]
    pub resource: Option<Resource>,
    #[prost(message, repeated, tag = "2")]
    pub scope_spans: Vec<ScopeSpans>,
}

#[derive(Clone, PartialEq, Message)]
pub struct Resource {
    #[prost(message, repeated, tag = "1")]
    pub attributes: Vec<KeyValue>,
}

#[derive(Clone, PartialEq, Message)]
pub struct ScopeSpans {
    #[prost(message, optional, tag = "1")]
    pub scope: Option<InstrumentationScope>,
    #[prost(message, repeated, tag = "2")]
    pub spans: Vec<Span>,
}

#[derive(Clone, PartialEq, Message)]
pub struct InstrumentationScope {
    #[prost(string, tag = "1")]
    pub name: String,
}

#[derive(Clone, PartialEq, Message)]
pub struct Span {
    #[prost(bytes = "vec", tag = "1")]
    pub trace_id: Vec<u8>,
    #[prost(bytes = "vec", tag = "2")]
    pub span_id: Vec<u8>,
    #[prost(string, tag = "3")]
    pub trace_state: String,
    #[prost(bytes = "vec", tag = "4")]
    pub parent_span_id: Vec<u8>,
    #[prost(string, tag = "5")]
    pub name: String,
    #[prost(int32, tag = "6")]
    pub kind: i32,
    #[prost(fixed64, tag = "7")]
    pub start_time_unix_nano: u64,
    #[prost(fixed64, tag = "8")]
    pub end_time_unix_nano: u64,
    #[prost(message, repeated, tag = "9")]
    pub attributes: Vec<KeyValue>,
    #[prost(message, repeated, tag = "13")]
    pub links: Vec<Link>,
    #[prost(message, optional, tag = "15")]
    pub status: Option<Status>,
}

#[derive(Clone, PartialEq, Message)]
pub struct Link {
    #[prost(bytes = "vec", tag = "1")]
    pub trace_id: Vec<u8>,
    #[prost(bytes = "vec", tag = "2")]
    pub span_id: Vec<u8>,
    #[prost(string, tag = "3")]
    pub trace_state: String,
}

#[derive(Clone, PartialEq, Message)]
pub struct Status {
    #[prost(string, tag = "2")]
    pub message: String,
    #[prost(int32, tag = "3")]
    pub code: i32,
}

#[derive(Clone, PartialEq, Message)]
pub struct KeyValue {
    #[prost(string, tag = "1")]
    pub key: String,
    #[prost(message, optional, tag = "2")]
    pub value: Option<AnyValue>,
}

#[derive(Clone, PartialEq, Message)]
pub struct AnyValue {
    #[prost(oneof = "Case", tags = "1, 2, 3, 5")]
    pub value: Option<Case>,
}

/// The `AnyValue.value` cases a span attribute takes.
#[derive(Clone, PartialEq, prost::Oneof)]
pub enum Case {
    #[prost(string, tag = "1")]
    StringValue(String),
    #[prost(bool, tag = "2")]
    BoolValue(bool),
    #[prost(int64, tag = "3")]
    IntValue(i64),
    #[prost(message, tag = "5")]
    ArrayValue(ArrayValue),
}

#[derive(Clone, PartialEq, Message)]
pub struct ArrayValue {
    #[prost(message, repeated, tag = "1")]
    pub values: Vec<AnyValue>,
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn any_value(v: &AnyValue) -> Value {
    match &v.value {
        Some(Case::StringValue(s)) => json!({ "stringValue": s }),
        Some(Case::BoolValue(b)) => json!({ "boolValue": b }),
        Some(Case::IntValue(n)) => json!({ "intValue": n.to_string() }),
        Some(Case::ArrayValue(a)) => json!({ "arrayValue": { "values": a.values.iter().map(any_value).collect::<Vec<_>>() } }),
        None => json!({}),
    }
}

fn attributes(kvs: &[KeyValue]) -> Value {
    kvs.iter().map(|kv| json!({ "key": kv.key, "value": kv.value.as_ref().map_or(json!({}), any_value) })).collect()
}

fn span(s: &Span) -> Value {
    let mut out = Map::new();
    out.insert("traceId".into(), json!(hex(&s.trace_id)));
    out.insert("spanId".into(), json!(hex(&s.span_id)));
    if !s.trace_state.is_empty() {
        out.insert("traceState".into(), json!(s.trace_state));
    }
    if !s.parent_span_id.is_empty() {
        out.insert("parentSpanId".into(), json!(hex(&s.parent_span_id)));
    }
    out.insert("name".into(), json!(s.name));
    out.insert("kind".into(), json!(s.kind));
    out.insert("startTimeUnixNano".into(), json!(s.start_time_unix_nano.to_string()));
    out.insert("endTimeUnixNano".into(), json!(s.end_time_unix_nano.to_string()));
    out.insert("attributes".into(), attributes(&s.attributes));
    let status = s.status.clone().unwrap_or_default();
    let status = if status.message.is_empty() { json!({ "code": status.code }) } else { json!({ "code": status.code, "message": status.message }) };
    out.insert("status".into(), status);
    if !s.links.is_empty() {
        let links = s.links.iter().map(|l| {
            let mut link = json!({ "traceId": hex(&l.trace_id), "spanId": hex(&l.span_id) });
            if !l.trace_state.is_empty() {
                link["traceState"] = json!(l.trace_state);
            }
            link
        });
        out.insert("links".into(), links.collect());
    }
    Value::Object(out)
}

/// The OTLP JSON shape of the protobuf `ExportTraceServiceRequest` in `body`: ids as lowercase
/// hex, nanosecond instants and 64-bit integers as decimal strings, and fields at their
/// default omitted as the JSON mapping omits them.
pub fn traces_json(body: &[u8]) -> Result<Value, prost::DecodeError> {
    let request = ExportTraceServiceRequest::decode(body)?;
    let resources = request.resource_spans.iter().map(|r| {
        json!({
            "resource": { "attributes": attributes(&r.resource.clone().unwrap_or_default().attributes) },
            "scopeSpans": r.scope_spans.iter().map(|s| json!({
                "scope": { "name": s.scope.clone().unwrap_or_default().name },
                "spans": s.spans.iter().map(span).collect::<Vec<_>>(),
            })).collect::<Vec<_>>(),
        })
    });
    Ok(json!({ "resourceSpans": resources.collect::<Vec<_>>() }))
}
