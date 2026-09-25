//! `run.advance`: cursor kinds, the watermark a monotonic position serializes as, the
//! frontier over fetched rows, and the rules an opening position is held to.

use super::RunError;
use crate::time::Instant;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::cmp::Ordering;

/// A cursor's declared kind (`run.advance.cursor-kind`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum CursorKind {
    /// A timestamp, autoincrement or watermark.
    Monotonic,
    /// A vendor continuation token.
    OpaqueToken,
    /// A log sequence number or version id.
    SnapshotId,
}

impl CursorKind {
    pub fn name(self) -> &'static str {
        match self {
            CursorKind::Monotonic => "monotonic",
            CursorKind::OpaqueToken => "opaque-token",
            CursorKind::SnapshotId => "snapshot-id",
        }
    }

    /// Resolve a declared kind. An undeclared kind resolves to `opaque-token`.
    pub fn resolve(declared: Option<&str>) -> Result<CursorKind, RunError> {
        match declared {
            None => Ok(CursorKind::OpaqueToken),
            Some("monotonic") => Ok(CursorKind::Monotonic),
            Some("opaque-token") => Ok(CursorKind::OpaqueToken),
            Some("snapshot-id") => Ok(CursorKind::SnapshotId),
            Some(other) => Err(RunError::Invalid(format!(
                "cursor kind `{other}` is none of `monotonic`, `opaque-token` and `snapshot-id`"
            ))),
        }
    }

    /// Whether a position of this kind moves only under a single-writer lease
    /// (`run.advance.concurrency-by-kind`). A monotonic position is concurrent-safe.
    pub fn single_writer(self) -> bool {
        self != CursorKind::Monotonic
    }
}

/// Resolve two writers' positions of one cursor. A monotonic position commits the
/// highest value observed; every other kind has one writer, so a second position is a
/// conflict the lease exists to prevent, never a last-write-wins.
pub fn resolve_concurrent(kind: CursorKind, a: &Value, b: &Value) -> Result<Value, RunError> {
    match kind {
        CursorKind::Monotonic => {
            let (fa, fb) = (watermark_at(a)?, watermark_at(b)?);
            Ok(if compare(fa, fb)? == Ordering::Less { b.clone() } else { a.clone() })
        }
        _ if a == b => Ok(a.clone()),
        _ => Err(RunError::Invalid(format!(
            "two writers moved a `{}` cursor to different positions; it moves only under its single-writer lease",
            kind.name()
        ))),
    }
}

/// A watermark position: `{"field": "<name>", "at": <value>}` (`run.advance.watermark-shape`).
pub fn watermark(field: &str, at: Value) -> Value {
    serde_json::json!({ "field": field, "at": at })
}

fn watermark_at(position: &Value) -> Result<&Value, RunError> {
    position
        .get("at")
        .ok_or_else(|| RunError::Invalid(format!("position {position} is not a watermark `{{\"field\", \"at\"}}`")))
}

/// Open a stored watermark against the declared `incremental` field. A different stored
/// field refuses before any request leaves the host (`run.advance.field-rename`).
pub fn open_watermark<'a>(stored: Option<&'a Value>, declared_field: &str) -> Result<Option<&'a Value>, RunError> {
    let Some(position) = stored else { return Ok(None) };
    let field = position.get("field").and_then(Value::as_str).unwrap_or_default();
    if field != declared_field {
        return Err(RunError::CursorFieldMismatch(format!(
            "the stored position was measured against `{field}` and the pipeline declares `incremental = \"{declared_field}\"`; reset the position to change the field"
        )));
    }
    Ok(Some(watermark_at(position)?))
}

/// The order of two clock values: numbers numerically, instants as instants, other text
/// lexically. A value with no order, or text against a number, refuses
/// (`run.advance.unorderable-position`).
pub fn compare(a: &Value, b: &Value) -> Result<Ordering, RunError> {
    match (a, b) {
        (Value::Number(x), Value::Number(y)) => match (x.as_i64(), y.as_i64()) {
            (Some(x), Some(y)) => Ok(x.cmp(&y)),
            _ => x.as_f64().zip(y.as_f64()).and_then(|(x, y)| x.partial_cmp(&y)).ok_or_else(|| unorderable(a, b)),
        },
        (Value::String(x), Value::String(y)) => match (Instant::parse(x), Instant::parse(y)) {
            (Ok(x), Ok(y)) => Ok(x.cmp(&y)),
            _ => Ok(x.cmp(y)),
        },
        _ => Err(unorderable(a, b)),
    }
}

fn unorderable(a: &Value, b: &Value) -> RunError {
    RunError::CursorPositionUnorderable(format!("clock values {a} and {b} have no common order"))
}

/// The frontier of one fetched window: the highest clock value over every fetched row,
/// landed or not. A row with no orderable value refuses, terminal for the pull.
pub fn frontier<'a>(field: &str, rows: impl IntoIterator<Item = &'a serde_json::Map<String, Value>>) -> Result<Option<Value>, RunError> {
    let mut best: Option<&Value> = None;
    for row in rows {
        let v = row.get(field).filter(|v| v.is_number() || v.is_string()).ok_or_else(|| {
            RunError::CursorPositionUnorderable(format!(
                "a row carries {} in the clock field `{field}`",
                row.get(field).map_or("no value".to_string(), Value::to_string)
            ))
        })?;
        best = Some(match best {
            Some(b) if compare(b, v)? != Ordering::Less => b,
            _ => v,
        });
    }
    Ok(best.cloned())
}

/// The position a window commits: forward to the frontier, or held when the window was
/// empty or older. A committed position never rewinds (`run.advance.frontier`).
pub fn advance(stored: Option<&Value>, frontier: Option<&Value>) -> Result<Option<Value>, RunError> {
    Ok(match (stored, frontier) {
        (None, f) => f.cloned(),
        (Some(s), None) => Some(s.clone()),
        (Some(s), Some(f)) => Some(if compare(s, f)? == Ordering::Less { f.clone() } else { s.clone() }),
    })
}

/// Whether a polled load admits a row at clock value `v` against stored position `at`:
/// at or after it, so the boundary instant's rows re-land on every poll
/// (`run.advance.inclusive-boundary`).
pub fn admits(at: Option<&Value>, v: &Value) -> Result<bool, RunError> {
    match at {
        None => Ok(true),
        Some(at) => Ok(compare(v, at)? != Ordering::Less),
    }
}
