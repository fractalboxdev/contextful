//! Parquet files and the Arrow types behind a store schema.

use crate::error::{ContextError, IoPath, Result};
use arrow_array::{new_null_array, RecordBatch};
use arrow_schema::{DataType, Field, Schema as ArrowSchema, TimeUnit};
use contextful_core::store::reconcile::{Column, ColumnType, Schema};
use parquet::arrow::arrow_reader::ParquetRecordBatchReaderBuilder;
use parquet::arrow::ArrowWriter;
use parquet::basic::{Compression, ZstdLevel};
use parquet::file::properties::WriterProperties;
use std::collections::HashMap;
use std::fs::File;
use std::path::Path;
use std::sync::Arc;

const EXTENSION_NAME: &str = "ARROW:extension:name";
const JSON_EXTENSION: &str = "arrow.json";

pub fn data_type(ty: ColumnType) -> DataType {
    match ty {
        ColumnType::Null => DataType::Null,
        ColumnType::Boolean => DataType::Boolean,
        ColumnType::Int32 => DataType::Int32,
        ColumnType::Int64 => DataType::Int64,
        ColumnType::Float64 => DataType::Float64,
        ColumnType::Utf8 | ColumnType::Json => DataType::Utf8,
        ColumnType::Timestamp => DataType::Timestamp(TimeUnit::Nanosecond, Some("UTC".into())),
    }
}

pub fn field(c: &Column) -> Field {
    let f = Field::new(&c.name, data_type(c.ty), c.nullable);
    if c.ty == ColumnType::Json {
        f.with_metadata(HashMap::from([(EXTENSION_NAME.to_string(), JSON_EXTENSION.to_string())]))
    } else {
        f
    }
}

pub fn arrow_schema(s: &Schema) -> Arc<ArrowSchema> {
    Arc::new(ArrowSchema::new(s.columns.iter().map(field).collect::<Vec<_>>()))
}

/// Write one batch as a ZSTD-compressed Parquet file.
pub fn write(path: &Path, batch: &RecordBatch) -> Result<()> {
    let pq = |e: parquet::errors::ParquetError| ContextError::Parquet { path: path.to_path_buf(), message: e.to_string() };
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).at(dir)?;
    }
    let file = File::create(path).at(path)?;
    let props = WriterProperties::builder().set_compression(Compression::ZSTD(ZstdLevel::default())).build();
    let mut w = ArrowWriter::try_new(file, batch.schema(), Some(props)).map_err(pq)?;
    w.write(batch).map_err(pq)?;
    w.close().map_err(pq)?;
    Ok(())
}

/// Read every batch of a Parquet file.
pub fn read(path: &Path) -> Result<Vec<RecordBatch>> {
    let pq = |m: String| ContextError::Parquet { path: path.to_path_buf(), message: m };
    let file = File::open(path).at(path)?;
    let reader = ParquetRecordBatchReaderBuilder::try_new(file).map_err(|e| pq(e.to_string()))?.build().map_err(|e| pq(e.to_string()))?;
    reader.map(|b| b.map_err(|e| pq(e.to_string()))).collect()
}

/// The column names a Parquet file carries, read from its footer.
pub fn columns(path: &Path) -> Result<Vec<String>> {
    let file = File::open(path).at(path)?;
    let b = ParquetRecordBatchReaderBuilder::try_new(file)
        .map_err(|e| ContextError::Parquet { path: path.to_path_buf(), message: e.to_string() })?;
    Ok(b.schema().fields().iter().map(|f| f.name().clone()).collect())
}

/// Conform a batch to `target`: each column cast to its reconciled type, and each
/// column the batch predates backfilled with nulls (`store.reconcile.fold-never-narrows`).
pub fn conform(batch: &RecordBatch, target: &Arc<ArrowSchema>) -> Result<RecordBatch> {
    let rows = batch.num_rows();
    let mut cols = Vec::with_capacity(target.fields().len());
    for f in target.fields() {
        let col = match batch.column_by_name(f.name()) {
            Some(c) if c.data_type() == f.data_type() => c.clone(),
            Some(c) => arrow_cast::cast(c, f.data_type()).map_err(|e| {
                ContextError::Invalid(format!("column `{}` does not widen from {} to {}: {e}", f.name(), c.data_type(), f.data_type()))
            })?,
            None => new_null_array(f.data_type(), rows),
        };
        cols.push(col);
    }
    RecordBatch::try_new(target.clone(), cols).map_err(|e| ContextError::Invalid(e.to_string()))
}
