//! `run.select` and `run.emit`: the outstanding set recomputed from the output table each
//! tick, and the rows a unit lands as — passages, or one marker.

use super::config::DeriveConfig;
use super::cues::{Cue, Parsed};
use crate::connector::attach::scrub_text;
use crate::run::journal::sha256_hex;
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

/// The column every passage and marker row carries its derivation key in (`run.emit.derivation-key`).
pub const DERIVATION_KEY: &str = "derivation_key";

/// The one column identifying a derived row (`run.emit.derived-id`).
pub const DERIVED_ID: &str = "derived_id";

/// A derived row's id: lowercase hex SHA-256 over its `unit_ref`, `derivation_key` and
/// `cue_seq` (`run.emit.derived-id`).
pub fn derived_id(unit_ref: &str, derivation_key: &str, cue_seq: i64) -> String {
    sha256_hex(&serde_json::to_vec(&json!([unit_ref, derivation_key, cue_seq])).expect("a JSON value serializes"))
}

/// What a unit's rows are derived under, besides the parent row: the engine id, the
/// binding's output-bearing parameters and the output table's declared columns.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Derivation {
    pub engine_id: String,
    /// `Binding::derivation_params`.
    pub binding: Value,
    /// The output table's declared columns.
    pub output_schema: Value,
}

impl Derivation {
    /// The key of one parent row: lowercase hex SHA-256 over this derivation and the parent's
    /// id, media value and own key, which a derive parent carries (`run.emit.derivation-key`).
    pub fn key(&self, parent_id: &str, media: &str, parent_key: Option<&str>) -> String {
        let doc = json!({
            "engine": self.engine_id,
            "binding": self.binding,
            "columns": self.output_schema,
            "parent": { "id": parent_id, "media": media, "key": parent_key },
        });
        sha256_hex(&serde_json::to_vec(&doc).expect("a JSON value serializes"))
    }
}

/// One unit to derive.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Unit {
    pub key: String,
    pub media: String,
    /// Attempts its marker under `derivation_key` records so far.
    pub prior_attempts: i64,
    /// The key its rows land under.
    pub derivation_key: String,
}

/// The outstanding set of one tick.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Selection {
    pub outstanding: Vec<Unit>,
    /// Parent rows skipped for a missing key or media value.
    pub incomplete: Vec<RunError>,
    /// The counts a dry run prints (`run.select.dry-run`).
    pub counts: Counts,
}

/// One selection's unit counts: every distinct complete parent, those already settled under
/// their current key, and those outstanding before the run's row budget truncates them.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize)]
pub struct Counts {
    pub eligible: u64,
    pub derived: u64,
    pub outstanding: u64,
}

fn text(v: Option<&Value>) -> Option<String> {
    match v? {
        Value::String(s) if !s.trim().is_empty() => Some(s.clone()),
        Value::Number(n) => Some(n.to_string()),
        _ => None,
    }
}

/// The columns of the output table `select` reads.
pub const OUTPUT_COLUMNS: [&str; 9] =
    ["unit_ref", DERIVATION_KEY, KIND, "attempts", "unit_status", "retryable", "_ingested_at", "_run_id", "_row_seq"];

/// A row's place in landing order: `_ingested_at` (fixed-width RFC 3339), `_run_id`,
/// `_row_seq`, then its position in the read.
type Recency = (String, String, i64, usize);

fn recency(r: &Row, position: usize) -> Recency {
    let s = |k: &str| r.get(k).and_then(Value::as_str).unwrap_or_default().to_string();
    (s("_ingested_at"), s("_run_id"), r.get("_row_seq").and_then(Value::as_i64).unwrap_or(0), position)
}

/// One landed row of a unit, as standing reads it.
pub(super) struct Landed {
    key: Option<String>,
    at: Recency,
    passage: bool,
    status: UnitStatus,
    attempts: i64,
    permanent: bool,
}

impl Landed {
    fn read(r: &Row, position: usize) -> Landed {
        Landed {
            key: text(r.get(DERIVATION_KEY)),
            at: recency(r, position),
            passage: r.get(KIND).and_then(Value::as_str) != Some("marker"),
            status: r.get("unit_status").and_then(Value::as_str).and_then(|s| UnitStatus::parse(s).ok()).unwrap_or(UnitStatus::Failed),
            attempts: r.get("attempts").and_then(Value::as_i64).unwrap_or(0),
            permanent: r.get("retryable").and_then(Value::as_bool) == Some(false),
        }
    }

