//! `run.export`: the `[[export]]` block, the batch statement a delivery reads past its
//! cursor in commit order, and the OTLP/HTTP JSON log records a batch becomes. Reading,
//! delivery and the cursor file live in adapters; this module performs no I/O.

use crate::connector::reference::Template;
use crate::store::lay_out::is_path_segment;
use crate::store::reconcile::ColumnType;
use crate::store::relation::ident;
use crate::store::reserve::{COMMIT_SEQ, INGESTED_AT, ROW_SEQ, RUN_ID};
use crate::surface::arm::Schedule;
use crate::time::Instant;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet};
use url::Url;

/// Rows one delivery batch holds (`run.export.batch-rows`).
pub const EXPORT_BATCH_ROWS: usize = 500;
/// Maximum JSON request size for typed delivery.
pub const TYPED_REQUEST_BYTES: usize = 64 * 1024;

/// The resource's `service.name` on every delivered batch.
pub const SERVICE_NAME: &str = "contextful";

/// The instrumentation scope every delivered log record sits under.
pub const SCOPE_NAME: &str = "contextful.export";

/// The refusals of an export. `Display` begins with the identifier.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ExportError {
    /// A signal the built-in arm does not map. (`run.export.signal-unknown`)
    #[error("ExportSignalUnknown: {0}")]
    ExportSignalUnknown(String),
    /// Rows whose `_commit_seq` is null, which no cursor passes. (`run.export.commit-seq-missing`)
    #[error("ExportCommitSeqMissing: {0}")]
    ExportCommitSeqMissing(String),
    /// A target answering other than 2xx, or unreachable. (`run.export.delivery-refused`)
    #[error("ExportDeliveryRefused: {0}")]
    ExportDeliveryRefused(String),
    /// One typed event cannot fit a request by itself.
    #[error("ExportEventTooLarge: {0}")]
    ExportEventTooLarge(String),
    /// A block that does not parse, or a row export cannot place past a cursor.
    #[error("{0}")]
    Invalid(String),
}

/// The OTLP signal an export maps rows onto.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Signal {
    Logs,
    ChangesV1,
}

impl Signal {
    pub fn name(self) -> &'static str {
        match self {
            Signal::Logs => "logs",
            Signal::ChangesV1 => "changes-v1",
        }
    }
}

/// One validated `[[export]]` block.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Export {
    pub name: String,
    pub table: String,
    pub endpoint: Url,
    pub signal: Signal,
    /// Identity columns for typed state changes; empty for OTLP logs.
    pub key: Vec<String>,
    /// The delivery cadence for typed state changes.
    pub schedule: Option<Schedule>,
    /// Header templates, each hydrated per request.
    pub headers: BTreeMap<String, Template>,
}

/// Where an export stands: the last delivered row's commit sequence and row sequence.
/// The zero cursor stands before every row.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct ExportCursor {
    pub commit_seq: i64,
    pub row_seq: i64,
}

/// A consumer can persist `id` to deduplicate an at-least-once delivery.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ChangeEvent {
    pub version: u32,
    pub id: String,
    pub publication: String,
    pub sequence: u64,
    pub table: String,
    #[serde(flatten)]
    pub change: Change,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Change {
    Upsert { key: Value, row: Value },
    Delete { key: Value },
    PublicationComplete { changes: u64 },
}

/// Stable key encoding and the policy-filtered row visible to this export.
pub type ChangeState = BTreeMap<String, Value>;

/// Turn one complete read into a keyed state. Refuse missing, null or duplicate keys.
pub fn change_state(export: &Export, rows: &[Value]) -> Result<ChangeState, ExportError> {
    let mut state = BTreeMap::new();
    for row in rows {
        let object = row.as_object().ok_or_else(|| ExportError::Invalid(format!("export `{}`: a row is an object", export.name)))?;
        let mut key = serde_json::Map::new();
        for column in &export.key {
            let value = object.get(column).filter(|v| !v.is_null()).ok_or_else(|| ExportError::Invalid(format!("export `{}`: key column `{column}` is absent or null", export.name)))?;
            key.insert(column.clone(), value.clone());
        }
        let key = Value::Object(key);
        let encoded = serde_json::to_string(&key).map_err(|e| ExportError::Invalid(e.to_string()))?;
        let visible: serde_json::Map<String, Value> = object.iter().filter(|(k, _)| !k.starts_with('_')).map(|(k, v)| (k.clone(), v.clone())).collect();
        if state.insert(encoded.clone(), Value::Object(visible)).is_some() {
            return Err(ExportError::Invalid(format!("export `{}`: duplicate key {encoded}", export.name)));
        }
    }
    Ok(state)
}

