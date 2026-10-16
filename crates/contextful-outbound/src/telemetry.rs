//! The span exporter: OTLP/HTTP JSON to the collector the operator names in
//! `OTEL_EXPORTER_OTLP_ENDPOINT` (`disclosure.record.span-export`). An unset endpoint
//! builds no exporter, so a default configuration opens no socket for telemetry. Spans
//! leave off the operation's path through a bounded queue; a span past the cap is dropped
//! and counted (`disclosure.record.span-queue`).

use crate::client::{Client, HeaderValue};
use contextful_core::connector::attach::Allowlist;
use contextful_core::connector::reference::Hydrated;
use contextful_core::trace::{otlp_traces, Span, SpanSink, TraceContext};
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
    /// `OTEL_EXPORTER_OTLP_HEADERS` headers under `OTEL_SERVICE_NAME`; `None` with
    /// neither endpoint set.
    pub fn from_env(var: impl Fn(&str) -> Option<String>) -> Result<Option<SpanExporter>, String> {
        let set = |k: &str| var(k).map(|v| v.trim().to_string()).filter(|v| !v.is_empty());
        let endpoint = match (set("OTEL_EXPORTER_OTLP_TRACES_ENDPOINT"), set("OTEL_EXPORTER_OTLP_ENDPOINT")) {
            (Some(traces), _) => traces,
            (None, Some(base)) => format!("{}/v1/traces", base.trim_end_matches('/')),
            (None, None) => return Ok(None),
        };
        let headers = parse_headers(&set("OTEL_EXPORTER_OTLP_HEADERS").unwrap_or_default())?;
        let service = set("OTEL_SERVICE_NAME").unwrap_or_else(|| DEFAULT_SERVICE.to_string());
        SpanExporter::new(&endpoint, headers, &service).map(Some)
    }

    /// An exporter posting batches to the collector at `endpoint`.
    pub fn new(endpoint: &str, headers: Vec<(String, String)>, service: &str) -> Result<SpanExporter, String> {
        let url = Url::parse(endpoint).map_err(|e| format!("the OTLP endpoint `{endpoint}` is no URL: {e}"))?;
        if !matches!(url.scheme(), "http" | "https") {
            return Err(format!("the OTLP endpoint speaks http or https, not `{}`", url.scheme()));
        }
        let allow = Allowlist::parse(&[url.host_str().unwrap_or_default().to_string()]).map_err(|e| e.to_string())?;
        // The collector is the operator's own, so an internal address is admitted for it alone.
        let client = Client::new(allow, url.clone()).with_timeout(EXPORT_TIMEOUT).with_body_limit(64 * 1024).framed("Content-Type", "application/json").admitting_internal();
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
                    let body = otlp_traces(&service, &batch).to_string();
                    let n = batch.len() as u64;
                    match client.send_once("POST", &url, &headers, Some(body.as_bytes())) {
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
