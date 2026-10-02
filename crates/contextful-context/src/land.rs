//! Landing a run: reconcile each batch's schema, inject the provenance columns, write the
//! run's parts, and commit by conditionally creating its manifest. A run lands its batches
//! at once, or stages them part by part and commits the staged parts.

use crate::error::{ContextError, IoPath, Result};
use crate::parquet_io;
use crate::store::{create_new_file, replace_file, FileLock, Store, LOCK_WAIT_SECS};
use arrow_array::builder::{
    BinaryBuilder, BooleanBuilder, FixedSizeBinaryBuilder, FixedSizeListBuilder, Float16Builder, Float32Builder, Float64Builder,
    Int32Builder, Int64Builder, StringBuilder, TimestampNanosecondBuilder,
};
use arrow_array::{ArrayRef, NullArray, RecordBatch};
use contextful_core::store::declare::TableDecl;
use contextful_core::store::lay_out::{is_path_segment, part_name, NodeId, PartEntry, RunManifest, MANIFEST_FILE};
use contextful_core::store::reconcile::{decode_binary, supertype, Column, ColumnType, FloatItem, Schema, VECTOR_ITEM};
use contextful_core::store::reserve::{
    optional_value_problem, producer_columns, Injection, ALWAYS_INJECTED, AUTHORED_BY, BATCH_SEQ, COMMIT_SEQ, INGESTED_AT, ROW_SEQ, TAINT,
    RUN_ID, SITE_ID,
};
use contextful_core::store::StoreError;
use contextful_core::time::Instant;
use serde_json::{Map, Value};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
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

/// The pipeline a run commits for and the position its rows reach, carried on the run
/// manifest so rows and position commit together (`run.advance.commit-with-rows`).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Position {
    pub pipeline_id: Option<String>,
    pub cursor: Option<Value>,
    /// The fence of the lease the commit runs under.
    pub fence: Option<u64>,
    /// The run commits through its node's commit log, which makes it readable.
    pub logged: bool,
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
    rows_schema(batch.rows.iter(), &batch.types)
}

