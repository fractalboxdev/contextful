//! The span exporter: OTLP/HTTP JSON or protobuf to the collector the operator names in
//! `OTEL_EXPORTER_OTLP_ENDPOINT` (`disclosure.record.span-export`,
//! `disclosure.record.span-encoding`). An unset endpoint
//! builds no exporter, so a default configuration opens no socket for telemetry. Spans
//! leave off the operation's path through a bounded queue; a span past the cap is dropped
//! and counted (`disclosure.record.span-queue`).

use crate::client::{Client, HeaderValue};
use contextful_core::connector::attach::Allowlist;
use contextful_core::connector::reference::Hydrated;
use contextful_core::trace::{otlp_traces, Attr, Span, SpanKind, SpanSink, TraceContext};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{sync_channel, Receiver, SyncSender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use url::Url;

/// Spans the queue holds between the operation and the collector.
pub const SPAN_QUEUE: usize = 2048;
/// Spans one export request carries.
const BATCH: usize = 512;
/// Wall clock one export request may take, the OTLP exporter default.
const EXPORT_TIMEOUT: Duration = Duration::from_secs(10);
/// The service name a process exports under when `OTEL_SERVICE_NAME` is unset.
pub const DEFAULT_SERVICE: &str = "contextful";

/// Draw `N` random bytes for a trace or span id, never all zero.
fn random<const N: usize>() -> [u8; N] {
    let mut out = [0u8; N];
    while out == [0u8; N] {
        if getrandom::fill(&mut out).is_err() {
            // An exhausted source still yields a unique id within the process.
            let nanos = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(1);
            let seq = u128::from(SEQ.fetch_add(1, Ordering::Relaxed));
            for (i, byte) in out.iter_mut().enumerate() {
                *byte = ((nanos ^ (seq << 64)) >> ((i % 16) * 8)) as u8 | 1;
            }
        }
    }
    out
}

static SEQ: AtomicU64 = AtomicU64::new(1);

/// A fresh span id.
pub fn span_id() -> [u8; 8] {
    random()
}

/// A fresh, sampled trace for an operation no caller traced.
pub fn fresh_trace() -> TraceContext {
    TraceContext::root(random(), random())
}

/// The current wall clock in Unix nanoseconds, as a span interval records it.
pub fn now_unix_nanos() -> i128 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_nanos() as i128).unwrap_or(0)
}

/// The exporter to the operator's collector.
pub struct SpanExporter {
    queue: Mutex<Option<SyncSender<Span>>>,
    dropped: Arc<AtomicU64>,
    exported: Arc<AtomicU64>,
    done: Mutex<Option<Receiver<()>>>,
}

impl SpanExporter {
    /// The exporter `OTEL_EXPORTER_OTLP_TRACES_ENDPOINT` (used as given) or
    /// `OTEL_EXPORTER_OTLP_ENDPOINT` (with `/v1/traces` appended) names, sending the
    /// `OTEL_EXPORTER_OTLP_HEADERS` headers under `OTEL_SERVICE_NAME` in `wire`; `None` with
    /// neither endpoint set.
    pub fn from_env(var: impl Fn(&str) -> Option<String>, wire: Wire) -> Result<Option<SpanExporter>, String> {
        let set = |k: &str| var(k).map(|v| v.trim().to_string()).filter(|v| !v.is_empty());
        let endpoint = match (set("OTEL_EXPORTER_OTLP_TRACES_ENDPOINT"), set("OTEL_EXPORTER_OTLP_ENDPOINT")) {
            (Some(traces), _) => traces,
            (None, Some(base)) => format!("{}/v1/traces", base.trim_end_matches('/')),
            (None, None) => return Ok(None),
        };
        let headers = parse_headers(&set("OTEL_EXPORTER_OTLP_HEADERS").unwrap_or_default())?;
        let service = set("OTEL_SERVICE_NAME").unwrap_or_else(|| DEFAULT_SERVICE.to_string());
        SpanExporter::new(&endpoint, headers, &service, wire).map(Some)
    }

