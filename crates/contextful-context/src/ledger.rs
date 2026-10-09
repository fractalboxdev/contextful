//! A table's request ledger on the filesystem: one Parquet file per run and writing node
//! at `requests/<run-id>.<node-id>.parquet` (`store.reserve.ledger-path`), beside `data/`
//! so no run-directory sweep reaches it.
//!
//! Parquet has no in-place append, so a flush reads the file's rows, adds the new ones
//! and replaces the file through one rename, all under a lock on the file: a reader meets
//! the previous complete file or the new one, concurrent flushes of one run serialize, and
//! once [`append`] returns its rows and the rename are synced (`store.reserve.ledger-append`).

use crate::error::{ContextError, IoPath, Result};
use crate::parquet_io;
use crate::store::{FileLock, Store};
use crate::vector::Sealing;
use arrow_array::cast::AsArray;
use arrow_array::types::{Int32Type, Int64Type, TimestampNanosecondType};
use arrow_array::{Array, ArrayRef, Int32Array, Int64Array, RecordBatch, StringArray, TimestampNanosecondArray};
use contextful_core::store::lay_out::{is_path_segment, NodeId};
use contextful_core::store::ledger::{ledger_columns, RequestRecord};
use contextful_core::store::reconcile::{Column, ColumnType, Schema};
use contextful_core::store::reserve::{ledger_path, LEDGER_RETENTION_SECS};
use contextful_core::time::Instant;
use parquet::arrow::ArrowWriter;
use parquet::basic::{Compression, ZstdLevel};
use parquet::file::properties::WriterProperties;
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// The directory holding a table's ledger files.
const LEDGER_DIR: &str = contextful_core::store::lay_out::REQUESTS_DIR;
/// The prefix of a merged ledger file, `folded-<snapshot-id>.parquet` (`store.reserve.ledger-fold`).
pub const FOLDED_PREFIX: &str = "folded-";
/// The column a merged ledger file carries beside the ledger's own: each row's run commit
/// instant, from which retention ages it (`store.reserve.ledger-retention`).
const COMMITTED_AT: &str = "committed_at";
/// How long an append waits for another flush of the same run to release the file.
const LOCK_WAIT: std::time::Duration = std::time::Duration::from_secs(10);

/// Append `rows` to run `run_id`'s ledger for `table` as written by `node`. An empty
/// `rows` writes nothing.
pub fn append(store: &Store, table: &str, run_id: &str, node: &NodeId, rows: &[RequestRecord]) -> Result<()> {
    store.check_writable("append a request ledger")?;
    if run_id.len() > 128 || !is_path_segment(run_id) {
        return Err(ContextError::Invalid(format!("run id `{run_id}` is not 1 to 128 chars of [A-Za-z0-9._-]")));
    }
    if rows.is_empty() {
        return Ok(());
    }
    let path = store.table_dir(table)?.join(ledger_path(run_id, node.as_str()));
    let dir = path.parent().unwrap_or(Path::new("."));
    std::fs::create_dir_all(dir).at(dir)?;
    let lock_path = path.with_extension("parquet.lock");
    let _lock = FileLock::acquire_within(&lock_path, LOCK_WAIT)?.ok_or_else(|| {
        ContextError::Invalid(format!("`{}` stayed held {} s by another flush of run `{run_id}`", lock_path.display(), LOCK_WAIT.as_secs()))
    })?;
    let mut all = if path.is_file() { read_for_store(store, &path)? } else { Vec::new() };
    all.extend(rows.iter().map(|r| (run_id.to_string(), r.clone())));
    let plain = encode(&path, &all)?;
    let bytes = match store.sealing() {
        Sealing::Plaintext => plain,
        Sealing::Sealed(cipher) => cipher.seal(&plain).map_err(|e| ContextError::Invalid(format!("{}: sealing: {e}", path.display())))?,
    };
    write_synced(&path, &bytes)
}

/// Replace `path` with `bytes`: a synced sibling temporary file renamed into place, then
/// the directory synced so the rename survives a crash.
fn write_synced(path: &Path, bytes: &[u8]) -> Result<()> {
    use std::io::Write;
    let tmp = path.with_extension("parquet.tmp");
    let written = (|| -> std::io::Result<()> {
        let mut f = std::fs::File::create(&tmp)?;
        f.write_all(bytes)?;
        f.sync_all()?;
        std::fs::rename(&tmp, path)?;
        contextful_fs::open_dir_for_sync(path.parent().unwrap_or(Path::new(".")))?.sync_all()
    })();
    if written.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    written.at(path)
}