/// The schema of `rows` read in order, each column typed by `types` or by the lattice
/// supertype of its values.
fn rows_schema<'a>(rows: impl Iterator<Item = &'a Map<String, Value>>, types: &HashMap<String, ColumnType>) -> Result<Schema> {
    let mut columns: Vec<Column> = Vec::new();
    for row in rows {
        for (name, v) in row {
            let seen = value_type(v);
            match columns.iter_mut().find(|c| &c.name == name) {
                Some(c) => {
                    if types.contains_key(name) {
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
                    let ty = types.get(name).copied().unwrap_or(seen);
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
        ColumnType::Binary => {
            let mut b = BinaryBuilder::new();
            for v in vals {
                b.append_option(v.map(|v| v.as_str().and_then(decode_binary).ok_or_else(|| bad(v))).transpose()?);
            }
            Arc::new(b.finish())
        }
        ColumnType::FixedSizeBinary(n) => {
            let mut b = FixedSizeBinaryBuilder::new(parquet_io::width(n));
            for v in vals {
                match v {
                    None => b.append_null(),
                    Some(v) => {
                        let bytes = v.as_str().and_then(decode_binary).filter(|x| x.len() == n as usize).ok_or_else(|| bad(v))?;
                        b.append_value(bytes).map_err(|_| bad(v))?;
                    }
                }
            }
            Arc::new(b.finish())
        }
        ColumnType::FixedSizeList(item, n) => {
            // Each element is a JSON number and the array holds exactly the dimension.
            let elements = |v: &Value| -> Result<Vec<f64>> {
                let a = v.as_array().filter(|a| a.len() == n as usize).ok_or_else(|| bad(v))?;
                a.iter().map(|x| x.as_f64().ok_or_else(|| bad(v))).collect()
            };
            let field = Arc::new(arrow_schema::Field::new(VECTOR_ITEM, parquet_io::data_type_of(item), false));
            match item {
                FloatItem::Float32 => {
                    let mut b = FixedSizeListBuilder::new(Float32Builder::new(), parquet_io::width(n)).with_field(field);
                    for v in vals {
                        match v {
                            None => {
                                (0..n).for_each(|_| b.values().append_value(0.0));
                                b.append(false);
                            }
                            Some(v) => {
                                elements(v)?.into_iter().for_each(|x| b.values().append_value(x as f32));
                                b.append(true);
                            }
                        }
                    }
                    Arc::new(b.finish())
                }
                FloatItem::Float16 => {
                    let mut b = FixedSizeListBuilder::new(Float16Builder::new(), parquet_io::width(n)).with_field(field);
                    for v in vals {
                        match v {
                            None => {
                                (0..n).for_each(|_| b.values().append_value(half::f16::ZERO));
                                b.append(false);
                            }
                            Some(v) => {
                                elements(v)?.into_iter().for_each(|x| b.values().append_value(half::f16::from_f64(x)));
                                b.append(true);
                            }
                        }
                    }
                    Arc::new(b.finish())
                }
            }
        }
    })
}

fn check_run_id(run_id: &str) -> Result<()> {
    if run_id.len() <= 128 && is_path_segment(run_id) {
        Ok(())
    } else {
        Err(ContextError::Invalid(format!("run id `{run_id}` is not 1 to 128 chars of [A-Za-z0-9._-]")))
    }
}

/// One record batch per non-empty batch of the run: the producer's `arriving` columns,
/// then the injected ones carrying `commit_seq` and the instant `at`.
fn run_parts(arriving: &Schema, batches: &[Batch], ctx: &RunContext, per_batch: bool, commit_seq: i64, at: Instant) -> Result<Vec<RecordBatch>> {
    let mut parts = Vec::new();
    let mut row_offset: i64 = 0;
    for (ordinal, b) in batches.iter().enumerate().filter(|(_, b)| !b.rows.is_empty()) {
        let mut injection = ctx.injection.clone();
        if per_batch {
            injection.batch_seq = Some(batch_seq(ordinal)?);
        }
        parts.push(part_batch(arriving, &b.rows, &injection, row_offset, Some((at, commit_seq)))?);
        row_offset += b.rows.len() as i64;
    }
    Ok(parts)
}

/// The answer to a landing of a run its node already committed: the committed manifest
/// when the run is unlogged and the landing carries its position and rebuilds its parts
/// column for column (`store.lay-out.run-replay`), `StoreRunConflict` otherwise
/// (`store.lay-out.run-conflict`).
fn replay(
    manifest_path: &std::path::Path,
    types: &HashMap<String, ColumnType>,
    batches: &[Batch],
    ctx: &RunContext,
    position: &Position,
    per_batch: bool,
) -> Result<Landing> {
    let run_id = ctx.injection.run_id.as_str();
    let conflict = |why: &str| -> ContextError {
        StoreError::StoreRunConflict(format!("run `{run_id}` is already committed on node `{}` {why}", ctx.node)).into()
    };
    let bytes = std::fs::read(manifest_path).at(manifest_path)?;
    let committed: RunManifest = serde_json::from_slice(&bytes)
        .map_err(|e| StoreError::StoreManifestUnreadable(format!("{}: {e}", manifest_path.display())))?;
    if committed.logged {
        return Err(conflict("as a logged run, which commits through its commit-log entry"));
    }
    let held = Position { pipeline_id: committed.pipeline_id.clone(), cursor: committed.cursor.clone(), fence: committed.fence, logged: committed.logged };
    if &held != position {
        return Err(conflict("at another position"));
    }
    let other_rows = || conflict("with other rows");
    let arriving = rows_schema(batches.iter().flat_map(|b| b.rows.iter()), types)
        .and_then(|s| Ok(producer_columns(&s)?))
        .map_err(|_| other_rows())?;
    let rebuilt = run_parts(&arriving, batches, ctx, per_batch, committed.commit_seq.unwrap_or_default(), committed.committed_at)
        .map_err(|_| other_rows())?;
    if rebuilt.len() != committed.parts.len() {
        return Err(other_rows());
    }
    let dir = manifest_path.parent().expect("a manifest sits in its node directory");
    for (part, entry) in rebuilt.iter().zip(&committed.parts) {
        if !same_columns(part, &parquet_io::read(&dir.join(&entry.name))?) {
            return Err(other_rows());
        }
    }
    Ok(Landing { manifest: committed, replay: true })
}

/// `part` and the batches `stored` holds carry the same named columns, each with the same
/// type and values, field metadata and column order aside.
fn same_columns(part: &RecordBatch, stored: &[RecordBatch]) -> bool {
    let Some(first) = stored.first() else { return part.num_rows() == 0 };
    let Ok(stored) = arrow_select::concat::concat_batches(&first.schema(), stored) else { return false };
    part.num_rows() == stored.num_rows()
        && part.num_columns() == stored.num_columns()
        && part.schema().fields().iter().zip(part.columns()).all(|(f, a)| {
            stored.column_by_name(f.name()).is_some_and(|b| a.data_type() == b.data_type() && a.to_data() == b.to_data())
        })
}

/// Land `batch` into `decl`'s table as run `ctx.injection.run_id`. Every refusal — a
/// reserved name, an incompatible or widened type, an unknown ordering column —
/// fires before any Parquet is written; the run is visible once its manifest exists.
pub fn land(store: &Store, decl: &TableDecl, batch: &Batch, ctx: &RunContext) -> Result<RunManifest> {
    land_batches(store, decl, std::slice::from_ref(batch), ctx, &Position::default(), &|| Ok(()))
}

/// Land `batches` as one run commit: a part per non-empty batch in order, each row's
/// `_batch_seq` its batch's ordinal when the run carries one per batch, `_row_seq`
/// numbering the run's rows across parts, and the manifest carrying `position`.
/// `precommit` runs immediately before the manifest is created; a refusal there leaves
/// the run uncommitted, joining no file list.
pub fn land_batches(
    store: &Store,
    decl: &TableDecl,
    batches: &[Batch],
    ctx: &RunContext,
    position: &Position,
    precommit: &dyn Fn() -> Result<()>,
) -> Result<RunManifest> {
    land_run(store, decl, batches, ctx, position, precommit).map(|l| l.manifest)
}

/// [`land_batches`], answering whether the landing replayed a run already committed
/// (`store.lay-out.run-replay`).
pub fn land_run(
    store: &Store,
    decl: &TableDecl,
    batches: &[Batch],
    ctx: &RunContext,
    position: &Position,
    precommit: &dyn Fn() -> Result<()>,
) -> Result<Landing> {
    commit_run(store, decl, batches, ctx, position, precommit, &|_| Ok(()))
}

/// A run's landing: the committed manifest, and whether this landing replayed it
/// rather than committing it.
#[derive(Debug, Clone, PartialEq)]
pub struct Landing {
    pub manifest: RunManifest,
    pub replay: bool,
}

/// [`land_batches`], with `commit_point` run once the manifest exists: the step that makes
/// a logged run readable. `precommit` and `commit_point` both run under the table's commit
/// lock, taken before `_commit_seq` is assigned (`store.reserve.commit-order`).
pub fn commit_batches(
    store: &Store,
    decl: &TableDecl,
    batches: &[Batch],
    ctx: &RunContext,
    position: &Position,
    precommit: &dyn Fn() -> Result<()>,
    commit_point: &dyn Fn(&RunManifest) -> Result<()>,
) -> Result<RunManifest> {
    commit_run(store, decl, batches, ctx, position, precommit, commit_point).map(|l| l.manifest)
}

fn commit_run(
    store: &Store,
    decl: &TableDecl,
    batches: &[Batch],
    ctx: &RunContext,
    position: &Position,
    precommit: &dyn Fn() -> Result<()>,
    commit_point: &dyn Fn(&RunManifest) -> Result<()>,
) -> Result<Landing> {
    store.check_writable("land")?;
    let per_batch = batches.len() > 1 || position.pipeline_id.is_some();
    let (node_dir, manifest_path) = run_dir(store, &decl.name, &ctx.node, &ctx.injection.run_id)?;
    let types = column_types(store, decl, batches)?;
    if manifest_path.exists() {
        return replay(&manifest_path, &types, batches, ctx, position, per_batch);
    }
    let site = reconcile(store, decl, batches, &ctx.node, &ctx.injection, per_batch, None, true)?;

    // From here to the commit point the table's commits serialize, the Parquet write
    // included, so the readable runs always hold a prefix of its commit sequence; a
    // landing on the table waits for another's write, and refuses past LOCK_WAIT_SECS
    // (`store.reserve.commit-order`).
    let _commit_lock = store.lock_commit(&decl.name)?;
    let _run_lock = take_run_lock(&node_dir)?;
    if manifest_path.exists() {
        return replay(&manifest_path, &types, batches, ctx, position, per_batch);
    }
    let commit_seq = store.assign_commit_seq(&decl.name)?;

    let mut parts = Vec::new();
    for (ordinal, rb) in run_parts(&site.arriving, batches, ctx, per_batch, commit_seq, ctx.committed_at)?.into_iter().enumerate() {
        let name = part_name(part_ordinal(ordinal)?);
        write_batch(&site.node_dir.join(&name), &rb)?;
        parts.push(PartEntry { name, key_version: 0 });
    }
    match create_manifest(&site, decl, ctx, parts, position, commit_seq, precommit, commit_point)? {
        Some(manifest) => Ok(Landing { manifest, replay: false }),
        None => replay(&manifest_path, &types, batches, ctx, position, per_batch),
    }
}

/// The directory under a run's node directory holding its staged parts and staged schema.
/// Its `.staging` suffix keeps it off every sync push, and no manifest names a file in it.
const STAGE_DIR: &str = "stage.staging";

/// The run's staged schema inside [`STAGE_DIR`]: the producer columns its stages merged
/// (`run.own.stage-schema`).
const STAGED_SCHEMA_FILE: &str = "schema.json";

/// Where one run's parts live on its node, the producer columns its batches
/// arrive with after reconciliation, and those merged over the run's earlier stages.
struct RunSite {
    node_dir: PathBuf,
    arriving: Schema,
    run_columns: Schema,
}

/// The node directory of run `run_id` in `table`, and its manifest path.
fn run_dir(store: &Store, table: &str, node: &NodeId, run_id: &str) -> Result<(PathBuf, PathBuf)> {
    check_run_id(run_id)?;
    contextful_core::store::reserve::check_table_name(table)?;
    let node_dir = store.table_dir(table)?.join("data").join("runs").join(run_id).join(node.as_str());
    let manifest_path = node_dir.join(MANIFEST_FILE);
    Ok((node_dir, manifest_path))
}

fn already_committed(node: &NodeId, run_id: &str) -> ContextError {
    ContextError::Invalid(format!("run `{run_id}` is already committed on node `{node}`"))
}

/// The producer columns the run's earlier stages merged, if any stage wrote them.
fn read_staged_schema(stage_dir: &Path) -> Result<Option<Schema>> {
    let path = stage_dir.join(STAGED_SCHEMA_FILE);
    match std::fs::read(&path) {
        Ok(bytes) => serde_json::from_slice(&bytes).map(Some).map_err(|e| ContextError::Invalid(format!("{}: {e}", path.display()))),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(ContextError::Io { path, source: e }),
    }
}

/// A column's type comes from the producer, then the declaration
/// (`store.declare.column-types`), then a binary or vector type `schema.json` already
/// holds (`store.reconcile.stored-type`); a JSON value alone carries none of these.
fn column_types(store: &Store, decl: &TableDecl, batches: &[Batch]) -> Result<HashMap<String, ColumnType>> {
    let mut types: HashMap<String, ColumnType> = HashMap::new();
    if let Some(stored) = store.try_schema(&decl.name)? {
        types.extend(stored.columns.iter().filter(|c| c.ty.is_binary() || c.ty.is_vector()).map(|c| (c.name.clone(), c.ty)));
    }
    types.extend(decl.column_types());
    for b in batches {
        types.extend(b.types.iter().map(|(k, v)| (k.clone(), *v)));
    }
    Ok(types)
}

/// Every refusal of `batches` against `decl`, the stored schema and the columns the run's
/// `earlier` stages merged; then, when `write`, the merged schema written to `schema.json`.
/// No Parquet is written here.
#[allow(clippy::too_many_arguments)]
fn reconcile(
    store: &Store,
    decl: &TableDecl,
    batches: &[Batch],
    node: &NodeId,
    injection: &Injection,
    per_batch: bool,
    earlier: Option<&Schema>,
    write: bool,
) -> Result<RunSite> {
    store.check_writable("land")?;
    let all_rows = || batches.iter().flat_map(|b| b.rows.iter());
    let table = decl.name.as_str();
    let (node_dir, _) = run_dir(store, table, node, &injection.run_id)?;
    let types = column_types(store, decl, batches)?;

    // Reconcile: the producer's columns, held to the namespace, merged into the stored shape.
    let arriving = producer_columns(&rows_schema(all_rows(), &types)?)?;
    // An optional column's value is held to its vocabulary whatever JSON type it arrives
    // as: a number reads as its text, so `{"_modality": 7}` refuses like `"7"` would.
    for c in arriving.columns.iter().filter(|c| c.name.starts_with('_')) {
        for row in all_rows() {
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
    // The schema lock spans the read-merge-replace of `schema.json` alone.
    let schema_lock = store.lock_schema(table)?;
    let stored = store.try_schema(table)?.unwrap_or_default();
    let mut schema_injection = injection.clone();
    if per_batch {
        schema_injection.batch_seq = Some(0);
    }
    let injected: Vec<Column> = schema_injection
        .columns()
        .into_iter()
        .map(|mut c| {
            // Paths without a batch scope or a subject omit the others, so their column is nullable.
            c.nullable = !ALWAYS_INJECTED.contains(&c.name.as_str());
            c
        })
        .collect();
    let run_columns = match earlier {
        Some(e) => e.merge(&arriving, decl.primary_key())?,
        None => arriving.clone(),
    };
    let mut merged = stored
        .merge(&run_columns, decl.primary_key())?
        .merge(&Schema { columns: injected }, decl.primary_key())?;
    // A column joining a non-empty schema merges as nullable; these are in every file.
    for c in merged.columns.iter_mut().filter(|c| ALWAYS_INJECTED.contains(&c.name.as_str())) {
        c.nullable = false;
    }
    decl.validate(&merged)?;
    decl.validate_index_types(&merged)?;
    // Every refusal of the batch has fired. The schema commits before the manifest, so
    // a fold reading a run finds its columns in the schema it reads after.
    if write {
        store.write_schema(table, &merged)?;
    }
    drop(schema_lock);
    Ok(RunSite { node_dir, arriving, run_columns })
}

/// The run's manifest lock on its node. A part carries the name the manifest names, so two
/// landings of one run on one node serialize from the part to the manifest: without the
/// lock the loser rewrites the part the winner's manifest already describes, and the run
/// reads rows no manifest accounts for.
fn take_run_lock(node_dir: &Path) -> Result<FileLock> {
    std::fs::create_dir_all(node_dir).at(node_dir)?;
    FileLock::acquire(&node_dir.join(format!("{MANIFEST_FILE}.lock")), std::time::Duration::from_secs(LOCK_WAIT_SECS))
}

/// [`take_run_lock`], refusing once the run's manifest stands.
fn lock_run(node_dir: &Path, manifest_path: &Path, node: &NodeId, run_id: &str) -> Result<FileLock> {
    let lock = take_run_lock(node_dir)?;
    if manifest_path.exists() {
        return Err(already_committed(node, run_id));
    }
    Ok(lock)
}

fn part_ordinal(i: usize) -> Result<u32> {
    u32::try_from(i).map_err(|_| ContextError::Invalid("a run holds more parts than a part name numbers".into()))
}

fn batch_seq(ordinal: usize) -> Result<i32> {
    i32::try_from(ordinal).map_err(|_| ContextError::Invalid(format!("batch ordinal {ordinal} exceeds the `_batch_seq` range")))
}

/// One part as a record batch: the producer's columns in their `arriving` types, then the
/// injected ones, `_row_seq` counting on from `row_offset`. `stamp` is the commit instant
/// and `_commit_seq`; a staged part, written before its commit, omits both columns, which
/// its commit adds.
fn part_batch(arriving: &Schema, rows: &[Map<String, Value>], injection: &Injection, row_offset: i64, stamp: Option<(Instant, i64)>) -> Result<RecordBatch> {
    let n = rows.len();
    let stamp = stamp.map(|(at, seq)| nanos(at).map(|at| (at, seq))).transpose()?;
    let run_id = injection.run_id.as_str();
    let mut cols: Vec<Column> = arriving.columns.clone();
    let mut arrays: Vec<ArrayRef> = arriving.columns.iter().map(|c| column_array(c, rows)).collect::<Result<_>>()?;
    for c in injection.columns() {
        let array: ArrayRef = match (c.name.as_str(), stamp) {
            (COMMIT_SEQ | INGESTED_AT, None) => continue,
            (COMMIT_SEQ, Some((_, seq))) => Arc::new(arrow_array::Int64Array::from(vec![seq; n])),
            (INGESTED_AT, Some((at, _))) => Arc::new(arrow_array::TimestampNanosecondArray::from(vec![at; n]).with_timezone("UTC")),
            (RUN_ID, _) => Arc::new(arrow_array::StringArray::from(vec![run_id; n])),
            (ROW_SEQ, _) => Arc::new(arrow_array::Int64Array::from_iter_values(row_offset..row_offset + n as i64)),
            (BATCH_SEQ, _) => Arc::new(arrow_array::Int32Array::from(vec![injection.batch_seq.unwrap_or_default(); n])),
            (SITE_ID, _) => Arc::new(arrow_array::StringArray::from(vec![injection.site_id.as_str(); n])),
            (AUTHORED_BY, _) => Arc::new(arrow_array::StringArray::from(vec![injection.authored_by.as_deref().unwrap_or_default(); n])),
            (TAINT, _) => Arc::new(arrow_array::StringArray::from(vec![injection.taint.map(|p| p.as_str()).unwrap_or_default(); n])),
            (other, _) => unreachable!("no injected column `{other}`"),
        };
        cols.push(c);
        arrays.push(array);
    }
    RecordBatch::try_new(parquet_io::arrow_schema(&Schema { columns: cols }), arrays).map_err(|e| ContextError::Invalid(e.to_string()))
}

/// Write `rb` as the Parquet file `path`, replacing a file a lost landing left there.
fn write_batch(path: &Path, rb: &RecordBatch) -> Result<()> {
    if path.exists() {
        std::fs::remove_file(path).at(path)?;
    }
    parquet_io::write(path, rb)
}

/// An instant as the nanoseconds `_ingested_at` stores.
fn nanos(at: Instant) -> Result<i64> {
    i64::try_from(at.unix_nanos()).map_err(|_| ContextError::Invalid(format!("{at} is outside the nanosecond timestamp range")))
}

/// Create the run's manifest naming `parts` under `commit_seq`, only if absent, after
/// `precommit`; then run `commit_point`. `None` when a manifest stood already.
#[allow(clippy::too_many_arguments)]
fn create_manifest(
    site: &RunSite,
    decl: &TableDecl,
    ctx: &RunContext,
    parts: Vec<PartEntry>,
    position: &Position,
    commit_seq: i64,
    precommit: &dyn Fn() -> Result<()>,
    commit_point: &dyn Fn(&RunManifest) -> Result<()>,
) -> Result<Option<RunManifest>> {
    let manifest = RunManifest {
        run_id: ctx.injection.run_id.clone(),
        table: decl.name.clone(),
        node_id: ctx.node.to_string(),
        parts,
        committed_at: ctx.committed_at,
        pipeline_id: position.pipeline_id.clone(),
        cursor: position.cursor.clone(),
        fence: position.fence,
        logged: position.logged,
        commit_seq: Some(commit_seq),
    };
    std::fs::create_dir_all(&site.node_dir).at(&site.node_dir)?;
    let bytes = serde_json::to_vec_pretty(&manifest).expect("a manifest serializes");
    precommit()?;
    if !create_new_file(&site.node_dir.join(MANIFEST_FILE), &bytes)? {
        return Ok(None);
    }
    commit_point(&manifest)?;
    Ok(Some(manifest))
}

/// One part a run staged: its file name inside the run's staging directory, and what it holds.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StagedPart {
    pub name: String,
    pub rows: u64,
    pub bytes: u64,
}

/// The name of a run's staged part `ordinal`.
fn stage_name(ordinal: u32) -> String {
    format!("stage-{ordinal:05}.parquet")
}

/// Stage `batch` as part `ordinal` of run `injection.run_id` on `node`
/// (`run.own.backpressure`): the batch's refusals fire against `schema.json` and the run's
/// earlier stages, and its columns merge into the run's staged schema, leaving
/// `schema.json` as it stood (`run.own.stage-schema`). Its Parquet lands in the run's
/// staging directory without `_commit_seq` or `_ingested_at` (`run.own.stage-commit-seq`,
/// `run.own.stage-instant`), joining no file list until [`commit_parts`] names it. Every
/// row carries `_batch_seq` `ordinal` and `_row_seq` counting on from `row_offset`.
pub fn stage_part(store: &Store, decl: &TableDecl, batch: &Batch, node: &NodeId, injection: &Injection, ordinal: u32, row_offset: u64) -> Result<StagedPart> {
    if batch.rows.is_empty() {
        return Err(ContextError::Invalid(format!("run `{}` stages an empty batch as part {ordinal}", injection.run_id)));
    }
    let (node_dir, manifest_path) = run_dir(store, &decl.name, node, &injection.run_id)?;
    let stage_dir = node_dir.join(STAGE_DIR);
    let _run_lock = lock_run(&node_dir, &manifest_path, node, &injection.run_id)?;
    let earlier = read_staged_schema(&stage_dir)?;
    let landing = reconcile(store, decl, std::slice::from_ref(batch), node, injection, true, earlier.as_ref(), false)?;
    // The staged schema lands before the part, so every part a commit names has its
    // columns in the schema that commit merges.
    std::fs::create_dir_all(&stage_dir).at(&stage_dir)?;
    let text = serde_json::to_vec_pretty(&landing.run_columns).expect("a schema serializes");
    replace_file(&stage_dir.join(STAGED_SCHEMA_FILE), &text)?;
    let mut injection = injection.clone();
    injection.batch_seq = Some(batch_seq(ordinal as usize)?);
    let offset = i64::try_from(row_offset).map_err(|_| ContextError::Invalid(format!("row offset {row_offset} exceeds the `_row_seq` range")))?;
    let name = stage_name(ordinal);
    let path = stage_dir.join(&name);
    write_batch(&path, &part_batch(&landing.arriving, &batch.rows, &injection, offset, None)?)?;
    let bytes = std::fs::metadata(&path).at(&path)?.len();
    Ok(StagedPart { name, rows: batch.rows.len() as u64, bytes })
}

/// Commit the staged `parts` of run `ctx.injection.run_id` as one run commit carrying
/// `position`, committed at `ctx.committed_at`. A part no stage wrote refuses first. Under
/// the table's commit lock, the run's staged columns merge into `schema.json`, refusing as
/// a landing does against the schema as it stands then; `_commit_seq` is assigned, and it
/// and `ctx.committed_at` as `_ingested_at` are written into each staged part as it
/// becomes the run's part `i`, one record batch in memory at a time
/// (`run.own.stage-commit-seq`, `run.own.stage-instant`). `precommit` runs immediately
/// before the manifest is created and `commit_point` once it exists, as in
/// [`commit_batches`]. A commit naming no part still merges the injected columns, so the
/// table and its marker exist. Success or failure, the run's staging directory is gone on
/// return, and a commit that fails before its manifest exists leaves none of the parts it
/// wrote (`run.own.stage-discard`).
pub fn commit_parts(
    store: &Store,
    decl: &TableDecl,
    parts: &[String],
    ctx: &RunContext,
    position: &Position,
    precommit: &dyn Fn() -> Result<()>,
    commit_point: &dyn Fn(&RunManifest) -> Result<()>,
) -> Result<RunManifest> {
    store.check_writable("land")?;
    let (node_dir, manifest_path) = run_dir(store, &decl.name, &ctx.node, &ctx.injection.run_id)?;
    let stage_dir = node_dir.join(STAGE_DIR);
    let _commit_lock = store.lock_commit(&decl.name)?;
    let _run_lock = lock_run(&node_dir, &manifest_path, &ctx.node, &ctx.injection.run_id)?;
    let mut written: Vec<PathBuf> = Vec::new();
    let committed = (|| {
        for name in parts {
            if !is_path_segment(name) || !stage_dir.join(name).is_file() {
                return Err(ContextError::Invalid(format!("run `{}` commits part `{name}`, which no stage wrote on node `{}`", ctx.injection.run_id, ctx.node)));
            }
        }
        let earlier = read_staged_schema(&stage_dir)?;
        let landing = reconcile(store, decl, &[], &ctx.node, &ctx.injection, true, earlier.as_ref(), true)?;
        let commit_seq = store.assign_commit_seq(&decl.name)?;
        let inserts = [
            parquet_io::Insert {
                field: parquet_io::field(&Column::new(INGESTED_AT, ColumnType::Timestamp, false)),
                at: parquet_io::At::Before(RUN_ID),
                fill: parquet_io::Fill::Timestamp(nanos(ctx.committed_at)?),
            },
            parquet_io::Insert { field: parquet_io::field(&Column::new(COMMIT_SEQ, ColumnType::Int64, false)), at: parquet_io::At::After(ROW_SEQ), fill: parquet_io::Fill::Int64(commit_seq) },
        ];
        let mut entries = Vec::with_capacity(parts.len());
        for (i, staged) in parts.iter().enumerate() {
            let name = part_name(part_ordinal(i)?);
            let to = landing.node_dir.join(&name);
            written.push(to.clone());
            parquet_io::copy_inserting(&stage_dir.join(staged), &to, &inserts)?;
            entries.push(PartEntry { name, key_version: 0 });
        }
        create_manifest(&landing, decl, ctx, entries, position, commit_seq, precommit, commit_point)?.ok_or_else(|| already_committed(&ctx.node, &ctx.injection.run_id))
    })();
    if committed.is_err() && !manifest_path.exists() {
        for path in &written {
            let _ = std::fs::remove_file(path);
        }
    }
    // A committed run reads its own parts and a failed one never commits under its id, so
    // the staged parts serve neither.
    let _ = std::fs::remove_dir_all(&stage_dir);
    committed
}

/// Remove every part run `run_id` staged on `node` in `table` (`run.own.stage-discard`). A
/// run with nothing staged discards nothing.
pub fn discard_staged(store: &Store, table: &str, node: &NodeId, run_id: &str) -> Result<()> {
    let (node_dir, _) = run_dir(store, table, node, run_id)?;
    let stage_dir = node_dir.join(STAGE_DIR);
    match std::fs::remove_dir_all(&stage_dir) {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(ContextError::Io { path: stage_dir, source: e }),
        _ => Ok(()),
    }
}
