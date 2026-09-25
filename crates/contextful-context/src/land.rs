//! Landing one batch as one run: reconcile its schema, inject the provenance columns,
//! write the run's part, and commit by conditionally creating its manifest.

use crate::error::{ContextError, IoPath, Result};
use crate::parquet_io;
use crate::store::{create_new_file, FileLock, Store, LOCK_WAIT_SECS};
use arrow_array::builder::{BooleanBuilder, Float64Builder, Int32Builder, Int64Builder, StringBuilder, TimestampNanosecondBuilder};
use arrow_array::{ArrayRef, NullArray, RecordBatch};
use contextful_core::store::declare::TableDecl;
use contextful_core::store::lay_out::{part_name, NodeId, PartEntry, RunManifest, MANIFEST_FILE};
use contextful_core::store::reconcile::{supertype, Column, ColumnType, Schema};
use contextful_core::store::reserve::{
    optional_value_problem, producer_columns, Injection, ALWAYS_INJECTED, AUTHORED_BY, BATCH_SEQ, INGESTED_AT, RUN_ID,
    SITE_ID,
};
use contextful_core::store::StoreError;
use contextful_core::time::Instant;
use serde_json::{Map, Value};
use std::collections::HashMap;
use std::sync::Arc;

/// One batch to land as one run.
#[derive(Debug, Clone)]
pub struct Batch {
    pub rows: Vec<Map<String, Value>>,
    /// Column types fixed by the producer rather than read off the values.
    pub types: HashMap<String, ColumnType>,
}

/// What the landing run is, and where it commits.
#[derive(Debug, Clone)]
pub struct RunContext {
    pub node: NodeId,
    pub injection: Injection,
    pub committed_at: Instant,
}

/// The type a JSON value carries on its own.
fn value_type(v: &Value) -> ColumnType {
    match v {
        Value::Null => ColumnType::Null,
        Value::Bool(_) => ColumnType::Boolean,
        Value::Number(n) if n.is_i64() => ColumnType::Int64,
        Value::Number(_) => ColumnType::Float64,
        Value::String(_) => ColumnType::Utf8,
        Value::Array(_) | Value::Object(_) => ColumnType::Json,
    }
}

/// The batch's own schema: columns in order of first appearance, each typed by the
/// producer or by the lattice supertype of its values. Producer columns are nullable.
pub fn batch_schema(batch: &Batch) -> Result<Schema> {
    let mut columns: Vec<Column> = Vec::new();
    for row in &batch.rows {
        for (name, v) in row {
            let seen = value_type(v);
            match columns.iter_mut().find(|c| &c.name == name) {
                Some(c) => {
                    if batch.types.contains_key(name) {
                        continue;
                    }
                    c.ty = supertype(c.ty, seen).ok_or_else(|| {
                        StoreError::StoreSchemaIncompatible(format!(
                            "column `{name}` arrives as {} and as {} in one batch",
                            c.ty.name(),
                            seen.name()
                        ))
                    })?;
                }
                None => {
                    let ty = batch.types.get(name).copied().unwrap_or(seen);
                    columns.push(Column::new(name.clone(), ty, true));
                }
            }
        }
    }
    Ok(Schema { columns })
}

