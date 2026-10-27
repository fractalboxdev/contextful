//! `connector.record`: the operator record a `[secret.<name>]` table carries, the
//! value-free observe port, and the inventory row built from configured bindings.

use super::reference::{SecretName, SCHEME};
use super::ConnectorError;
use crate::pipeline::guard;
use crate::time::Instant;
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet};
use time::{Date, Month};

/// The manifest block holding operator records (`connector.record.record-table`).
pub const RECORD_BLOCK: &str = "secret";
/// The window within which the inventory marks an expiring credential: 30 d
/// (`connector.record.expiry-warning`).
pub const EXPIRY_WARNING_DAYS: i64 = 30;
/// The one explicit no-expiry assertion an operator record spells.
pub const NEVER: &str = "never";

/// An expiry: a date or instant, an explicit no-expiry assertion, or unknown
/// (`connector.record.expiry-unknown`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case", tag = "kind", content = "at")]
pub enum Expiry {
    Unknown,
    Never,
    At(Instant),
}

/// One documented consuming surface and its credential binding name
/// (`connector.record.operator-record`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Surface {
    pub surface: String,
    pub binding: String,
}

/// An operator record: descriptive metadata only, never a value or ciphertext
/// (`connector.record.operator-record`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct OperatorRecord {
    pub description: Option<String>,
    pub account: Option<String>,
    /// Opaque grant strings; `None` reads as unknown (`connector.record.unverified-scope-is-marked`).
    pub grants: Option<Vec<String>>,
    pub surfaces: Vec<Surface>,
    pub rotate: Option<String>,
    pub created: Option<Instant>,
    pub expires: Expiry,
}

impl Default for OperatorRecord {
    fn default() -> OperatorRecord {
        OperatorRecord { description: None, account: None, grants: None, surfaces: Vec::new(), rotate: None, created: None, expires: Expiry::Unknown }
    }
}

/// The value-withholding refusal of one record key.
fn rejected(file: &str, name: &str, path: &str, why: &str) -> ConnectorError {
    let at = if path.is_empty() { format!("{RECORD_BLOCK}.{name}") } else { format!("{RECORD_BLOCK}.{name}.{path}") };
    ConnectorError::SecretRecordRejected(format!("{file}: `{at}` {why}"))
}

/// A string checked for credential shape; the refusal names the key path, never the text.
fn text(file: &str, name: &str, path: &str, v: &toml::Value) -> Result<String, ConnectorError> {
    let s = v.as_str().ok_or_else(|| rejected(file, name, path, &format!("is a {}, not a string", v.type_str())))?;
    if !guard::spans(s).is_empty() {
        return Err(rejected(file, name, path, "holds a credential-shaped string; a record carries descriptive metadata only"));
    }
    Ok(s.to_string())
}

fn date(file: &str, name: &str, path: &str, v: &toml::Value) -> Result<Instant, ConnectorError> {
    let mistyped = || rejected(file, name, path, &format!("is a {}, not a TOML date", v.type_str()));
    let dt = v.as_datetime().ok_or_else(mistyped)?;
    match (dt.date, dt.time, dt.offset) {
        (Some(d), None, None) => Month::try_from(d.month)
            .ok()
            .and_then(|m| Date::from_calendar_date(i32::from(d.year), m, d.day).ok())
            .and_then(|d| Instant::from_unix_secs(d.midnight().assume_utc().unix_timestamp()).ok())
            .ok_or_else(|| rejected(file, name, path, "is no calendar date")),
        _ => Err(rejected(file, name, path, "is a date-time, not a TOML date")),
    }
}

