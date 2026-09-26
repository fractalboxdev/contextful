//! The request ledger's row and the child relation a table's ledger registers as.
//!
//! A ledger row is operational metadata of one mediated outbound call: identifiers,
//! connector, method, host, status and timing. It carries no payload and no row content.
//! `run_id` and `batch_seq` join it onto a data row's `_run_id` and `_batch_seq`
//! (`run.land.batch-write`).

use super::reconcile::{Column, ColumnType};
use super::relation::{ident, literal};
use crate::time::Instant;

/// The suffix naming a table's request-ledger relation (`read.register.ledger-relation`).
pub const LEDGER_RELATION_SUFFIX: &str = "__requests";

/// The child relation `<table>__requests` a table's request ledger registers as.
pub fn ledger_relation(table: &str) -> String {
    format!("{table}{LEDGER_RELATION_SUFFIX}")
}

/// The table a request-ledger relation name belongs to, or `None` for any other name.
pub fn ledger_table(relation: &str) -> Option<&str> {
    relation.strip_suffix(LEDGER_RELATION_SUFFIX).filter(|t| !t.is_empty())
}

/// One mediated outbound call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RequestRecord {
    /// The host-assigned identifier, present on every row.
    pub request_id: String,
    /// The vendor's own correlation identifier, where the response echoed one.
    pub vendor_request_id: Option<String>,
    /// The connector whose traffic this was.
    pub connector: String,
    /// The HTTP method, uppercase.
    pub method: String,
    /// The request's host, with no path, query or userinfo.
    pub url_host: String,
    /// The response status, or `None` where no response arrived.
    pub status_code: Option<u16>,
    /// When the request left the mediated client.
    pub started_at: Instant,
    /// Wall time from dispatch to response or transport failure.
    pub duration_ms: u64,
    /// The batch ordinal the call's scope produced, or `None` where the scope produced no
    /// batch; the row lands either way.
    pub batch_seq: Option<i32>,
}

/// The ledger's columns in file order: the run id, then each [`RequestRecord`] field.
pub fn ledger_columns() -> Vec<Column> {
    vec![
        Column::new("run_id", ColumnType::Utf8, false),
        Column::new("batch_seq", ColumnType::Int32, true),
        Column::new("request_id", ColumnType::Utf8, false),
        Column::new("vendor_request_id", ColumnType::Utf8, true),
        Column::new("connector", ColumnType::Utf8, false),
        Column::new("method", ColumnType::Utf8, false),
        Column::new("url_host", ColumnType::Utf8, false),
        Column::new("status_code", ColumnType::Int32, true),
        Column::new("started_at", ColumnType::Timestamp, false),
        Column::new("duration_ms", ColumnType::Int64, false),
    ]
}

/// The SQL a table's ledger registers as: its explicit file list projected to the ledger
/// columns, or a zero-row relation over them where no call has been recorded.
pub fn ledger_sql(files: &[String]) -> String {
    let columns = ledger_columns();
    if files.is_empty() {
        let nulls: Vec<String> = columns.iter().map(|c| format!("CAST(NULL AS {}) AS {}", c.ty.sql(), ident(&c.name))).collect();
        return format!("SELECT {} WHERE false", nulls.join(", "));
    }
    let names: Vec<String> = columns.iter().map(|c| ident(&c.name)).collect();
    let list: Vec<String> = files.iter().map(|f| literal(f)).collect();
    format!(
        "SELECT {} FROM read_parquet([{}], union_by_name = true, hive_partitioning = false)",
        names.join(", "),
        list.join(", ")
    )
}
