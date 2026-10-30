//! The third-party OTLP protobuf decoder a stub collector reads a span exporter's body with.

use contextful_acceptance::otlp::{traces_json, AnyValue, ArrayValue, Case, ExportTraceServiceRequest, InstrumentationScope, KeyValue, Link, Resource, ResourceSpans, ScopeSpans, Span, Status};
use prost::Message;
use serde_json::json;

fn kv(key: &str, value: Case) -> KeyValue {
    KeyValue { key: key.into(), value: Some(AnyValue { value: Some(value) }) }
}

/// A protobuf `ExportTraceServiceRequest` reads as its OTLP JSON shape: hex ids, decimal instants and integers, an
/// absent parent and an unset status omitted.
#[test]
fn a_protobuf_trace_request_reads_as_its_otlp_json_shape() {
    let child = Span {
        trace_id: vec![0xab; 16],
        span_id: vec![0x01; 8],
        parent_span_id: vec![0x02; 8],
        name: "contextful.run".into(),
        kind: 1,
        start_time_unix_nano: 1_000,
        end_time_unix_nano: 2_000,
        attributes: vec![
            kv("contextful.rows", Case::IntValue(-3)),
            kv("contextful.status", Case::StringValue("failed".into())),
            kv("contextful.truncated", Case::BoolValue(false)),
            kv("contextful.tables", Case::ArrayValue(ArrayValue { values: vec![AnyValue { value: Some(Case::StringValue("a".into())) }] })),
        ],
        links: vec![Link { trace_id: vec![0xcd; 16], span_id: vec![0x03; 8], trace_state: "v=1".into() }],
        status: Some(Status { message: "SourceUnreachable".into(), code: 2 }),
        ..Span::default()
    };
    let root = Span { trace_id: vec![0xab; 16], span_id: vec![0x02; 8], name: "root".into(), kind: 2, ..Span::default() };
    let request = ExportTraceServiceRequest {
        resource_spans: vec![ResourceSpans {
            resource: Some(Resource { attributes: vec![kv("service.name", Case::StringValue("svc".into()))] }),
            scope_spans: vec![ScopeSpans { scope: Some(InstrumentationScope { name: "contextful".into() }), spans: vec![child, root] }],
        }],
    };
    let body = traces_json(&request.encode_to_vec()).unwrap();
    let resource = &body["resourceSpans"][0];
    assert_eq!(resource["resource"]["attributes"][0], json!({ "key": "service.name", "value": { "stringValue": "svc" } }));
    assert_eq!(resource["scopeSpans"][0]["scope"]["name"], json!("contextful"));
    let spans = resource["scopeSpans"][0]["spans"].as_array().unwrap();
    assert_eq!(spans.len(), 2);
    assert_eq!(
        spans[0],
        json!({
            "traceId": "ab".repeat(16), "spanId": "01".repeat(8), "parentSpanId": "02".repeat(8), "name": "contextful.run", "kind": 1,
            "startTimeUnixNano": "1000", "endTimeUnixNano": "2000",
            "attributes": [
                { "key": "contextful.rows", "value": { "intValue": "-3" } },
                { "key": "contextful.status", "value": { "stringValue": "failed" } },
                { "key": "contextful.truncated", "value": { "boolValue": false } },
                { "key": "contextful.tables", "value": { "arrayValue": { "values": [{ "stringValue": "a" }] } } },
            ],
            "status": { "code": 2, "message": "SourceUnreachable" },
            "links": [{ "traceId": "cd".repeat(16), "spanId": "03".repeat(8), "traceState": "v=1" }],
        })
    );
    assert!(spans[1].get("parentSpanId").is_none() && spans[1].get("links").is_none());
    assert_eq!(spans[1]["status"], json!({ "code": 0 }));

    // A body that is no protobuf message refuses to decode.
    assert!(traces_json(&[0x0a, 0xff]).is_err());
}