/// Parse one `[secret.<name>]` table. Every refusal names `file`, the name and the key
/// path, and never the value (`connector.record.record-rejected`).
pub fn parse_record(file: &str, name: &str, table: &toml::Value) -> Result<OperatorRecord, ConnectorError> {
    let table = table.as_table().ok_or_else(|| rejected(file, name, "", &format!("is a {}, not a table", table.type_str())))?;
    let mut record = OperatorRecord::default();
    for (key, v) in table {
        match key.as_str() {
            "description" => record.description = Some(text(file, name, key, v)?),
            "account" => record.account = Some(text(file, name, key, v)?),
            "rotate" => record.rotate = Some(text(file, name, key, v)?),
            "grants" => {
                let items = v.as_array().ok_or_else(|| rejected(file, name, key, &format!("is a {}, not an array of strings", v.type_str())))?;
                record.grants = Some(items.iter().enumerate().map(|(i, g)| text(file, name, &format!("{key}[{i}]"), g)).collect::<Result<_, _>>()?);
            }
            "surfaces" => {
                let items = v.as_array().ok_or_else(|| rejected(file, name, key, &format!("is a {}, not an array of tables", v.type_str())))?;
                for (i, item) in items.iter().enumerate() {
                    let at = format!("{key}[{i}]");
                    let t = item.as_table().ok_or_else(|| rejected(file, name, &at, &format!("is a {}, not a table", item.type_str())))?;
                    if let Some(k) = t.keys().find(|k| !["surface", "binding"].contains(&k.as_str())) {
                        return Err(rejected(file, name, &format!("{at}.{k}"), "is no surface key; a surface carries `surface` and `binding`"));
                    }
                    let field = |k: &str| match t.get(k) {
                        Some(v) => text(file, name, &format!("{at}.{k}"), v),
                        None => Err(rejected(file, name, &format!("{at}.{k}"), "is missing")),
                    };
                    record.surfaces.push(Surface { surface: field("surface")?, binding: field("binding")? });
                }
            }
            "created" => record.created = Some(date(file, name, key, v)?),
            "expires" => {
                record.expires = match v.as_str() {
                    Some(NEVER) => Expiry::Never,
                    Some(_) => return Err(rejected(file, name, key, "is a string other than `\"never\"`, not a TOML date")),
                    None => Expiry::At(date(file, name, key, v)?),
                }
            }
            other => {
                return Err(rejected(
                    file,
                    name,
                    other,
                    "is no record key; a record carries `description`, `account`, `grants`, `surfaces`, `rotate`, `created` and `expires`",
                ))
            }
        }
    }
    Ok(record)
}

/// Read every operator record `manifest` holds. A record naming no configured binding in
/// `bound` refuses (`connector.record.record-rejected`).
pub fn read_records(file: &str, manifest: &toml::Value, bound: &BTreeSet<String>) -> Result<BTreeMap<String, OperatorRecord>, ConnectorError> {
    let mut out = BTreeMap::new();
    let Some(block) = manifest.get(RECORD_BLOCK) else { return Ok(out) };
    let block = block.as_table().ok_or_else(|| ConnectorError::SecretRecordRejected(format!("{file}: `{RECORD_BLOCK}` is a {}, not a table of records", block.type_str())))?;
    for (name, table) in block {
        if SecretName::parse(name).is_err() {
            return Err(ConnectorError::SecretRecordRejected(format!("{file}: a `{RECORD_BLOCK}` record name of {} bytes is no logical name", name.len())));
        }
        if !bound.contains(name) {
            return Err(rejected(file, name, "", "names no configured binding; a record describes a `secret://` name a declaration binds"));
        }
        out.insert(name.clone(), parse_record(file, name, table)?);
    }
    Ok(out)
}