fn column_array(c: &Column, rows: &[Map<String, Value>]) -> Result<ArrayRef> {
    let bad = |v: &Value| {
        ContextError::Store(StoreError::StoreSchemaIncompatible(format!(
            "column `{}` is {} and a value arrives as {v}",
            c.name,
            c.ty.name()
        )))
    };
    let vals = rows.iter().map(|r| r.get(&c.name).filter(|v| !v.is_null()));
    Ok(match c.ty {
        ColumnType::Null => Arc::new(NullArray::new(rows.len())),
        ColumnType::Boolean => {
            let mut b = BooleanBuilder::new();
            for v in vals {
                b.append_option(v.map(|v| v.as_bool().ok_or_else(|| bad(v))).transpose()?);
            }
            Arc::new(b.finish())
        }
        ColumnType::Int32 => {
            let mut b = Int32Builder::new();
            for v in vals {
                b.append_option(v.map(|v| v.as_i64().and_then(|n| i32::try_from(n).ok()).ok_or_else(|| bad(v))).transpose()?);
            }
            Arc::new(b.finish())
        }
        ColumnType::Int64 => {
            let mut b = Int64Builder::new();
            for v in vals {
                b.append_option(v.map(|v| v.as_i64().ok_or_else(|| bad(v))).transpose()?);
            }
            Arc::new(b.finish())
        }
        ColumnType::Float64 => {
            let mut b = Float64Builder::new();
            for v in vals {
                b.append_option(v.map(|v| v.as_f64().ok_or_else(|| bad(v))).transpose()?);
            }
            Arc::new(b.finish())
        }
        ColumnType::Utf8 => {
            let mut b = StringBuilder::new();
            for v in vals {
                b.append_option(v.map(|v| v.as_str().ok_or_else(|| bad(v))).transpose()?);
            }
            Arc::new(b.finish())
        }
        ColumnType::Json => {
            let mut b = StringBuilder::new();
            for v in vals {
                // Every value is its JSON encoding, a string included, so each is a JSON document.
                b.append_option(v.map(Value::to_string));
            }
            Arc::new(b.finish())
        }
        ColumnType::Timestamp => {
            let mut b = TimestampNanosecondBuilder::new().with_timezone("UTC");
            for v in vals {
                let n = v
                    .map(|v| {
                        let t = v.as_str().and_then(|s| Instant::parse(s).ok()).ok_or_else(|| bad(v))?;
                        i64::try_from(t.unix_nanos()).map_err(|_| bad(v))
                    })
                    .transpose()?;
                b.append_option(n);
            }
            Arc::new(b.finish())
        }
    })
}

fn check_run_id(run_id: &str) -> Result<()> {
    let ok = !run_id.is_empty()
        && run_id.len() <= 128
        && run_id != "."
        && run_id != ".."
        && run_id.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'));
    if ok {
        Ok(())
    } else {
        Err(ContextError::Invalid(format!("run id `{run_id}` is not 1 to 128 chars of [A-Za-z0-9._-]")))
    }
}

