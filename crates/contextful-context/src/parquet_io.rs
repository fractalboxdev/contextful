//! Parquet files and the Arrow types behind a store schema.

use crate::error::{ContextError, IoPath, Result};
use arrow_array::{new_null_array, Array, ArrayRef, RecordBatch, StringArray};
use arrow_schema::{DataType, Field, Schema as ArrowSchema, TimeUnit};
use contextful_core::store::reconcile::{Column, ColumnType, FloatItem, Schema, EXTENSION_NAME, JSON_EXTENSION, VECTOR_ITEM};
use parquet::arrow::arrow_reader::ParquetRecordBatchReaderBuilder;
use parquet::arrow::ArrowWriter;
use parquet::basic::{Compression, ZstdLevel};
use parquet::file::properties::WriterProperties;
use std::collections::HashMap;
use std::fs::File;
use std::path::Path;
use std::sync::Arc;

/// The Arrow type a column lands as. A vector is a fixed-size list of non-null floats, so
/// a half-float column keeps 2 bytes an element in storage.
pub fn data_type(ty: ColumnType) -> DataType {
    match ty {
        ColumnType::Null => DataType::Null,
        ColumnType::Boolean => DataType::Boolean,
        ColumnType::Int32 => DataType::Int32,
        ColumnType::Int64 => DataType::Int64,
        ColumnType::Float64 => DataType::Float64,
        ColumnType::Utf8 | ColumnType::Json => DataType::Utf8,
        ColumnType::Timestamp => DataType::Timestamp(TimeUnit::Nanosecond, Some("UTC".into())),
        ColumnType::Binary => DataType::Binary,
        ColumnType::FixedSizeBinary(n) => DataType::FixedSizeBinary(width(n)),
        ColumnType::FixedSizeList(item, n) => DataType::FixedSizeList(Arc::new(Field::new(VECTOR_ITEM, data_type_of(item), false)), width(n)),
    }
}

/// The Arrow type of a vector's elements.
pub fn data_type_of(item: FloatItem) -> DataType {
    match item {
        FloatItem::Float32 => DataType::Float32,
        FloatItem::Float16 => DataType::Float16,
    }
}

/// A declared width as Arrow's signed width; the declaration parser admits none wider
/// than [`contextful_core::store::reconcile::MAX_WIDTH`].
pub fn width(n: u32) -> i32 {
    i32::try_from(n).unwrap_or(i32::MAX)
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

/// The store columns a Parquet file carries, read from the Arrow schema in its footer:
/// the inverse of [`field`] over every type a store column lands as.
pub fn schema(path: &Path) -> Result<Vec<Column>> {
    let unreadable = |m: String| ContextError::Parquet { path: path.to_path_buf(), message: m };
    let file = File::open(path).at(path)?;
    let b = ParquetRecordBatchReaderBuilder::try_new(file).map_err(|e| unreadable(e.to_string()))?;
    b.schema()
        .fields()
        .iter()
        .map(|f| {
            let ty = column_type(f).ok_or_else(|| unreadable(format!("column `{}` is typed {}, no store type", f.name(), f.data_type())))?;
            Ok(Column::new(f.name(), ty, f.is_nullable()))
        })
        .collect()
}

/// The store type an Arrow field carries, or `None` for a type no store column lands as.
fn column_type(f: &Field) -> Option<ColumnType> {
    Some(match f.data_type() {
        DataType::Null => ColumnType::Null,
        DataType::Boolean => ColumnType::Boolean,
        DataType::Int32 => ColumnType::Int32,
        DataType::Int64 => ColumnType::Int64,
        DataType::Float64 => ColumnType::Float64,
        DataType::Utf8 if is_json(f) => ColumnType::Json,
        DataType::Utf8 => ColumnType::Utf8,
        DataType::Timestamp(TimeUnit::Nanosecond, _) => ColumnType::Timestamp,
        DataType::Binary => ColumnType::Binary,
        DataType::FixedSizeBinary(n) => ColumnType::FixedSizeBinary(u32::try_from(*n).ok()?),
        DataType::FixedSizeList(item, n) => {
            let item = match item.data_type() {
                DataType::Float32 => FloatItem::Float32,
                DataType::Float16 => FloatItem::Float16,
                _ => return None,
            };
            ColumnType::FixedSizeList(item, u32::try_from(*n).ok()?)
        }
        _ => return None,
    })
}

fn is_json(f: &Field) -> bool {
    f.metadata().get(EXTENSION_NAME).is_some_and(|v| v == JSON_EXTENSION)
}

/// A text column a later batch promoted to JSON, each string replaced by its JSON
/// encoding, so every value of a JSON column is a JSON document.
fn encode_json(c: &ArrayRef) -> Result<ArrayRef> {
    let text = arrow_cast::cast(c, &DataType::Utf8).map_err(|e| ContextError::Invalid(e.to_string()))?;
    let text = text.as_any().downcast_ref::<StringArray>().expect("cast to Utf8");
    let encoded: StringArray =
        text.iter().map(|v| v.map(|s| serde_json::Value::String(s.to_string()).to_string())).collect();
    Ok(Arc::new(encoded))
}

/// Conform a batch to `target`: each column cast to its reconciled type, and each
/// column the batch predates backfilled with nulls (`store.reconcile.fold-never-narrows`).
/// A column the target lacks refuses rather than being dropped.
pub fn conform(batch: &RecordBatch, target: &Arc<ArrowSchema>) -> Result<RecordBatch> {
    if let Some(extra) = batch.schema().fields().iter().find(|f| target.field_with_name(f.name()).is_err()) {
        return Err(ContextError::Invalid(format!(
            "column `{}` is absent from the merged schema; conforming would drop it",
            extra.name()
        )));
    }
    let rows = batch.num_rows();
    let mut cols = Vec::with_capacity(target.fields().len());
    for f in target.fields() {
        let col = match batch.column_by_name(f.name()) {
            Some(c) if is_json(f) && !batch.schema().field_with_name(f.name()).is_ok_and(is_json) => encode_json(c)?,
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
