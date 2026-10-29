//! The process's trace context and span exporter: the caller's `TRACEPARENT` and
//! `TRACESTATE` carriers (`disclosure.record.trace-carrier`), and the exporter the
//! operator's `OTEL_EXPORTER_OTLP_*` variables configure (`disclosure.record.span-export`).
//! With no endpoint set no exporter exists and no span leaves the process
//! (`disclosure.record.span-unconfigured`).

use contextful_core::run::record::RunRow;
use contextful_core::trace::{Span, SpanKind, SpanSink, TraceContext};
use contextful_outbound::telemetry::{fresh_trace, span_id, SpanExporter};
use std::sync::{Arc, OnceLock};
use std::time::Duration;

/// Longest wait at exit for queued spans to reach the collector.
const DRAIN: Duration = Duration::from_secs(5);

struct Telemetry {
    caller: Option<TraceContext>,
    /// The trace model calls join: the caller's, or one fresh trace for the whole process.
    root: TraceContext,
    exporter: Option<Arc<SpanExporter>>,
}

static TELEMETRY: OnceLock<Telemetry> = OnceLock::new();

fn telemetry() -> &'static Telemetry {
    TELEMETRY.get_or_init(|| {
        let var = |k: &str| std::env::var(k).ok();
        let caller = var("TRACEPARENT").and_then(|p| TraceContext::parse(&p, var("TRACESTATE").as_deref()));
        let exporter = match SpanExporter::from_env(var) {
            Ok(e) => e.map(Arc::new),
            Err(e) => {
                eprintln!("telemetry: no spans export: {e}");
                None
            }
        };
        let root = caller.clone().unwrap_or_else(fresh_trace);
        Telemetry { caller, root, exporter }
    })
}

/// Where spans go, when an exporter is configured.
pub fn sink() -> Option<Arc<dyn SpanSink>> {
    telemetry().exporter.clone().map(|e| e as Arc<dyn SpanSink>)
}

/// Where the read face records each read tool call's span, when an exporter is configured;
/// with none, the face records no span and parses no carrier (`disclosure.record.read-span`).
pub fn read_spans() -> Option<contextful_agent::mcp::ReadSpans> {
    sink().map(|sink| contextful_agent::mcp::ReadSpans { sink, fresh: fresh_trace, now: contextful_outbound::telemetry::now_unix_nanos })
}

/// The trace a model call joins and the span it parents under: the caller's, or the
/// process's one fresh trace as a root when only an exporter is configured; `None` when
/// neither is (`connector.infer.model-span`).
pub fn model_parent() -> Option<(TraceContext, Option<[u8; 8]>, Arc<dyn SpanSink>)> {
    let t = telemetry();
    let parent = t.caller.as_ref().map(|c| c.span_id);
    match (&t.caller, sink()) {
        (_, Some(sink)) => Some((t.root.clone(), parent, sink)),
        (Some(_), None) => Some((t.root.clone(), parent, Arc::new(Discard))),
        (None, None) => None,
    }
}

/// `endpoint` recording each call's span under [`model_parent`].
pub fn traced(endpoint: contextful_outbound::infer::Endpoint) -> contextful_outbound::infer::Endpoint {
    match model_parent() {
        Some((context, parent, sink)) => endpoint.traced(context, parent, sink),
        None => endpoint,
    }
}

/// A sink keeping nothing, for a call that carries the caller's trace on with no exporter.
struct Discard;

impl SpanSink for Discard {
    fn record(&self, _: Span) {}
}

/// One run's span position: a fresh span in the caller's trace, or in a fresh trace when no
/// caller traced the process (`run.record.trace-context`).
pub struct RunTrace {
    context: TraceContext,
    parent: Option<[u8; 8]>,
}

impl RunTrace {
    pub fn open() -> RunTrace {
        match &telemetry().caller {
            Some(caller) => RunTrace { context: caller.child(span_id()), parent: Some(caller.span_id) },
            None => RunTrace { context: fresh_trace(), parent: None },
        }
    }

    /// The trace id both run rows carry.
    pub fn trace_id(&self) -> String {
        self.context.trace_hex()
    }

    /// Export the run's span over `row` (`run.record.run-span`).
    pub fn close(&self, row: &RunRow) {
        let Some(sink) = sink() else { return };
        let mut span = Span::at(&self.context, self.parent, "contextful.run", SpanKind::Internal)
            .with("contextful.run.id", row.run_id.as_str())
            .with("contextful.pipeline", row.pipeline_id.as_str())
            .with("contextful.table", row.table.as_str())
            .with("contextful.status", row.status.to_string())
            .with("contextful.rows", row.rows)
            .with("contextful.bytes", row.bytes);
        if let Some(kind) = row.error_kind {
            span = span.with("error.type", kind.name());
            span.error = Some(kind.name().to_string());
        }
        span.start_unix_nanos = row.started_at.unix_nanos();
        span.end_unix_nanos = row.ended_at.map_or_else(contextful_outbound::telemetry::now_unix_nanos, |e| e.unix_nanos());
        sink.record(span);
    }
}

/// Drain the exporter before the process exits, reporting spans it dropped.
pub fn finish() {
    let Some(exporter) = TELEMETRY.get().and_then(|t| t.exporter.as_ref()) else { return };
    exporter.shutdown(DRAIN);
    if exporter.dropped() > 0 {
        eprintln!("telemetry: {} spans dropped", exporter.dropped());
    }
}
