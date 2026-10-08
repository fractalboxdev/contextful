//! Parquet files and the Arrow types behind a store schema.

use crate::error::{ContextError, IoPath, Result};
use arrow_array::cast::AsArray;
use arrow_array::{
    new_null_array, Array, ArrayRef, ListArray, MapArray, RecordBatch, StringArray, StructArray,
};
use arrow_schema::{DataType, Field, Fields, Schema as ArrowSchema, TimeUnit};
use contextful_core::store::reconcile::{
    variant_fields, Column, ColumnType, FloatItem, Schema, StructField, EXTENSION_NAME, JSON_EXTENSION, LIST_ITEM,
    MAP_ENTRIES, MAP_KEY, MAP_VALUE, VARIANT_EXTENSION, VECTOR_ITEM,
};
use parquet::arrow::arrow_reader::{ArrowReaderOptions, ParquetRecordBatchReaderBuilder};
use parquet::arrow::ArrowWriter;
use parquet::basic::{Compression, ZstdLevel};
use parquet::encryption::decrypt::FileDecryptionProperties;
use parquet::encryption::encrypt::FileEncryptionProperties;
use parquet::file::properties::WriterProperties;
use std::collections::HashMap;
use std::fs::File;
use std::path::Path;
use std::sync::Arc;

/// The Arrow type a column lands as. A vector is a fixed-size list of non-null floats, so
/// a half-float column keeps 2 bytes an element in storage. A struct's fields, a list's
/// item and a map's value are nullable; a map's keys are non-null text.
pub fn data_type(ty: &ColumnType) -> DataType {
    match ty {
        ColumnType::Null => DataType::Null,
        ColumnType::Boolean => DataType::Boolean,
        ColumnType::Int32 => DataType::Int32,
        ColumnType::Int64 => DataType::Int64,
        ColumnType::Float64 => DataType::Float64,
        ColumnType::Utf8 | ColumnType::Json => DataType::Utf8,
        ColumnType::Timestamp => DataType::Timestamp(TimeUnit::Nanosecond, Some("UTC".into())),
        ColumnType::Binary => DataType::Binary,
        ColumnType::FixedSizeBinary(n) => DataType::FixedSizeBinary(width(*n)),
        ColumnType::FixedSizeList(item, n) => DataType::FixedSizeList(
            Arc::new(Field::new(VECTOR_ITEM, data_type_of(*item), false)),
            width(*n),
        ),
        ColumnType::Struct(fields) => DataType::Struct(struct_fields(fields)),
        ColumnType::List(item) => DataType::List(Arc::new(field_of(LIST_ITEM, item, true))),
        ColumnType::Map(value) => DataType::Map(Arc::new(map_entries(value)), false),
        ColumnType::Variant => DataType::Struct(struct_fields(&variant_fields())),
    }
}

/// The Arrow fields of a struct column.
pub fn struct_fields(fields: &[StructField]) -> Fields {
    fields
        .iter()
        .map(|f| field_of(&f.name, &f.ty, true))
        .collect()
}

/// The entries field of a map column: non-null text keys beside nullable values.
pub fn map_entries(value: &ColumnType) -> Field {
    let entries = Fields::from(vec![
        Field::new(MAP_KEY, DataType::Utf8, false),
        field_of(MAP_VALUE, value, true),
    ]);
    Field::new(MAP_ENTRIES, DataType::Struct(entries), false)
}

/// One Arrow field, carrying the JSON extension on a JSON column at any depth.
pub fn field_of(name: &str, ty: &ColumnType, nullable: bool) -> Field {
    let f = Field::new(name, data_type(ty), nullable);
    if matches!(ty, ColumnType::Json | ColumnType::Variant) {
        f.with_metadata(HashMap::from([(
            EXTENSION_NAME.to_string(),
            if *ty == ColumnType::Json { JSON_EXTENSION } else { VARIANT_EXTENSION }.to_string(),
        )]))
    } else {
        f
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
    field_of(&c.name, &c.ty, c.nullable)
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

/// Write one batch with Parquet modular encryption of every column and the footer.
pub fn write_encrypted(path: &Path, batch: &RecordBatch, key: &[u8]) -> Result<()> {
    let pq = |e: parquet::errors::ParquetError| ContextError::Parquet { path: path.to_path_buf(), message: e.to_string() };
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).at(dir)?;
    }
    let encryption = FileEncryptionProperties::builder(key.to_vec()).build().map_err(pq)?;
    let props = WriterProperties::builder()
        .set_compression(Compression::ZSTD(ZstdLevel::default()))
        .with_file_encryption_properties(encryption)
        .build();
    let file = File::create(path).at(path)?;
    let mut writer = ArrowWriter::try_new(file, batch.schema(), Some(props)).map_err(pq)?;
    writer.write(batch).map_err(pq)?;
    writer.close().map_err(pq)?;
    Ok(())
}