/// Every ledger file of `table`, absolute and sorted; a table with no ledger has none.
pub fn files(store: &Store, table: &str) -> Result<Vec<PathBuf>> {
    let dir = store.table_dir(table)?.join(LEDGER_DIR);
    let entries = match std::fs::read_dir(&dir) {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e).at(&dir),
    };
    let mut out = Vec::new();
    for entry in entries {
        let path = entry.at(&dir)?.path();
        if path.is_file() && path.extension().is_some_and(|x| x == "parquet") {
            out.push(path);
        }
    }
    out.sort();
    Ok(out)
}

/// Every `(run_id, record)` a ledger file holds. A file that does not parse refuses
/// rather than restarting empty, since committed rows may join onto it.
pub fn read(path: &Path) -> Result<Vec<(String, RequestRecord)>> {
    decode(path, parquet_io::read(path)?)
}

/// Read a ledger with the project key; sealed bytes never touch a temporary file.
pub fn read_for_store(store: &Store, path: &Path) -> Result<Vec<(String, RequestRecord)>> {
    match store.sealing() {
        Sealing::Plaintext => read(path),
        Sealing::Sealed(cipher) => {
            let sealed = std::fs::read(path).at(path)?;
            let plain = cipher.open(&sealed).map_err(|e| ContextError::Invalid(format!("{}: opening: {e}", path.display())))?;
            decode(path, parquet_io::read_bytes(plain)?)
        }
    }
}

/// Decoded rows stay in RAM; memory exhaustion refuses instead of spilling to disk.
#[cfg(feature = "read")]
pub(crate) fn disable_spilling(conn: &duckdb::Connection) -> std::result::Result<(), crate::read::ReadFault> {
    let fail = |e: duckdb::Error| crate::read::ReadFault::Engine(e.to_string());
    let directory: String = conn.query_row("SELECT current_setting('temp_directory')", [], |row| row.get(0)).map_err(fail)?;
    if !directory.is_empty() {
        conn.execute_batch("SET temp_directory = ''").map_err(fail)?;
    }
    Ok(())
}

/// Materialize decoded ledger rows in a DuckDB temporary table without a plaintext file.
#[cfg(feature = "read")]
pub fn register_memory(conn: &duckdb::Connection, name: &str, rows: &[(String, RequestRecord)]) -> std::result::Result<(), crate::read::ReadFault> {
    use contextful_core::store::relation::ident;
    disable_spilling(conn)?;
    let fail = |e: duckdb::Error| crate::read::ReadFault::Engine(e.to_string());
    conn.execute_batch(&format!(
        "CREATE OR REPLACE TEMP TABLE {} (run_id VARCHAR, batch_seq INTEGER, request_id VARCHAR, vendor_request_id VARCHAR, connector VARCHAR, method VARCHAR, url_host VARCHAR, status_code INTEGER, started_at TIMESTAMP_NS, duration_ms BIGINT)",
        ident(name)
    )).map_err(fail)?;
    let mut insert = conn.prepare(&format!(
        "INSERT INTO {} VALUES (?, ?, ?, ?, ?, ?, ?, ?, make_timestamp_ns(?), ?)", ident(name)
    )).map_err(fail)?;
    for (run_id, row) in rows {
        let nanos = i64::try_from(row.started_at.unix_nanos()).map_err(|_| crate::read::ReadFault::Engine("ledger timestamp exceeds TIMESTAMP_NS".into()))?;
        let duration = i64::try_from(row.duration_ms).map_err(|_| crate::read::ReadFault::Engine("ledger duration exceeds BIGINT".into()))?;
        insert.execute(duckdb::params![
            run_id, row.batch_seq, row.request_id, row.vendor_request_id, row.connector,
            row.method, row.url_host, row.status_code.map(i32::from), nanos, duration,
        ]).map_err(fail)?;
    }
    Ok(())
}