/// Land `batch` into `decl`'s table as run `ctx.injection.run_id`. Every refusal — a
/// reserved name, an incompatible or widened type, an unknown ordering column —
/// fires before any Parquet is written; the run is visible once its manifest exists.
pub fn land(store: &Store, decl: &TableDecl, batch: &Batch, ctx: &RunContext) -> Result<RunManifest> {
    let table = decl.name.as_str();
    let run_id = ctx.injection.run_id.as_str();
    check_run_id(run_id)?;
    contextful_core::store::reserve::check_table_name(table)?;
    let node_dir = store.table_dir(table)?.join("data").join("runs").join(run_id).join(ctx.node.as_str());
    let manifest_path = node_dir.join(MANIFEST_FILE);
    if manifest_path.exists() {
        return Err(ContextError::Invalid(format!("run `{run_id}` is already committed on node `{}`", ctx.node)));
    }

    // Reconcile: the producer's columns, held to the namespace, merged into the stored shape.
    let arriving = producer_columns(&batch_schema(batch)?)?;
    // An optional column's value is held to its vocabulary whatever JSON type it arrives
    // as: a number reads as its text, so `{"_modality": 7}` refuses like `"7"` would.
    for c in arriving.columns.iter().filter(|c| c.name.starts_with('_')) {
        for row in &batch.rows {
            let text = match row.get(&c.name) {
                None | Some(Value::Null) => continue,
                Some(Value::String(v)) => v.clone(),
                Some(v) => v.to_string(),
            };
            if let Some(problem) = optional_value_problem(&c.name, &text) {
                return Err(ContextError::Invalid(format!("batch invalid: {problem}")));
            }
        }
    }
    // The schema lock spans the read-merge-replace of `schema.json` alone; the Parquet
    // write runs outside it, so a large batch holds up no other landing on the table.
    let schema_lock = store.lock_schema(table)?;
    let stored = store.try_schema(table)?.unwrap_or_default();
    let injected: Vec<Column> = ctx
        .injection
        .columns()
        .into_iter()
        .map(|mut c| {
            // Paths without a batch scope or a subject omit the others, so their column is nullable.
            c.nullable = !ALWAYS_INJECTED.contains(&c.name.as_str());
            c
        })
        .collect();
    let mut merged = stored
        .merge(&arriving, decl.primary_key())?
        .merge(&Schema { columns: injected }, decl.primary_key())?;
    // A column joining a non-empty schema merges as nullable; these are in every file.
    for c in merged.columns.iter_mut().filter(|c| ALWAYS_INJECTED.contains(&c.name.as_str())) {
        c.nullable = false;
    }
    decl.validate(&merged)?;
    // Every refusal of the batch has fired. The schema commits before the manifest, so
    // a fold reading a run finds its columns in the schema it reads after.
    store.write_schema(table, &merged)?;
    drop(schema_lock);

    // The part carries the name the manifest names, so two landings of one run on one
    // node serialize from here to the manifest: without the lock the loser rewrites the part the winner's manifest
    // already describes, and the run reads rows no manifest accounts for.
    std::fs::create_dir_all(&node_dir).at(&node_dir)?;
    let _run_lock =
        FileLock::acquire(&node_dir.join(format!("{MANIFEST_FILE}.lock")), std::time::Duration::from_secs(LOCK_WAIT_SECS))?;
    if manifest_path.exists() {
        return Err(ContextError::Invalid(format!("run `{run_id}` is already committed on node `{}`", ctx.node)));
    }

    // Build the part: the producer's columns in their arriving types, then the injected ones.
    let mut parts = Vec::new();
    if !batch.rows.is_empty() {
        let n = batch.rows.len();
        let mut cols: Vec<Column> = arriving.columns.clone();
        let mut arrays: Vec<ArrayRef> = arriving.columns.iter().map(|c| column_array(c, &batch.rows)).collect::<Result<_>>()?;
        let at = i64::try_from(ctx.committed_at.unix_nanos())
            .map_err(|_| ContextError::Invalid(format!("{} is outside the nanosecond timestamp range", ctx.committed_at)))?;
        for c in ctx.injection.columns() {
            let array: ArrayRef = match c.name.as_str() {
                INGESTED_AT => Arc::new(arrow_array::TimestampNanosecondArray::from(vec![at; n]).with_timezone("UTC")),
                RUN_ID => Arc::new(arrow_array::StringArray::from(vec![run_id; n])),
                BATCH_SEQ => Arc::new(arrow_array::Int32Array::from(vec![ctx.injection.batch_seq.unwrap_or_default(); n])),
                SITE_ID => Arc::new(arrow_array::StringArray::from(vec![ctx.injection.site_id.as_str(); n])),
                AUTHORED_BY => Arc::new(arrow_array::StringArray::from(vec![ctx.injection.authored_by.as_deref().unwrap_or_default(); n])),
                other => unreachable!("no injected column `{other}`"),
            };
            cols.push(c);
            arrays.push(array);
        }
        let rb = RecordBatch::try_new(parquet_io::arrow_schema(&Schema { columns: cols }), arrays)
            .map_err(|e| ContextError::Invalid(e.to_string()))?;
        let name = part_name(0);
        let path = node_dir.join(&name);
        if path.exists() {
            std::fs::remove_file(&path).at(&path)?;
        }
        parquet_io::write(&path, &rb)?;
        parts.push(PartEntry { name, key_version: 0 });
    }

    // Commit: the manifest, created only if absent.
    let manifest = RunManifest {
        run_id: run_id.to_string(),
        table: table.to_string(),
        node_id: ctx.node.to_string(),
        parts,
        committed_at: ctx.committed_at,
        pipeline_id: None,
        cursor: None,
        fence: None,
    };
    std::fs::create_dir_all(&node_dir).at(&node_dir)?;
    let bytes = serde_json::to_vec_pretty(&manifest).expect("a manifest serializes");
    if !create_new_file(&manifest_path, &bytes)? {
        return Err(ContextError::Invalid(format!("run `{run_id}` is already committed on node `{}`", ctx.node)));
    }
    Ok(manifest)
}