    /// An exporter posting batches to the collector at `endpoint` in `wire`.
    pub fn new(endpoint: &str, headers: Vec<(String, String)>, service: &str, wire: Wire) -> Result<SpanExporter, String> {
        let url = Url::parse(endpoint).map_err(|e| format!("the OTLP endpoint `{endpoint}` is no URL: {e}"))?;
        if !matches!(url.scheme(), "http" | "https") {
            return Err(format!("the OTLP endpoint speaks http or https, not `{}`", url.scheme()));
        }
        // Each header may carry a credential, so a cleartext collector off loopback refuses here
        // rather than drop every batch at send.
        for (name, _) in &headers {
            contextful_core::connector::attach::check_transport(&url, name).map_err(|e| e.to_string())?;
        }
        let allow = Allowlist::parse(&[url.host_str().unwrap_or_default().to_string()]).map_err(|e| e.to_string())?;
        // The collector is the operator's own, so an internal address is admitted for it alone.
        let client = Client::new(allow, url.clone()).with_timeout(EXPORT_TIMEOUT).with_body_limit(64 * 1024).framed("Content-Type", wire.content_type()).admitting_internal();
        let headers: Vec<(String, HeaderValue)> = headers.into_iter().map(|(k, v)| (k, HeaderValue::Sensitive(Arc::new(Hydrated::new(v))))).collect();
        let (tx, rx) = sync_channel::<Span>(SPAN_QUEUE);
        let (done_tx, done_rx) = sync_channel::<()>(1);
        let dropped = Arc::new(AtomicU64::new(0));
        let exported = Arc::new(AtomicU64::new(0));
        let (lost, sent, service) = (dropped.clone(), exported.clone(), service.to_string());
        std::thread::Builder::new()
            .name("contextful-otlp".into())
            .spawn(move || {
                while let Ok(first) = rx.recv() {
                    let mut batch = vec![first];
                    while batch.len() < BATCH {
                        match rx.try_recv() {
                            Ok(span) => batch.push(span),
                            Err(_) => break,
                        }
                    }
                    let body = wire.body(&service, &batch);
                    let n = batch.len() as u64;
                    match client.send_once("POST", &url, &headers, Some(&body)) {
                        Ok(r) if (200..300).contains(&r.status) => sent.fetch_add(n, Ordering::Relaxed),
                        _ => lost.fetch_add(n, Ordering::Relaxed),
                    };
                }
                let _ = done_tx.send(());
            })
            .map_err(|e| format!("starting the span exporter: {e}"))?;
        Ok(SpanExporter { queue: Mutex::new(Some(tx)), dropped, exported, done: Mutex::new(Some(done_rx)) })
    }

    /// Spans dropped at the full queue or lost to a failed export.
    pub fn dropped(&self) -> u64 {
        self.dropped.load(Ordering::Relaxed)
    }

    /// Spans a collector accepted.
    pub fn exported(&self) -> u64 {
        self.exported.load(Ordering::Relaxed)
    }

    /// Close the queue and wait up to `wait` for the queued spans to leave; spans still
    /// queued past it are dropped and counted.
    pub fn shutdown(&self, wait: Duration) {
        let pending = self.queue.lock().ok().and_then(|mut q| q.take());
        drop(pending);
        // Past `wait` the worker still holds spans, which leave with the process unexported.
        if let Some(done) = self.done.lock().ok().and_then(|mut d| d.take()) {
            let _ = done.recv_timeout(wait);
        }
    }
}

impl SpanSink for SpanExporter {
    fn record(&self, span: Span) {
        // A full or closed queue drops the span rather than wait on the collector.
        let queued = self.queue.lock().is_ok_and(|queue| queue.as_ref().is_some_and(|q| q.try_send(span).is_ok()));
        if !queued {
            self.dropped.fetch_add(1, Ordering::Relaxed);
        }
    }
}

/// The OTLP/HTTP encoding an exporter sends each batch in (`disclosure.record.span-encoding`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Wire {
    /// The OTLP JSON mapping, under `application/json`.
    Json,
    /// Binary protobuf `ExportTraceServiceRequest`, under `application/x-protobuf`.
    Protobuf,
}

impl Wire {
    fn content_type(self) -> &'static str {
        match self {
            Wire::Json => "application/json",
            Wire::Protobuf => "application/x-protobuf",
        }
    }

    fn body(self, service: &str, spans: &[Span]) -> Vec<u8> {
        match self {
            Wire::Json => otlp_traces(service, spans).to_string().into_bytes(),
            Wire::Protobuf => otlp_protobuf(service, spans),
        }
    }
}

/// Protobuf wire types.
const VARINT: u8 = 0;
const FIXED64: u8 = 1;
const LEN: u8 = 2;

fn varint(out: &mut Vec<u8>, mut v: u64) {
    while v >= 0x80 {
        out.push((v as u8) | 0x80);
        v >>= 7;
    }
    out.push(v as u8);
}