/// A store part uses modular encryption exactly when its project binds a key.
pub fn write_with_key(path: &Path, batch: &RecordBatch, key: Option<&[u8]>) -> Result<()> {
    match key {
        Some(key) => write_encrypted(path, batch, key),
        None => write(path, batch),
    }
}

/// Read every batch of a Parquet file.
pub fn read(path: &Path) -> Result<Vec<RecordBatch>> {
    let pq = |m: String| ContextError::Parquet { path: path.to_path_buf(), message: m };
    let file = File::open(path).at(path)?;
    let reader = ParquetRecordBatchReaderBuilder::try_new(file).map_err(|e| pq(e.to_string()))?.build().map_err(|e| pq(e.to_string()))?;
    reader.map(|b| b.map_err(|e| pq(e.to_string()))).collect()
}

/// Read every batch of a modular-encrypted Parquet file with its footer key.
pub fn read_encrypted(path: &Path, key: &[u8]) -> Result<Vec<RecordBatch>> {
    let pq = |e: parquet::errors::ParquetError| ContextError::Parquet { path: path.to_path_buf(), message: e.to_string() };
    let decryption = FileDecryptionProperties::builder(key.to_vec()).build().map_err(pq)?;
    let options = ArrowReaderOptions::new().with_file_decryption_properties(decryption);
    let file = File::open(path).at(path)?;
    let reader = ParquetRecordBatchReaderBuilder::try_new_with_options(file, options).map_err(pq)?.build().map_err(pq)?;
    reader.map(|batch| batch.map_err(|e| ContextError::Parquet { path: path.to_path_buf(), message: e.to_string() })).collect()
}

/// A store part reads with its project's modular encryption setting.
pub fn read_with_key(path: &Path, key: Option<&[u8]>) -> Result<Vec<RecordBatch>> {
    match key {
        Some(key) => read_encrypted(path, key),
        None => read(path),
    }
}

/// Decode a complete Parquet file already held in anonymous process memory.
pub fn read_bytes(bytes: Vec<u8>) -> Result<Vec<RecordBatch>> {
    let path = Path::new("<in-memory parquet>");
    let pq = |e: String| ContextError::Parquet { path: path.to_path_buf(), message: e };
    let reader = ParquetRecordBatchReaderBuilder::try_new(bytes::Bytes::from(bytes))
        .map_err(|e| pq(e.to_string()))?
        .build()
        .map_err(|e| pq(e.to_string()))?;
    reader.map(|batch| batch.map_err(|e| pq(e.to_string()))).collect()
}

fn builder_with_key(path: &Path, key: Option<&[u8]>) -> Result<ParquetRecordBatchReaderBuilder<File>> {
    let file = File::open(path).at(path)?;
    let pq = |e: parquet::errors::ParquetError| ContextError::Parquet { path: path.to_path_buf(), message: e.to_string() };
    match key {
        Some(key) => {
            let decryption = FileDecryptionProperties::builder(key.to_vec()).build().map_err(pq)?;
            let options = ArrowReaderOptions::new().with_file_decryption_properties(decryption);
            ParquetRecordBatchReaderBuilder::try_new_with_options(file, options).map_err(pq)
        }
        None => ParquetRecordBatchReaderBuilder::try_new(file).map_err(pq),
    }
}

/// Where [`copy_inserting`] places an inserted column: beside a column the source file
/// carries.
#[derive(Debug, Clone, Copy)]
pub enum At<'a> {
    Before(&'a str),
    After(&'a str),
}

/// The one value an inserted column holds on every row.
#[derive(Debug, Clone, Copy)]
pub enum Fill {
    Int64(i64),
    /// Nanoseconds since the epoch, in UTC.
    Timestamp(i64),
}

/// One column [`copy_inserting`] adds.
#[derive(Debug, Clone)]
pub struct Insert<'a> {
    pub field: Field,
    pub at: At<'a>,
    pub fill: Fill,
}