    /// Whether this row establishes what its key derives: passages, or an `ok` or `empty`
    /// marker. Only such a landing supersedes another key's rows.
    fn establishes(&self) -> bool {
        self.passage || matches!(self.status, UnitStatus::Ok | UnitStatus::Empty)
    }
}

/// Every unit's landed rows, keyed by `unit_ref`.
pub(super) fn by_unit(derived: &[Row]) -> BTreeMap<String, Vec<Landed>> {
    grouped(derived, false)
}

/// A row's supersession group: its `unit_ref`, joined with its `task_version` when the
/// table retains versions (`run.emit.version-retained`).
fn group_of(r: &Row, by_version: bool) -> Option<String> {
    let unit = text(r.get("unit_ref"))?;
    if !by_version {
        return Some(unit);
    }
    let version = r.get(super::task::TASK_VERSION).and_then(Value::as_str).unwrap_or_default();
    Some(format!("{unit}\u{0}{version}"))
}

fn grouped(derived: &[Row], by_version: bool) -> BTreeMap<String, Vec<Landed>> {
    let mut units: BTreeMap<String, Vec<Landed>> = BTreeMap::new();
    for (position, r) in derived.iter().enumerate() {
        if let Some(group) = group_of(r, by_version) {
            units.entry(group).or_default().push(Landed::read(r, position));
        }
    }
    units
}

/// A unit's latest landing that establishes its key: that key and when it landed.
fn current(rows: &[Landed]) -> Option<(Option<&str>, &Recency)> {
    rows.iter().filter(|l| l.establishes()).max_by(|a, b| a.at.cmp(&b.at)).map(|l| (l.key.as_deref(), &l.at))
}

/// A unit's standing under `key`: `None` when it settled there, else the attempts its
/// latest marker there records. Rows under `key` landed before the unit's latest
/// establishing landing under another key count for nothing (`run.select.key-change`).
pub(super) fn standing(rows: &[Landed], key: &str, max_attempts: i64) -> Option<i64> {
    let since = match current(rows) {
        Some((k, _)) if k == Some(key) => return None,
        Some((_, at)) => Some(at),
        None => None,
    };
    let latest = rows
        .iter()
        .filter(|l| l.key.as_deref() == Some(key) && since.is_none_or(|at| l.at > *at))
        .max_by(|a, b| a.at.cmp(&b.at));
    match latest {
        None => Some(0),
        Some(l) if l.establishes() || l.permanent || l.attempts >= max_attempts => None,
        Some(l) => Some(l.attempts),
    }
}

/// Recompute the outstanding set: every parent row holding neither passages nor a settled
/// marker under its current derivation key in the output table, truncated to the run's row
/// budget after the anti-join. A parent missing its key or media value is skipped and
/// reported.
pub fn select(parents: &[Row], derived: &[Row], config: &DeriveConfig, derivation: &Derivation) -> Selection {
    let mut selector = Selector::new(derived, config, derivation);
    selector.feed(parents);
    selector.finish()
}

/// [`select`] over parent rows fed in batches, so selection holds one batch of parents and
/// the outstanding units within the run's row budget (`run.select.parent-scan`).
pub struct Selector<'a> {
    units: BTreeMap<String, Vec<Landed>>,
    config: &'a DeriveConfig,
    derivation: &'a Derivation,
    seen: std::collections::BTreeSet<String>,
    budget: usize,
    sel: Selection,
}