fn tag(out: &mut Vec<u8>, field: u32, wire: u8) {
    varint(out, (u64::from(field) << 3) | u64::from(wire));
}

fn bytes(out: &mut Vec<u8>, field: u32, data: &[u8]) {
    tag(out, field, LEN);
    varint(out, data.len() as u64);
    out.extend_from_slice(data);
}

fn message(out: &mut Vec<u8>, field: u32, fill: impl FnOnce(&mut Vec<u8>)) {
    let mut inner = Vec::new();
    fill(&mut inner);
    bytes(out, field, &inner);
}

fn fixed64(out: &mut Vec<u8>, field: u32, v: u64) {
    tag(out, field, FIXED64);
    out.extend_from_slice(&v.to_le_bytes());
}

/// An `AnyValue`; each case is a `oneof` member, so a default value is still written.
fn any_value(out: &mut Vec<u8>, v: &Attr) {
    match v {
        Attr::Str(s) => bytes(out, 1, s.as_bytes()),
        Attr::Bool(b) => {
            tag(out, 2, VARINT);
            varint(out, u64::from(*b));
        }
        Attr::Int(n) => {
            tag(out, 3, VARINT);
            varint(out, *n as u64);
        }
        Attr::List(items) => message(out, 5, |array| {
            for item in items {
                message(array, 1, |value| bytes(value, 1, item.as_bytes()));
            }
        }),
    }
}

fn key_value(out: &mut Vec<u8>, field: u32, key: &str, value: &Attr) {
    message(out, field, |kv| {
        bytes(kv, 1, key.as_bytes());
        message(kv, 2, |v| any_value(v, value));
    });
}

fn span(out: &mut Vec<u8>, s: &Span) {
    bytes(out, 1, &s.trace_id);
    bytes(out, 2, &s.span_id);
    if let Some(parent) = s.parent {
        bytes(out, 4, &parent);
    }
    bytes(out, 5, s.name.as_bytes());
    tag(out, 6, VARINT);
    varint(out, match s.kind {
        SpanKind::Internal => 1,
        SpanKind::Server => 2,
        SpanKind::Client => 3,
    });
    // The JSON body's clamping: no instant before the epoch, no end before the start.
    let start = s.start_unix_nanos.max(0);
    let end = s.end_unix_nanos.max(start);
    fixed64(out, 7, u64::try_from(start).unwrap_or(u64::MAX));
    fixed64(out, 8, u64::try_from(end).unwrap_or(u64::MAX));
    for (k, v) in &s.attributes {
        key_value(out, 9, k, v);
    }
    for linked in &s.links {
        message(out, 13, |link| {
            bytes(link, 1, &linked.trace_id);
            bytes(link, 2, &linked.span_id);
            if let Some(state) = &linked.state {
                bytes(link, 3, state.as_bytes());
            }
        });
    }
    if let Some(reason) = &s.error {
        message(out, 15, |status| {
            bytes(status, 2, reason.as_bytes());
            tag(status, 3, VARINT);
            varint(status, 2);
        });
    }
}

/// The binary protobuf `ExportTraceServiceRequest` for `spans`, carrying what
/// [`otlp_traces`] carries under resource attribute `service.name` = `service`.
pub fn otlp_protobuf(service: &str, spans: &[Span]) -> Vec<u8> {
    let mut out = Vec::new();
    message(&mut out, 1, |resource_spans| {
        message(resource_spans, 1, |resource| key_value(resource, 1, "service.name", &Attr::Str(service.to_string())));
        message(resource_spans, 2, |scope_spans| {
            message(scope_spans, 1, |scope| bytes(scope, 1, b"contextful"));
            for s in spans {
                message(scope_spans, 2, |out| span(out, s));
            }
        });
    });
    out
}

/// The `k=v,k2=v2` list `OTEL_EXPORTER_OTLP_HEADERS` carries, values percent-decoded.
fn parse_headers(text: &str) -> Result<Vec<(String, String)>, String> {
    let mut out = Vec::new();
    for pair in text.split(',').map(str::trim).filter(|p| !p.is_empty()) {
        let (k, v) = pair.split_once('=').ok_or_else(|| "OTEL_EXPORTER_OTLP_HEADERS holds an entry without `=`".to_string())?;
        let v = url::form_urlencoded::parse(format!("v={}", v.trim().replace('+', "%2B")).as_bytes()).next().map(|(_, v)| v.into_owned()).unwrap_or_default();
        out.push((k.trim().to_string(), v));
    }
    Ok(out)
}
