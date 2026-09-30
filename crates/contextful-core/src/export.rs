//! `run.export`: the `[[export]]` block, the batch statement a delivery reads past its
//! cursor in commit order, and the OTLP/HTTP JSON log records a batch becomes. Reading,
//! delivery and the cursor file live in adapters; this module performs no I/O.

use crate::connector::reference::Template;
use crate::store::lay_out::is_path_segment;
use crate::store::reconcile::ColumnType;
use crate::store::relation::ident;
use crate::store::reserve::{COMMIT_SEQ, INGESTED_AT, ROW_SEQ, RUN_ID};
use crate::time::Instant;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::BTreeMap;
use url::Url;

/// Rows one delivery batch holds (`run.export.batch-rows`).
pub const EXPORT_BATCH_ROWS: usize = 500;

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
    /// A target answering other than 2xx, or unreachable. (`run.export.delivery-refused`)
    #[error("ExportDeliveryRefused: {0}")]
    ExportDeliveryRefused(String),
    /// A block that does not parse, or a row export cannot place past a cursor.
    #[error("{0}")]
    Invalid(String),
}

/// The OTLP signal an export maps rows onto.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Signal {
    Logs,
}

impl Signal {
    pub fn name(self) -> &'static str {
        match self {
            Signal::Logs => "logs",
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

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Block {
    name: String,
    table: String,
    endpoint: String,
    #[serde(default)]
    signal: Option<String>,
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
    let signal = match block.signal.as_deref() {
        Some("logs") => Signal::Logs,
        Some(other) => {
            return Err(ExportError::ExportSignalUnknown(format!("export `{name}` names signal `{other}`; the built-in arm maps rows onto `logs` alone")));
        }
        None => return Err(ExportError::ExportSignalUnknown(format!("export `{name}` names no signal; the built-in arm maps rows onto `logs`"))),
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
    Ok(Export { name, table: block.table, endpoint, signal, headers })
}

impl Export {
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
            .map(|(c, v)| attribute(c, any_value(v, types.get(c).copied())))
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
