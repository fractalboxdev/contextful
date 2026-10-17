//! The item record (`run.items.outcome-record`): the engine's reserved `_items` table,
//! holding one row per terminal isolated item, appended as the item's receipt commits.
//! It reads under the store's bounds like any other table (`read.register.reserved-relation`).

use crate::error::{ContextError, Result};
use crate::land::{land_engine_owned, Batch, RunContext};
use crate::store::Store;
use contextful_core::run::drive::ItemReceipt;
use contextful_core::run::record::cap_error;
use contextful_core::store::declare::TableDecl;
use contextful_core::store::lay_out::{NodeId, RunManifest};
use contextful_core::store::reconcile::ColumnType;
use contextful_core::store::reserve::{Injection, ITEM_RECORD_TABLE};
use contextful_core::time::Instant;
use serde_json::{Map, Value};

/// The columns each row carries, with their types.
const COLUMNS: [(&str, ColumnType); 10] = [
    ("job", ColumnType::Utf8),
    ("logical_key", ColumnType::Utf8),
    ("entity_namespace", ColumnType::Utf8),
    ("entity_key", ColumnType::Utf8),
    ("attempts", ColumnType::Int64),
    ("outcome", ColumnType::Utf8),
    ("reason", ColumnType::Utf8),
    ("run_id", ColumnType::Utf8),
    ("execution_id", ColumnType::Utf8),
    ("ended_at", ColumnType::Utf8),
];

/// The item record's declaration: unkeyed, so every appended row stays readable.
pub fn decl() -> TableDecl {
    TableDecl::named(ITEM_RECORD_TABLE)
}

/// A key cell as text: a string as itself, any other value as its JSON.
fn key_text(v: &Value) -> Value {
    Value::String(match v {
        Value::String(s) => s.clone(),
        other => other.to_string(),
    })
}

/// Append the row `receipt` names for the item executing as `execution_id`, as one store
/// run of its own on `node` at `at`. A receipt naming no item identity appends nothing.
pub fn append(store: &Store, node: &NodeId, site_id: &str, execution_id: &str, receipt: &ItemReceipt, at: Instant) -> Result<Option<RunManifest>> {
    let Some(item) = &receipt.item else { return Ok(None) };
    let mut fields = Map::new();
    fields.insert("job".into(), Value::String(item.job.clone()));
    fields.insert("logical_key".into(), key_text(&item.logical_key));
    fields.insert("entity_namespace".into(), Value::String(item.entity_namespace.clone()));
    fields.insert("entity_key".into(), key_text(&item.entity_key));
    fields.insert("attempts".into(), Value::from(item.attempts));
    fields.insert("outcome".into(), Value::String(item.outcome.name().into()));
    fields.insert(
        "reason".into(),
        receipt.failure.as_ref().map_or(Value::Null, |f| Value::String(cap_error(&format!("{}: {}", f.tag.name(), f.message)))),
    );
    fields.insert("run_id".into(), Value::String(receipt.run_id.clone()));
    fields.insert("execution_id".into(), Value::String(execution_id.into()));
    fields.insert("ended_at".into(), Value::String(at.to_rfc3339_nanos()));
    let batch = Batch { rows: vec![fields], types: COLUMNS.iter().map(|(c, t)| (c.to_string(), t.clone())).collect() };
    let mut nonce = [0u8; 4];
    getrandom::fill(&mut nonce).map_err(|e| ContextError::Invalid(format!("item record nonce: {e}")))?;
    let store_run = format!("item-{}-{}", at.unix_nanos(), crate::store::hex(&nonce));
    let injection = Injection { run_id: store_run, site_id: site_id.into(), batch_seq: None, authored_by: None, authored_agent: None, taint: None };
    land_engine_owned(store, &decl(), &batch, &RunContext { node: node.clone(), injection, committed_at: at }).map(Some)
}
