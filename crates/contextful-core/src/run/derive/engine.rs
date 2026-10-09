//! `run.bind`: the port every derive engine answers through — the variants an engine
//! error takes, what each costs by port, and the values an engine returns held to their
//! ranges before a row lands.

use super::config::Binding;
use crate::run::ports::Row;
use crate::run::RunError;
use serde_json::Value;

/// Vendor response text an `Upstream` error carries: 4 KiB (`run.bind.upstream-excerpt`).
pub const UPSTREAM_EXCERPT_BYTES: usize = 4 * 1024;

/// A vendor's non-success answer: its status and an excerpt of its response. It holds no
/// field for the request, so no request body reaches an error (`run.bind.upstream-excerpt`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Upstream {
    pub status: u16,
    excerpt: String,
}

impl Upstream {
    /// The answer `status` with the first 4 KiB of `response`, cut on a char boundary.
    pub fn new(status: u16, response: &[u8]) -> Upstream {
        let text = String::from_utf8_lossy(&response[..response.len().min(UPSTREAM_EXCERPT_BYTES)]);
        let mut end = text.len().min(UPSTREAM_EXCERPT_BYTES);
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        Upstream { status, excerpt: text[..end].trim().to_string() }
    }

    pub fn excerpt(&self) -> &str {
        &self.excerpt
    }
}

impl std::fmt::Display for Upstream {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "upstream answered {}: {}", self.status, self.excerpt)
    }
}

/// The port an engine serves.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Port {
    /// Reads media into cues: the `transcribe` task.
    Transcriber,
    /// Reads a link's head: the `link_preview` task.
    LinkReader,
}

/// Why an engine returned no answer for a unit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EngineError {
    /// The engine itself cannot answer: its process, socket or vendor is unreachable.
    Unavailable(String),
    /// The engine answered with a non-success status.
    Upstream(Upstream),
}

/// What an engine error costs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Settled {
    /// The run ends; no unit after this one is attempted.
    EndRun(String),
    /// The unit lands a `failed` marker carrying this error text and spends one attempt.
    Unit(String),
}

/// Settle an engine error by its port: an unavailable transcriber ends the run, an
/// unavailable link reader costs that one unit (`run.bind.engine-unavailable`).
pub fn settle(port: Port, error: EngineError) -> Settled {
    match (port, error) {
        (Port::Transcriber, EngineError::Unavailable(why)) => Settled::EndRun(format!("EngineUnavailable: {why}")),
        (Port::LinkReader, EngineError::Unavailable(why)) => Settled::Unit(RunError::DeriveLinkEngineUnavailable(why).to_string()),
        (_, EngineError::Upstream(answer)) => Settled::Unit(answer.to_string()),
    }
}

/// The column an engine's normalized confidence lands in.
pub const CONFIDENCE: &str = "confidence";

/// Hold a row's `confidence` to `0.0..=1.0`: a value outside it, or no number, lands null
/// and raises `DeriveConfidenceOutOfRange` (`run.bind.confidence-range`).
pub fn bound_confidence(row: &mut Row) -> Option<RunError> {
    let value = row.get(CONFIDENCE)?;
    if value.is_null() || value.as_f64().is_some_and(|c| (0.0..=1.0).contains(&c)) {
        return None;
    }
    let shown = value.to_string();
    row.insert(CONFIDENCE.into(), Value::Null);
    Some(RunError::DeriveConfidenceOutOfRange(format!("confidence {shown} lies outside 0.0..=1.0; the row lands null")))
}

/// The column a row's zone lands in.
pub const ZONE: &str = "zone";

/// Refuse rows whose zone is the binding's advisory `zone` value: the adapter writes a
/// row's zone, never the operator's key (`run.bind.advisory-zone`).
pub fn check_advisory_zone(engine: &str, binding: &Binding, rows: &[Row]) -> Result<(), RunError> {
    let Some(advisory) = binding.zone.as_deref() else { return Ok(()) };
    if rows.iter().any(|r| r.get(ZONE).and_then(Value::as_str) == Some(advisory)) {
        return Err(RunError::DeriveAdvisoryZone(format!(
            "engine `{engine}` rows carry zone `{advisory}` from the binding's advisory `zone` key; the adapter writes a row's zone"
        )));
    }
    Ok(())
}