fn decode(path: &Path, batches: Vec<RecordBatch>) -> Result<Vec<(String, RequestRecord)>> {
    let invalid = |what: &str| ContextError::Parquet { path: path.to_path_buf(), message: format!("the ledger column `{what}` is missing or mistyped") };
    let mut out = Vec::new();
    for batch in batches {
        let text = |name: &str| batch.column_by_name(name).and_then(|c| c.as_string_opt::<i32>()).ok_or_else(|| invalid(name));
        let int = |name: &str| batch.column_by_name(name).and_then(|c| c.as_primitive_opt::<Int32Type>()).ok_or_else(|| invalid(name));
        let (run, seq, id, vendor, connector, method, host, status) = (
            text("run_id")?,
            int("batch_seq")?,
            text("request_id")?,
            text("vendor_request_id")?,
            text("connector")?,
            text("method")?,
            text("url_host")?,
            int("status_code")?,
        );
        let started = batch
            .column_by_name("started_at")
            .and_then(|c| c.as_primitive_opt::<TimestampNanosecondType>())
            .ok_or_else(|| invalid("started_at"))?;
        let duration = batch.column_by_name("duration_ms").and_then(|c| c.as_primitive_opt::<Int64Type>()).ok_or_else(|| invalid("duration_ms"))?;
        for i in 0..batch.num_rows() {
            let opt_text = |a: &StringArray| (!a.is_null(i)).then(|| a.value(i).to_string());
            let opt_int = |a: &Int32Array| (!a.is_null(i)).then(|| a.value(i));
            let started_at = Instant::from_unix_nanos(i128::from(started.value(i))).map_err(|_| invalid("started_at"))?;
            out.push((
                run.value(i).to_string(),
                RequestRecord {
                    request_id: id.value(i).to_string(),
                    vendor_request_id: opt_text(vendor),
                    connector: connector.value(i).to_string(),
                    method: method.value(i).to_string(),
                    url_host: host.value(i).to_string(),
                    status_code: opt_int(status).and_then(|s| u16::try_from(s).ok()),
                    started_at,
                    duration_ms: u64::try_from(duration.value(i)).unwrap_or_default(),
                    batch_seq: opt_int(seq),
                },
            ));
        }
    }
    Ok(out)
}

/// The Parquet bytes of one complete ledger file.
fn encode(path: &Path, rows: &[(String, RequestRecord)]) -> Result<Vec<u8>> {
    encode_with(path, rows, None)
}

/// The Parquet bytes of a ledger file, with a `committed_at` column after the ledger's own
/// where `committed` gives each row's run commit instant.
fn encode_with(path: &Path, rows: &[(String, RequestRecord)], committed: Option<&[Instant]>) -> Result<Vec<u8>> {
    let pq = |e: parquet::errors::ParquetError| ContextError::Parquet { path: path.to_path_buf(), message: e.to_string() };
    let mut ledger = ledger_columns();
    if committed.is_some() {
        ledger.push(Column::new(COMMITTED_AT, ColumnType::Timestamp, false));
    }
    let schema = parquet_io::arrow_schema(&Schema { columns: ledger });
    let text = |f: &dyn Fn(&(String, RequestRecord)) -> Option<String>| -> ArrayRef { Arc::new(rows.iter().map(f).collect::<StringArray>()) };
    let columns: Vec<ArrayRef> = vec![
        text(&|(run, _)| Some(run.clone())),
        Arc::new(rows.iter().map(|(_, r)| r.batch_seq).collect::<Int32Array>()),
        text(&|(_, r)| Some(r.request_id.clone())),
        text(&|(_, r)| r.vendor_request_id.clone()),
        text(&|(_, r)| Some(r.connector.clone())),
        text(&|(_, r)| Some(r.method.clone())),
        text(&|(_, r)| Some(r.url_host.clone())),
        Arc::new(rows.iter().map(|(_, r)| r.status_code.map(i32::from)).collect::<Int32Array>()),
        Arc::new(
            rows.iter()
                .map(|(_, r)| i64::try_from(r.started_at.unix_nanos()).ok())
                .collect::<TimestampNanosecondArray>()
                .with_timezone("UTC"),
        ),
        Arc::new(rows.iter().map(|(_, r)| i64::try_from(r.duration_ms).unwrap_or(i64::MAX)).collect::<Int64Array>()),
    ];
    let mut columns = columns;
    if let Some(at) = committed {
        columns.push(Arc::new(at.iter().map(|i| i64::try_from(i.unix_nanos()).ok()).collect::<TimestampNanosecondArray>().with_timezone("UTC")));
    }
    let batch = RecordBatch::try_new(schema.clone(), columns).map_err(|e| ContextError::Parquet { path: path.to_path_buf(), message: e.to_string() })?;
    let mut bytes = Vec::new();
    let props = WriterProperties::builder().set_compression(Compression::ZSTD(ZstdLevel::default())).build();
    let mut w = ArrowWriter::try_new(&mut bytes, schema, Some(props)).map_err(pq)?;
    w.write(&batch).map_err(pq)?;
    w.close().map_err(pq)?;
    Ok(bytes)
}