/// Compare two complete states. A publication marker follows all changes, including an
/// empty publication, and every event id remains stable across retries.
pub fn change_events(export: &Export, publication: &str, first_sequence: u64, before: &ChangeState, after: &ChangeState) -> Result<Vec<ChangeEvent>, ExportError> {
    let keys: BTreeSet<&String> = before.keys().chain(after.keys()).collect();
    let mut events = Vec::new();
    for encoded in keys {
        let change = match (before.get(encoded), after.get(encoded)) {
            (Some(old), Some(new)) if old == new => continue,
            (_, Some(row)) => Change::Upsert { key: serde_json::from_str(encoded).map_err(|e| ExportError::Invalid(e.to_string()))?, row: row.clone() },
            (Some(_), None) => Change::Delete { key: serde_json::from_str(encoded).map_err(|e| ExportError::Invalid(e.to_string()))? },
            (None, None) => continue,
        };
        let sequence = first_sequence.checked_add(events.len() as u64).ok_or_else(|| ExportError::Invalid("event sequence overflow".into()))?;
        events.push(ChangeEvent { version: 1, id: format!("{}:{publication}:{sequence}", export.name), publication: publication.into(), sequence, table: export.table.clone(), change });
    }
    let changes = events.len() as u64;
    let sequence = first_sequence.checked_add(changes).ok_or_else(|| ExportError::Invalid("event sequence overflow".into()))?;
    events.push(ChangeEvent { version: 1, id: format!("{}:{publication}:{sequence}", export.name), publication: publication.into(), sequence, table: export.table.clone(), change: Change::PublicationComplete { changes } });
    Ok(events)
}