impl<'a> Selector<'a> {
    pub fn new(derived: &[Row], config: &'a DeriveConfig, derivation: &'a Derivation) -> Selector<'a> {
        let budget = usize::try_from(config.max_rows_per_run).unwrap_or(usize::MAX);
        Selector { units: by_unit(derived), config, derivation, seen: Default::default(), budget, sel: Selection::default() }
    }

    /// Select from one batch of parent rows.
    pub fn feed(&mut self, parents: &[Row]) {
        let config = self.config;
        for r in parents {
            let (Some(key), Some(media)) = (text(r.get(&config.parent_id_column)), text(r.get(&config.media_column))) else {
                self.sel.incomplete.push(RunError::DeriveUnitIncomplete(format!(
                    "a `{}` row lacks `{}` or `{}`; it is skipped",
                    config.source_table, config.parent_id_column, config.media_column
                )));
                continue;
            };
            if !self.seen.insert(key.clone()) {
                continue;
            }
            self.sel.counts.eligible += 1;
            let derivation_key = self.derivation.key(&key, &media, text(r.get(DERIVATION_KEY)).as_deref());
            let prior = match self.units.get(&key) {
                None => 0,
                Some(rows) => match standing(rows, &derivation_key, config.max_attempts) {
                    None => {
                        self.sel.counts.derived += 1;
                        continue;
                    }
                    Some(attempts) => attempts,
                },
            };
            self.sel.counts.outstanding += 1;
            if self.sel.outstanding.len() < self.budget {
                self.sel.outstanding.push(Unit { key, media, prior_attempts: prior, derivation_key });
            }
        }
    }

    pub fn finish(self) -> Selection {
        self.sel
    }
}

/// Refuse landing `unit`'s rows when the output table already holds it settled under its
/// key, as a concurrent tick lands it (`run.emit.settled-revived`).
pub fn revived(unit: &Unit, derived: &[Row], max_attempts: i64) -> Option<RunError> {
    let units = by_unit(derived);
    let rows = units.get(&unit.key)?;
    standing(rows, &unit.derivation_key, max_attempts).is_none().then(|| {
        RunError::DeriveSettledUnitRevived(format!(
            "unit `{}` settled under key `{}` while this tick derived it; its rows are not landed",
            unit.key,
            &unit.derivation_key[..unit.derivation_key.len().min(12)]
        ))
    })
}

/// The columns deciding whether a derived row is superseded.
pub const SUPERSEDE_COLUMNS: [&str; 7] = ["unit_ref", DERIVATION_KEY, KIND, "unit_status", "_ingested_at", "_run_id", "_row_seq"];

/// Per row of `derived`, whether it is superseded: landed under another key before its
/// unit's latest `ok` or `empty` landing (`run.emit.stale-supersedes`).
pub fn superseded(derived: &[Row]) -> Vec<bool> {
    superseded_in(derived, false)
}

/// Per row of a table declaring `retain_versions`, whether it is superseded: landed under
/// another key before the latest `ok` or `empty` landing of its unit under the same
/// `task_version` (`run.emit.version-retained`).
pub fn superseded_within_version(derived: &[Row]) -> Vec<bool> {
    superseded_in(derived, true)
}

fn superseded_in(derived: &[Row], by_version: bool) -> Vec<bool> {
    let units = grouped(derived, by_version);
    let latest: BTreeMap<&str, (Option<&str>, &Recency)> = units.iter().filter_map(|(u, rows)| Some((u.as_str(), current(rows)?))).collect();
    derived
        .iter()
        .enumerate()
        .map(|(position, r)| {
            let Some(unit) = group_of(r, by_version) else { return false };
            let Some((key, at)) = latest.get(unit.as_str()) else { return false };
            let own = text(r.get(DERIVATION_KEY));
            own.as_deref() != *key && recency(r, position) < **at
        })
        .collect()
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
                DERIVATION_KEY: unit.derivation_key, DERIVED_ID: derived_id(&unit.key, &unit.derivation_key, i as i64), "_modality": "text",
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
        DERIVATION_KEY: unit.derivation_key, DERIVED_ID: derived_id(&unit.key, &unit.derivation_key, MARKER_SEQ),
    });
    v.as_object().cloned().unwrap_or_default()
}

/// Refuse a row whose non-null `last_error` has not passed [`redact`]: an address in it
/// still carrying credentials, a query or a fragment (`run.emit.unredacted-error`).
pub fn check_last_error(row: &Row) -> Result<(), RunError> {
    let Some(error) = row.get("last_error").and_then(Value::as_str) else { return Ok(()) };
    if redact(error) != error {
        return Err(RunError::DeriveUnredactedError(format!(
            "unit `{}` writes a `last_error` holding an address that has not passed redaction",
            row.get("unit_ref").and_then(Value::as_str).unwrap_or_default()
        )));
    }
    Ok(())
}