/// A ledger file as Arrow batches, opened with the project key where sealed.
fn batches_for_store(store: &Store, path: &Path) -> Result<Vec<RecordBatch>> {
    match store.sealing() {
        Sealing::Plaintext => parquet_io::read(path),
        Sealing::Sealed(cipher) => {
            let sealed = std::fs::read(path).at(path)?;
            let plain = cipher.open(&sealed).map_err(|e| ContextError::Invalid(format!("{}: opening: {e}", path.display())))?;
            parquet_io::read_bytes(plain)
        }
    }
}

/// Every row of a merged ledger file with its run's commit instant.
fn read_merged(store: &Store, path: &Path) -> Result<Vec<(String, RequestRecord, Instant)>> {
    let batches = batches_for_store(store, path)?;
    let invalid = || ContextError::Parquet { path: path.to_path_buf(), message: format!("the merged ledger column `{COMMITTED_AT}` is missing or mistyped") };
    let mut at = Vec::new();
    for b in &batches {
        let column = b.column_by_name(COMMITTED_AT).and_then(|c| c.as_primitive_opt::<TimestampNanosecondType>()).ok_or_else(invalid)?;
        for i in 0..b.num_rows() {
            at.push(Instant::from_unix_nanos(i128::from(column.value(i))).map_err(|_| invalid())?);
        }
    }
    Ok(decode(path, batches)?.into_iter().zip(at).map(|((run, r), at)| (run, r, at)).collect())
}

/// Merge `table`'s ledger files of committed runs, with every earlier merge, into
/// `requests/folded-<snapshot_id>.parquet`, drop each row whose run committed 365 d or more
/// before `now`, then remove the files merged. A run with no committed manifest keeps its
/// file. A merge keeps one row per `(run_id, request_id)`, so a merge interrupted before
/// its removals repeats nothing (`store.reserve.ledger-fold`, `store.reserve.ledger-retention`).
/// Returns the merged file, or `None` where no row remains in one.
pub fn fold_ledgers(store: &Store, table: &str, snapshot_id: &str, now: Instant) -> Result<Option<PathBuf>> {
    let files = files(store, table)?;
    if files.is_empty() {
        return Ok(None);
    }
    let committed: std::collections::BTreeMap<String, Instant> = store
        .committed_runs(table)?
        .into_iter()
        .map(|m| (format!("{}.{}.parquet", m.run_id, m.node_id), m.committed_at))
        .collect();
    let mut rows: Vec<(String, RequestRecord, Instant)> = Vec::new();
    let mut merged = Vec::new();
    for path in files {
        let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        if name.starts_with(FOLDED_PREFIX) {
            rows.extend(read_merged(store, &path)?);
            merged.push(path);
        } else if let Some(at) = committed.get(&name) {
            rows.extend(read_for_store(store, &path)?.into_iter().map(|(run, r)| (run, r, *at)));
            merged.push(path);
        }
    }
    let target = store.table_dir(table)?.join(LEDGER_DIR).join(format!("{FOLDED_PREFIX}{snapshot_id}.parquet"));
    if merged.is_empty() {
        return Ok(target.is_file().then_some(target));
    }
    let mut seen = std::collections::BTreeSet::new();
    rows.retain(|(run, r, at)| at.plus_secs(LEDGER_RETENTION_SECS) > now && seen.insert((run.clone(), r.request_id.clone())));
    let wrote = !rows.is_empty();
    if wrote {
        let (calls, at): (Vec<(String, RequestRecord)>, Vec<Instant>) = rows.into_iter().map(|(run, r, at)| ((run, r), at)).unzip();
        let plain = encode_with(&target, &calls, Some(&at))?;
        let bytes = match store.sealing() {
            Sealing::Plaintext => plain,
            Sealing::Sealed(cipher) => cipher.seal(&plain).map_err(|e| ContextError::Invalid(format!("{}: sealing: {e}", target.display())))?,
        };
        write_synced(&target, &bytes)?;
    }
    for path in merged.iter().filter(|p| !(wrote && **p == target)) {
        match std::fs::remove_file(path) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e).at(path),
        }
    }
    Ok(wrote.then_some(target))
}
