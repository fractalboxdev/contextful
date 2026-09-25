//! `store.reserve`: the `_` column namespace, the injected provenance columns, the
//! reserved table namespaces and the request-ledger path.

use super::reconcile::{Column, ColumnType, Schema};
use super::StoreError;

pub const INGESTED_AT: &str = "_ingested_at";
pub const RUN_ID: &str = "_run_id";
pub const BATCH_SEQ: &str = "_batch_seq";
pub const SITE_ID: &str = "_site_id";
pub const AUTHORED_BY: &str = "_authored_by";

/// Columns the engine injects, replacing any producer value (`store.reserve.injected`).
pub const INJECTED: [&str; 5] = [INGESTED_AT, RUN_ID, BATCH_SEQ, SITE_ID, AUTHORED_BY];

/// Injected columns every write path carries, so no file lacks them and each is non-null
/// in the merged schema whichever landing created it.
pub const ALWAYS_INJECTED: [&str; 3] = [INGESTED_AT, RUN_ID, SITE_ID];

/// Columns a producer may set inside the `_` namespace (`store.reserve.optional`).
pub const OPTIONAL: [&str; 4] = ["_modality", "_lang", "_provenance", "_prompt_hash"];

/// The values `_modality` takes (`store.reserve.modality`).
pub const MODALITIES: [&str; 5] = ["text", "image", "audio", "structured", "mixed"];

/// Age at which a request-ledger row is collected: 365 d (`store.reserve.ledger-retention`).
pub const LEDGER_RETENTION_SECS: u64 = 365 * 24 * 60 * 60;

/// The two table namespaces the engine holds (`store.reserve.table-namespaces`): the
/// durable run record, and the prefix mirrored permission data lands under.
pub const RESERVED_TABLE_NAMESPACES: [(&str, &str); 2] =
    [("_runs", "the durable run record"), ("access/", "the mirrored permission tables")];

/// What a write path knows about the batch it lands. A path with no batch scope or no
/// authenticated subject omits that column (`store.reserve.no-placeholder`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Injection {
    pub run_id: String,
    pub site_id: String,
    pub batch_seq: Option<i32>,
    pub authored_by: Option<String>,
}

impl Injection {
    /// The injected columns this write carries, in their fixed order.
    pub fn columns(&self) -> Vec<Column> {
        let mut cols = vec![
            Column::new(INGESTED_AT, ColumnType::Timestamp, false),
            Column::new(RUN_ID, ColumnType::Utf8, false),
        ];
        if self.batch_seq.is_some() {
            cols.push(Column::new(BATCH_SEQ, ColumnType::Int32, false));
        }
        cols.push(Column::new(SITE_ID, ColumnType::Utf8, false));
        if self.authored_by.is_some() {
            cols.push(Column::new(AUTHORED_BY, ColumnType::Utf8, false));
        }
        cols
    }
}

/// Whether a column name is one the engine injects.
pub fn is_injected(name: &str) -> bool {
    INJECTED.contains(&name)
}

/// Hold a producer schema to the namespace: an injected name is dropped for the engine
/// to replace; any other `_` name outside the optional set refuses
/// (`store.reserve.column-name`).
pub fn producer_columns(schema: &Schema) -> Result<Schema, StoreError> {
    let mut kept = Vec::new();
    for c in &schema.columns {
        if is_injected(&c.name) {
            continue;
        }
        if c.name.starts_with('_') && !OPTIONAL.contains(&c.name.as_str()) {
            return Err(StoreError::StoreReservedColumnName(format!(
                "column `{}` is inside the engine's `_` namespace; a producer sets only {}",
                c.name,
                OPTIONAL.join(", ")
            )));
        }
        kept.push(c.clone());
    }
    Ok(Schema { columns: kept })
}

/// Refuse a table name inside a reserved namespace (`store.reserve.table-name`).
pub fn check_table_name(table: &str) -> Result<(), StoreError> {
    for (ns, what) in RESERVED_TABLE_NAMESPACES {
        let hit = if ns.ends_with('/') { table.starts_with(ns) } else { table == ns || table.starts_with(&format!("{ns}/")) };
        if hit {
            return Err(StoreError::StoreReservedTableName(format!("table `{table}` is inside `{ns}`, reserved for {what}")));
        }
    }
    Ok(())
}

/// Validate a producer's optional-column value; `None` means it passes.
pub fn optional_value_problem(column: &str, value: &str) -> Option<String> {
    match column {
        "_modality" if !MODALITIES.contains(&value) => {
            Some(format!("`_modality` value `{value}` is not one of {}", MODALITIES.join(", ")))
        }
        "_prompt_hash" => {
            let hex = value.strip_prefix("sha256:");
            let ok = hex.is_some_and(|h| h.len() == 64 && h.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)));
            (!ok).then(|| format!("`_prompt_hash` value `{value}` is not `sha256:<64 lowercase hex>`"))
        }
        _ => None,
    }
}

/// A run's request-ledger path under the table directory (`store.reserve.ledger-path`).
pub fn ledger_path(run_id: &str, node_id: &str) -> String {
    format!("requests/{run_id}.{node_id}.parquet")
}
