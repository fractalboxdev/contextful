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
use arrow_array::cast::AsArray;
use arrow_array::types::{Int32Type, Int64Type, TimestampNanosecondType};
use arrow_array::{Array, ArrayRef, Int32Array, Int64Array, RecordBatch, StringArray, TimestampNanosecondArray};
use contextful_core::store::lay_out::{is_path_segment, NodeId};
use contextful_core::store::ledger::{ledger_columns, RequestRecord};
use contextful_core::store::reconcile::Schema;
use contextful_core::store::reserve::ledger_path;
use contextful_core::time::Instant;
use parquet::arrow::ArrowWriter;
use parquet::basic::{Compression, ZstdLevel};
use parquet::file::properties::WriterProperties;
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// The directory holding a table's ledger files.
const LEDGER_DIR: &str = "requests";
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
    let mut all = if path.is_file() { read(&path)? } else { Vec::new() };
    all.extend(rows.iter().map(|r| (run_id.to_string(), r.clone())));
    write_synced(&path, &encode(&path, &all)?)
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
        std::fs::File::open(path.parent().unwrap_or(Path::new(".")))?.sync_all()
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
    let invalid = |what: &str| ContextError::Parquet { path: path.to_path_buf(), message: format!("the ledger column `{what}` is missing or mistyped") };
    let mut out = Vec::new();
    for batch in parquet_io::read(path)? {
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
    let pq = |e: parquet::errors::ParquetError| ContextError::Parquet { path: path.to_path_buf(), message: e.to_string() };
    let schema = parquet_io::arrow_schema(&Schema { columns: ledger_columns() });
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
    let batch = RecordBatch::try_new(schema.clone(), columns).map_err(|e| ContextError::Parquet { path: path.to_path_buf(), message: e.to_string() })?;
    let mut bytes = Vec::new();
    let props = WriterProperties::builder().set_compression(Compression::ZSTD(ZstdLevel::default())).build();
    let mut w = ArrowWriter::try_new(&mut bytes, schema, Some(props)).map_err(pq)?;
    w.write(&batch).map_err(pq)?;
    w.close().map_err(pq)?;
    Ok(bytes)
}