/// Copy the Parquet file `from` to `to` with each of `inserts` placed in turn, holding its
/// fill on every row. One record batch of `from` is in memory at a time; an existing `to`
/// is replaced.
pub fn copy_inserting(from: &Path, to: &Path, inserts: &[Insert<'_>]) -> Result<()> {
    copy_inserting_with_key(from, to, inserts, None)
}

/// Copy one store part while retaining its project's modular encryption.
pub fn copy_inserting_with_key(from: &Path, to: &Path, inserts: &[Insert<'_>], key: Option<&[u8]>) -> Result<()> {
    let pq = |path: &Path, m: String| ContextError::Parquet { path: path.to_path_buf(), message: m };
    let builder = builder_with_key(from, key)?;
    let source = builder.schema().clone();
    // Each target column is a source column or an insert, by index.
    enum Src {
        Column(usize),
        Inserted(usize),
    }
    let mut layout: Vec<(Field, Src)> = source.fields().iter().enumerate().map(|(i, f)| (f.as_ref().clone(), Src::Column(i))).collect();
    for (k, insert) in inserts.iter().enumerate() {
        let (anchor, offset) = match insert.at {
            At::Before(name) => (name, 0),
            At::After(name) => (name, 1),
        };
        let at = layout
            .iter()
            .position(|(f, _)| f.name() == anchor)
            .ok_or_else(|| pq(from, format!("no column `{anchor}` to place `{}` beside", insert.field.name())))?;
        layout.insert(at + offset, (insert.field.clone(), Src::Inserted(k)));
    }
    let target = Arc::new(ArrowSchema::new_with_metadata(layout.iter().map(|(f, _)| f.clone()).collect::<Vec<_>>(), source.metadata().clone()));
    let reader = builder.build().map_err(|e| pq(from, e.to_string()))?;
    if to.exists() {
        std::fs::remove_file(to).at(to)?;
    }
    let out = File::create(to).at(to)?;
    let mut props = WriterProperties::builder().set_compression(Compression::ZSTD(ZstdLevel::default()));
    if let Some(key) = key {
        let encryption = FileEncryptionProperties::builder(key.to_vec()).build().map_err(|e| pq(to, e.to_string()))?;
        props = props.with_file_encryption_properties(encryption);
    }
    let props = props.build();
    let mut w = ArrowWriter::try_new(out, target.clone(), Some(props)).map_err(|e| pq(to, e.to_string()))?;
    for batch in reader {
        let batch = batch.map_err(|e| pq(from, e.to_string()))?;
        let n = batch.num_rows();
        let columns: Vec<ArrayRef> = layout
            .iter()
            .map(|(_, src)| match src {
                Src::Column(i) => batch.column(*i).clone(),
                Src::Inserted(k) => match inserts[*k].fill {
                    Fill::Int64(v) => Arc::new(arrow_array::Int64Array::from(vec![v; n])) as ArrayRef,
                    Fill::Timestamp(v) => Arc::new(arrow_array::TimestampNanosecondArray::from(vec![v; n]).with_timezone("UTC")),
                },
            })
            .collect();
        let batch = RecordBatch::try_new(target.clone(), columns).map_err(|e| pq(to, e.to_string()))?;
        w.write(&batch).map_err(|e| pq(to, e.to_string()))?;
    }
    w.close().map_err(|e| pq(to, e.to_string()))?;
    Ok(())
}

/// The column names a Parquet file carries, read from its footer.
pub fn columns(path: &Path) -> Result<Vec<String>> {
    columns_with_key(path, None)
}

pub fn columns_with_key(path: &Path, key: Option<&[u8]>) -> Result<Vec<String>> {
    let b = builder_with_key(path, key)?;
    Ok(b.schema().fields().iter().map(|f| f.name().clone()).collect())
}

/// The store columns a Parquet file carries, read from the Arrow schema in its footer:
/// the inverse of [`field`] over every type a store column lands as.
pub fn schema(path: &Path) -> Result<Vec<Column>> {
    schema_with_key(path, None)
}

pub fn schema_with_key(path: &Path, key: Option<&[u8]>) -> Result<Vec<Column>> {
    let unreadable = |m: String| ContextError::Parquet { path: path.to_path_buf(), message: m };
    let b = builder_with_key(path, key)?;
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
        DataType::Struct(fields) if !fields.is_empty() => {
            let decoded = fields
                .iter()
                .map(|f| Some(StructField::new(f.name(), column_type(f)?)))
                .collect::<Option<Vec<_>>>()?;
            if f.metadata().get(EXTENSION_NAME).is_some_and(|v| v == VARIANT_EXTENSION) {
                (decoded == variant_fields()).then_some(ColumnType::Variant)?
            } else {
                ColumnType::Struct(decoded)
            }
        }
        DataType::List(item) | DataType::LargeList(item) => ColumnType::list(column_type(item)?),
        DataType::Map(entries, _) => {
            let DataType::Struct(kv) = entries.data_type() else {
                return None;
            };
            let [key, value] = kv.iter().collect::<Vec<_>>()[..] else {
                return None;
            };
            (column_type(key)? == ColumnType::Utf8).then_some(())?;
            ColumnType::map(column_type(value)?)
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
    let schema = batch.schema();
    let mut cols = Vec::with_capacity(target.fields().len());
    for f in target.fields() {
        let col = match schema
            .field_with_name(f.name())
            .ok()
            .zip(batch.column_by_name(f.name()))
        {
            Some((source, c)) => conform_array(c, source, f)?,
            None => new_null_array(f.data_type(), rows),
        };
        cols.push(col);
    }
    RecordBatch::try_new(target.clone(), cols).map_err(|e| ContextError::Invalid(e.to_string()))
}

/// Conform one array from its `source` field to the `target` field: a text value promoted
/// to JSON is encoded, a struct gains each field it predates as nulls, a list item and a
/// map value conform in place, and any other difference is a widening cast.
fn conform_array(c: &ArrayRef, source: &Field, target: &Field) -> Result<ArrayRef> {
    let widen = |e: String| {
        ContextError::Invalid(format!(
            "column `{}` does not widen from {} to {}: {e}",
            target.name(),
            c.data_type(),
            target.data_type()
        ))
    };
    if is_json(target) && !is_json(source) {
        return encode_json(c);
    }
    if c.data_type() == target.data_type() {
        return Ok(c.clone());
    }
    Ok(match (source.data_type(), target.data_type()) {
        (DataType::Null, to) => new_null_array(to, c.len()),
        (DataType::Struct(from), DataType::Struct(to)) => {
            let s = c.as_struct();
            let children = to
                .iter()
                .map(|t| match from.iter().position(|f| f.name() == t.name()) {
                    Some(i) => conform_array(s.column(i), &from[i], t),
                    None => Ok(new_null_array(t.data_type(), c.len())),
                })
                .collect::<Result<Vec<_>>>()?;
            if let Some(extra) = from
                .iter()
                .find(|f| !to.iter().any(|t| t.name() == f.name()))
            {
                return Err(widen(format!(
                    "field `{}` is absent from the merged schema; conforming would drop it",
                    extra.name()
                )));
            }
            Arc::new(
                StructArray::try_new(to.clone(), children, s.nulls().cloned())
                    .map_err(|e| widen(e.to_string()))?,
            )
        }
        (DataType::List(from), DataType::List(to)) => {
            let l = c.as_list::<i32>();
            let values = conform_array(l.values(), from, to)?;
            Arc::new(
                ListArray::try_new(to.clone(), l.offsets().clone(), values, l.nulls().cloned())
                    .map_err(|e| widen(e.to_string()))?,
            )
        }
        (DataType::Map(from, _), DataType::Map(to, sorted)) => {
            let m = c.as_map();
            let entries: ArrayRef = Arc::new(m.entries().clone());
            let entries = conform_array(&entries, from, to)?;
            Arc::new(
                MapArray::try_new(
                    to.clone(),
                    m.offsets().clone(),
                    entries.as_struct().clone(),
                    m.nulls().cloned(),
                    *sorted,
                )
                .map_err(|e| widen(e.to_string()))?,
            )
        }
        (_, to) => arrow_cast::cast(c, to).map_err(|e| widen(e.to_string()))?,
    })
}