/// The longest prefix that fits one typed JSON request. The caller persists its cursor
/// only through the returned prefix after a successful answer.
pub fn typed_batch_len(events: &[ChangeEvent]) -> Result<usize, ExportError> {
    let mut bytes = b"{\"version\":1,\"events\":[]}".len();
    let mut count = 0;
    for event in events {
        let event_bytes = serde_json::to_vec(event).map_err(|e| ExportError::Invalid(e.to_string()))?.len();
        let next = bytes.checked_add(event_bytes).and_then(|n| n.checked_add(usize::from(count > 0)))
            .ok_or_else(|| ExportError::ExportEventTooLarge(format!("event `{}` exceeds {TYPED_REQUEST_BYTES} bytes", event.id)))?;
        if next > TYPED_REQUEST_BYTES {
            if count == 0 {
                return Err(ExportError::ExportEventTooLarge(format!("event `{}` exceeds {TYPED_REQUEST_BYTES} bytes", event.id)));
            }
            break;
        }
        bytes = next;
        count += 1;
    }
    Ok(count)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Block {
    name: String,
    table: String,
    endpoint: String,
    #[serde(default)]
    signal: Option<String>,
    #[serde(default)]
    format: Option<String>,
    #[serde(default)]
    key: Vec<String>,
    #[serde(default)]
    schedule: Option<String>,
    #[serde(default)]
    headers: BTreeMap<String, String>,
}

#[derive(Deserialize)]
struct Manifest {
    #[serde(default)]
    export: Vec<toml::Value>,
}

/// Every `[[export]]` block of `manifest`, validated.
pub fn parse_exports(manifest: &str) -> Result<Vec<Export>, ExportError> {
    let parsed: Manifest = toml::from_str(manifest).map_err(|e| ExportError::Invalid(format!("the manifest does not parse: {e}")))?;
    let mut out: Vec<Export> = Vec::new();
    for value in parsed.export {
        let name = value.get("name").and_then(|v| v.as_str()).unwrap_or("(unnamed)").to_string();
        let block: Block = value.try_into().map_err(|e| ExportError::Invalid(format!("export `{name}`: {e}")))?;
        let e = check(block)?;
        if out.iter().any(|o| o.name == e.name) {
            return Err(ExportError::Invalid(format!("export `{}` is declared more than once", e.name)));
        }
        out.push(e);
    }
    Ok(out)
}

fn check(block: Block) -> Result<Export, ExportError> {
    let name = block.name;
    if !is_path_segment(&name) || name.len() > 128 {
        return Err(ExportError::Invalid(format!("export name `{name}` is not 1 to 128 chars of [A-Za-z0-9._-]")));
    }
    let signal = match (block.format.as_deref(), block.signal.as_deref()) {
        (Some("changes-v1"), None) => Signal::ChangesV1,
        (None, Some("logs")) => Signal::Logs,
        (Some(other), _) => return Err(ExportError::Invalid(format!("export `{name}` names unsupported format `{other}`"))),
        (None, Some(other)) => {
            return Err(ExportError::ExportSignalUnknown(format!("export `{name}` names signal `{other}`; the built-in arm maps rows onto `logs` alone")));
        }
        (None, None) => return Err(ExportError::ExportSignalUnknown(format!("export `{name}` names no signal; the built-in arm maps rows onto `logs`"))),
    };
    let (key, schedule) = match signal {
        Signal::Logs if block.key.is_empty() && block.schedule.is_none() => (Vec::new(), None),
        Signal::Logs => return Err(ExportError::Invalid(format!("export `{name}`: `key` and `schedule` require `format = \"changes-v1\"`"))),
        Signal::ChangesV1 => {
            if block.key.is_empty() || block.key.iter().any(|k| k.is_empty() || k.starts_with('_')) {
                return Err(ExportError::Invalid(format!("export `{name}`: a typed export names non-system `key` columns")));
            }
            let unique: std::collections::BTreeSet<&str> = block.key.iter().map(String::as_str).collect();
            if unique.len() != block.key.len() {
                return Err(ExportError::Invalid(format!("export `{name}`: `key` columns are unique")));
            }
            let schedule = block.schedule.as_deref().ok_or_else(|| ExportError::Invalid(format!("export `{name}`: a typed export names `schedule`")))?;
            (block.key, Some(Schedule::parse(schedule).map_err(|e| ExportError::Invalid(format!("export `{name}`: {e}")))?))
        }
    };
    if block.table.trim().is_empty() {
        return Err(ExportError::Invalid(format!("export `{name}` names no `table`")));
    }
    let endpoint = Url::parse(&block.endpoint).map_err(|e| ExportError::Invalid(format!("export `{name}`: endpoint: {e}")))?;
    if !matches!(endpoint.scheme(), "http" | "https") || endpoint.host_str().is_none() {
        return Err(ExportError::Invalid(format!("export `{name}`: the endpoint is an http or https URL with a host")));
    }
    let mut headers = BTreeMap::new();
    for (header, value) in block.headers {
        let t = Template::parse(&value).map_err(|e| ExportError::Invalid(format!("export `{name}`: header `{header}`: {e}")))?;
        headers.insert(header, t);
    }
    Ok(Export { name, table: block.table, endpoint, signal, key, schedule, headers })
}

impl Export {
    /// The statement counting the rows no cursor passes: those whose `_commit_seq` is
    /// null, from parts landed without the column (`run.export.commit-seq-missing`).
    pub fn missing_statement(&self) -> String {
        format!("SELECT count(*) FROM {} WHERE {} IS NULL", ident(&self.table), ident(COMMIT_SEQ))
    }

    /// The refusal for `rows` rows without a commit sequence, or `None` when there are none.
    pub fn missing(&self, rows: i64) -> Option<ExportError> {
        (rows > 0).then(|| {
            ExportError::ExportCommitSeqMissing(format!(
                "export `{}`: table `{}` holds {rows} rows without `{COMMIT_SEQ}`, which no cursor passes; nothing was delivered",
                self.name, self.table
            ))
        })
    }

    /// The statement reading the next batch: the rows past `cursor` in commit order, at
    /// most [`EXPORT_BATCH_ROWS`] of them (`run.export.commit-order`).
    pub fn batch_statement(&self, cursor: &ExportCursor) -> String {
        let (c, r) = (ident(COMMIT_SEQ), ident(ROW_SEQ));
        format!(
            "SELECT * FROM {} WHERE {c} > {} OR ({c} = {} AND {r} > {}) ORDER BY {c}, {r} LIMIT {EXPORT_BATCH_ROWS}",
            ident(&self.table),
            cursor.commit_seq,
            cursor.commit_seq,
            cursor.row_seq
        )
    }
}

fn integer(v: &Value) -> Option<i64> {
    match v {
        Value::Number(n) => n.as_i64(),
        Value::String(s) => s.parse().ok(),
        _ => None,
    }
}

/// Reconstruct the stored scalar type from a read face's JSON string representation.
/// A mask that changes the representation remains a string in the exported row.
pub fn typed_cell(value: Value, ty: Option<&ColumnType>) -> Value {
    let Value::String(text) = &value else { return value };
    match ty {
        Some(ColumnType::Int32 | ColumnType::Int64) => text.parse::<i64>().ok().map(Value::from),
        Some(ColumnType::Float64) => text.parse::<f64>().ok().and_then(serde_json::Number::from_f64).map(Value::Number),
        Some(ColumnType::Json) => serde_json::from_str(text).ok(),
        _ => None,
    }.unwrap_or(value)
}

fn string_value(s: impl Into<String>) -> Value {
    json!({ "stringValue": s.into() })
}

/// The OTLP `AnyValue` a cell becomes: the column's stored type where it carries an
/// integer or a float, the JSON type otherwise; a nested value is its JSON text.
fn any_value(v: &Value, ty: Option<ColumnType>) -> Value {
    match (v, ty) {
        (_, Some(ColumnType::Int32 | ColumnType::Int64)) if integer(v).is_some() => json!({ "intValue": integer(v).unwrap_or_default().to_string() }),
        (Value::Number(n), _) if n.is_i64() || n.is_u64() => json!({ "intValue": n.to_string() }),
        (Value::Number(n), _) => json!({ "doubleValue": n.as_f64() }),
        (Value::String(s), Some(ColumnType::Float64)) if s.parse::<f64>().is_ok() => json!({ "doubleValue": s.parse::<f64>().unwrap_or_default() }),
        (Value::Bool(b), _) => json!({ "boolValue": b }),
        (Value::String(s), _) => string_value(s.clone()),
        (other, _) => string_value(other.to_string()),
    }
}

fn attribute(key: &str, value: Value) -> Value {
    json!({ "key": key, "value": value })
}

/// The OTLP/HTTP JSON request body for `rows` read under `columns`, one log record per
/// row (`run.export.log-record`), and the cursor standing after its last row. `types`
/// carries the table's stored column types.
pub fn log_records(export: &Export, columns: &[String], rows: &[Vec<Value>], types: &BTreeMap<String, ColumnType>) -> Result<(Value, Option<ExportCursor>), ExportError> {
    let at = |name: &str| columns.iter().position(|c| c == name);
    let (Some(commit), Some(row_seq)) = (at(COMMIT_SEQ), at(ROW_SEQ)) else {
        return Err(ExportError::Invalid(format!("export `{}` reads `{}` without `{COMMIT_SEQ}` and `{ROW_SEQ}`", export.name, export.table)));
    };
    let (ingested, run) = (at(INGESTED_AT), at(RUN_ID));
    let mut records = Vec::with_capacity(rows.len());
    let mut last = None;
    for row in rows {
        let position = |i: usize, what: &str| {
            row.get(i).and_then(integer).ok_or_else(|| ExportError::Invalid(format!("export `{}`: a row of `{}` carries no {what}", export.name, export.table)))
        };
        let cursor = ExportCursor { commit_seq: position(commit, COMMIT_SEQ)?, row_seq: position(row_seq, ROW_SEQ)? };
        let mut attributes: Vec<Value> = columns
            .iter()
            .zip(row)
            .filter(|(c, v)| !c.starts_with('_') && !v.is_null())
            .map(|(c, v)| attribute(c, any_value(v, types.get(c).cloned())))
            .collect();
        attributes.push(attribute("contextful.table", string_value(export.table.clone())));
        if let Some(run_id) = run.and_then(|i| row.get(i)).and_then(Value::as_str) {
            attributes.push(attribute("contextful.run_id", string_value(run_id)));
        }
        attributes.push(attribute("contextful.commit_seq", json!({ "intValue": cursor.commit_seq.to_string() })));
        attributes.push(attribute("contextful.row_seq", json!({ "intValue": cursor.row_seq.to_string() })));
        let mut record = serde_json::Map::new();
        if let Some(nanos) = ingested.and_then(|i| row.get(i)).and_then(Value::as_str).and_then(|s| Instant::parse(s).ok()).map(Instant::unix_nanos) {
            record.insert("timeUnixNano".into(), Value::String(nanos.to_string()));
        }
        record.insert("attributes".into(), Value::Array(attributes));
        records.push(Value::Object(record));
        last = Some(cursor);
    }
    let body = json!({
        "resourceLogs": [{
            "resource": { "attributes": [attribute("service.name", string_value(SERVICE_NAME)), attribute("contextful.export", string_value(export.name.clone()))] },
            "scopeLogs": [{ "scope": { "name": SCOPE_NAME }, "logRecords": records }],
        }]
    });
    Ok((body, last))
}
