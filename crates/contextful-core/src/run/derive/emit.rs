//! `run.select` and `run.emit`: the outstanding set recomputed from the output table each
//! tick, and the rows a unit lands as — passages, or one marker.

use super::config::DeriveConfig;
use super::cues::{Cue, Parsed};
use crate::connector::attach::scrub_text;
use crate::run::ports::Row;
use crate::run::RunError;
use serde_json::{json, Value};
use std::collections::BTreeMap;

/// The column telling a passage from a marker, reserved to the tier.
pub const KIND: &str = "kind";
/// The `cue_seq` a marker takes.
pub const MARKER_SEQ: i64 = -1;
/// Attempts an established-empty unit receives: 1 attempt (`run.emit.attempts`).
pub const EMPTY_ATTEMPTS: i64 = 1;

/// What a unit's derivation established (`run.emit.unit-status`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnitStatus {
    Ok,
    /// The engine established there is nothing to derive.
    Empty,
    /// The engine returned nothing and stated no reason.
    Unavailable,
    /// A typed error.
    Failed,
}

impl UnitStatus {
    pub fn name(self) -> &'static str {
        match self {
            UnitStatus::Ok => "ok",
            UnitStatus::Empty => "empty",
            UnitStatus::Unavailable => "unavailable",
            UnitStatus::Failed => "failed",
        }
    }

    pub fn parse(s: &str) -> Result<UnitStatus, RunError> {
        match s {
            "ok" => Ok(UnitStatus::Ok),
            "empty" => Ok(UnitStatus::Empty),
            "unavailable" => Ok(UnitStatus::Unavailable),
            "failed" => Ok(UnitStatus::Failed),
            other => Err(RunError::DeriveUnitStatusUnknown(format!("unit status `{other}` is none of ok, empty, unavailable and failed"))),
        }
    }
}

/// One unit to derive.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Unit {
    pub key: String,
    pub media: String,
    /// Attempts its marker records so far.
    pub prior_attempts: i64,
}

/// The outstanding set of one tick.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Selection {
    pub outstanding: Vec<Unit>,
    /// Parent rows skipped for a missing key or media value.
    pub incomplete: Vec<RunError>,
}

fn text(v: Option<&Value>) -> Option<String> {
    match v? {
        Value::String(s) if !s.trim().is_empty() => Some(s.clone()),
        Value::Number(n) => Some(n.to_string()),
        _ => None,
    }
}

/// The columns of the output table `select` reads.
pub const OUTPUT_COLUMNS: [&str; 8] = ["unit_ref", KIND, "attempts", "unit_status", "retryable", "_ingested_at", "_run_id", "_row_seq"];

/// A marker's place in landing order: `_ingested_at` (fixed-width RFC 3339), `_run_id`,
/// `_row_seq`, then its position in the read.
type Recency = (String, String, i64, usize);

fn recency(r: &Row, position: usize) -> Recency {
    let s = |k: &str| r.get(k).and_then(Value::as_str).unwrap_or_default().to_string();
    (s("_ingested_at"), s("_run_id"), r.get("_row_seq").and_then(Value::as_i64).unwrap_or(0), position)
}

/// The attempts a unit's latest marker records and whether it settled, per unit.
fn markers(derived: &[Row], max_attempts: i64) -> (BTreeMap<String, (i64, bool)>, BTreeMap<String, ()>) {
    let mut marked: BTreeMap<String, (Recency, i64, bool)> = BTreeMap::new();
    let mut derived_units = BTreeMap::new();
    for (position, r) in derived.iter().enumerate() {
        let Some(unit) = text(r.get("unit_ref")) else { continue };
        if r.get(KIND).and_then(Value::as_str) != Some("marker") {
            derived_units.insert(unit, ());
            continue;
        }
        let attempts = r.get("attempts").and_then(Value::as_i64).unwrap_or(0);
        let status = r.get("unit_status").and_then(Value::as_str).and_then(|s| UnitStatus::parse(s).ok()).unwrap_or(UnitStatus::Failed);
        let permanent = r.get("retryable").and_then(Value::as_bool) == Some(false);
        let settled = matches!(status, UnitStatus::Ok | UnitStatus::Empty) || permanent || attempts >= max_attempts;
        let at = recency(r, position);
        if marked.get(&unit).is_none_or(|(latest, _, _)| at > *latest) {
            marked.insert(unit, (at, attempts, settled));
        }
    }
    (marked.into_iter().map(|(u, (_, a, s))| (u, (a, s))).collect(), derived_units)
}