/// Every logical name a `secret://` reference in `manifest` binds, outside the record block:
/// templates, whole-value references and every block that carries them. A string root is
/// a JSON manifest's text, scanned whole.
pub fn bound_names(manifest: &toml::Value) -> BTreeSet<String> {
    fn walk(v: &toml::Value, out: &mut BTreeSet<String>) {
        match v {
            toml::Value::String(s) => {
                let mut rest = s.as_str();
                while let Some(at) = rest.find(SCHEME) {
                    let tail = &rest[at + SCHEME.len()..];
                    let end = tail.find(|c: char| !(c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')).unwrap_or(tail.len());
                    if let Ok(n) = SecretName::parse(&tail[..end]) {
                        out.insert(n.as_str().to_string());
                    }
                    rest = &tail[end..];
                }
            }
            toml::Value::Array(items) => items.iter().for_each(|i| walk(i, out)),
            toml::Value::Table(t) => t.values().for_each(|i| walk(i, out)),
            _ => {}
        }
    }
    let mut out = BTreeSet::new();
    match manifest {
        toml::Value::Table(t) => t.iter().filter(|(k, _)| k.as_str() != RECORD_BLOCK).for_each(|(_, v)| walk(v, &mut out)),
        // A JSON manifest's text, whose string values hold each reference verbatim.
        toml::Value::String(_) => walk(manifest, &mut out),
        _ => {}
    }
    out
}

/// Why a metadata read is unavailable (`connector.record.observation-unavailable`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case", tag = "reason", content = "detail")]
pub enum Unavailable {
    Unsupported(String),
    Denied(String),
    Failed(String),
}

/// One adapter's metadata about a name, labelled with the adapter and the observation
/// time (`connector.record.observation-provenance`). No variant holds a hydrated value.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case", tag = "state")]
pub enum Observation {
    Observed { adapter: String, at: Instant, present: bool, version: Option<String>, created: Option<Instant>, expires: Expiry },
    Unavailable { adapter: String, at: Instant, #[serde(flatten)] reason: Unavailable },
}

impl Observation {
    pub fn adapter(&self) -> &str {
        match self {
            Observation::Observed { adapter, .. } | Observation::Unavailable { adapter, .. } => adapter,
        }
    }
}

/// An adapter's observe half (`connector.record.observe-port`): metadata for a name, never
/// material. The inventory holds observe halves only.
pub trait Observe: Send + Sync {
    /// The adapter's name, as attribution and the inventory label it.
    fn adapter(&self) -> &str;
    /// Observe `name` at `at`.
    fn observe(&self, name: &SecretName, at: Instant) -> Observation;
    /// Whether this adapter is configured to serve `name`; the lease provider serves its
    /// declared scopes alone.
    fn covers(&self, _name: &SecretName) -> bool {
        true
    }
}

/// The adapter that answered a name in the latest recorded run.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Answered {
    pub adapter: String,
    pub run_id: String,
    pub at: Instant,
}

/// A mark on a dated expiry (`connector.record.expiry-warning`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Mark {
    Within,
    Elapsed,
}

/// One expiry with its provenance: `operator`, or the observing adapter.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ExpiryView {
    pub provenance: String,
    pub expires: Expiry,
    /// Whole days remaining, for a dated expiry alone.
    pub days_remaining: Option<i64>,
    pub mark: Option<Mark>,
}

impl ExpiryView {
    fn of(provenance: &str, expires: Expiry, now: Instant) -> ExpiryView {
        let (days_remaining, mark) = match expires {
            Expiry::At(at) => {
                let days = (at.unix_secs() - now.unix_secs()).div_euclid(86_400);
                let mark = if at <= now {
                    Some(Mark::Elapsed)
                } else if days < EXPIRY_WARNING_DAYS {
                    Some(Mark::Within)
                } else {
                    None
                };
                (Some(days), mark)
            }
            Expiry::Never | Expiry::Unknown => (None, None),
        };
        ExpiryView { provenance: provenance.to_string(), expires, days_remaining, mark }
    }
}

/// One inventory row: a configured binding (`connector.record.inventory`). The configured
/// adapters and the answering adapter stay apart (`connector.record.configured-is-not-observed`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Row {
    pub name: String,
    pub configured: Vec<String>,
    pub answered: Option<Answered>,
    pub operator: Option<OperatorRecord>,
    pub observations: Vec<Observation>,
    pub expiries: Vec<ExpiryView>,
}

/// Build the row for one configured binding. The operator expiry and each observed one are
/// listed apart, each with its provenance, and an unknown one carries no days remaining.
pub fn inventory_row(name: &str, configured: Vec<String>, operator: Option<OperatorRecord>, observations: Vec<Observation>, answered: Option<Answered>, now: Instant) -> Row {
    let mut expiries = vec![ExpiryView::of("operator", operator.as_ref().map_or(Expiry::Unknown, |r| r.expires), now)];
    for o in &observations {
        if let Observation::Observed { adapter, expires, .. } = o {
            expiries.push(ExpiryView::of(adapter, *expires, now));
        }
    }
    Row { name: name.to_string(), configured, answered, operator, observations, expiries }
}