/// Recompute the outstanding set: every parent row not already holding passages or a
/// settled marker in the output table, truncated to the run's row budget after the
/// anti-join. A parent missing its key or media value is skipped and reported.
pub fn select(parents: &[Row], derived: &[Row], config: &DeriveConfig) -> Selection {
    let (marked, done) = markers(derived, config.max_attempts);
    let mut sel = Selection::default();
    let mut seen = BTreeMap::new();
    for r in parents {
        let (Some(key), Some(media)) = (text(r.get(&config.parent_id_column)), text(r.get(&config.media_column))) else {
            sel.incomplete.push(RunError::DeriveUnitIncomplete(format!(
                "a `{}` row lacks `{}` or `{}`; it is skipped",
                config.source_table, config.parent_id_column, config.media_column
            )));
            continue;
        };
        if done.contains_key(&key) || seen.insert(key.clone(), ()).is_some() {
            continue;
        }
        let prior = match marked.get(&key) {
            Some((_, true)) => continue,
            Some((attempts, false)) => *attempts,
            None => 0,
        };
        sel.outstanding.push(Unit { key, media, prior_attempts: prior });
    }
    sel.outstanding.truncate(usize::try_from(config.max_rows_per_run).unwrap_or(usize::MAX));
    sel
}

/// What a unit's cue document establishes: `ok` when it yields passages, `empty` only for
/// WebVTT whose blocks are all metadata, and `unavailable` for anything else.
pub fn document_status(_document: &str, parsed: &Parsed) -> UnitStatus {
    if !parsed.cues.is_empty() {
        UnitStatus::Ok
    } else if parsed.webvtt && parsed.unread == 0 {
        UnitStatus::Empty
    } else {
        UnitStatus::Unavailable
    }
}

/// A failure's text as it lands: every address reduced to scheme, host, port and path.
pub fn redact(message: &str) -> String {
    message
        .split(' ')
        .map(|w| if w.contains("://") { scrub_text(w) } else { w.to_string() })
        .collect::<Vec<_>>()
        .join(" ")
}

/// The passage rows of a derived unit, `cue_seq` 0..N.
pub fn passage_rows(unit: &Unit, passages: &[Cue], engine_id: &str) -> Vec<Row> {
    passages
        .iter()
        .enumerate()
        .map(|(i, p)| {
            let v = json!({
                "unit_ref": unit.key, "cue_seq": i as i64, KIND: "passage", "text": p.text,
                "start_ms": p.start_ms as i64, "end_ms": p.end_ms as i64,
                "unit_status": UnitStatus::Ok.name(), "attempts": unit.prior_attempts + 1, "engine_id": engine_id,
            });
            v.as_object().cloned().unwrap_or_default()
        })
        .collect()
}

/// The one marker row a unit that produced no passage lands as. `attempts` is the prior
/// count plus one; an established-empty unit takes its one attempt and settles.
pub fn marker_row(unit: &Unit, status: UnitStatus, error: Option<&str>, retryable: bool, engine_id: &str) -> Row {
    let attempts = if status == UnitStatus::Empty { EMPTY_ATTEMPTS } else { unit.prior_attempts + 1 };
    let v = json!({
        "unit_ref": unit.key, "cue_seq": MARKER_SEQ, KIND: "marker", "unit_status": status.name(),
        "attempts": attempts, "last_error": error.map(redact), "retryable": retryable, "engine_id": engine_id,
    });
    v.as_object().cloned().unwrap_or_default()
}
