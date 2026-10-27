//! The Iceberg schema of a planned export, the Arrow batch a page of read-face rows
//! becomes, and the Parquet data files those batches fill. Files are written into memory,
//! each uploaded once it closes and then dropped (`run.export-iceberg.read`,
//! `run.export-iceberg.file-size`).

use super::remote::Storage;
use anyhow::{anyhow, bail, Context, Result};
use arrow_array::builder::BooleanBufferBuilder;
use arrow_array::{
    ArrayRef, BinaryArray, BooleanArray, FixedSizeBinaryArray, Float32Array, Float64Array, Int32Array, Int64Array, LargeBinaryArray, LargeListArray, LargeStringArray, ListArray, MapArray,
    RecordBatch, StringArray, StructArray, TimestampNanosecondArray,
};
use arrow_buffer::{NullBuffer, OffsetBuffer};
use arrow_schema::{DataType, Fields, Schema as ArrowSchema, TimeUnit};
use base64::Engine;
use contextful_core::export::iceberg::{IcebergPlan, IcebergType, ICEBERG_DATA_FILE_BYTES};
use iceberg::io::FileIO;
use iceberg::spec::{DataContentType, DataFile, ListType, MapType, NestedField, PrimitiveType, Schema, StructType, Type};
use iceberg::writer::file_writer::{FileWriter, FileWriterBuilder, ParquetWriter, ParquetWriterBuilder};
use iceberg::writer::CurrentFileStatus;
use parquet::basic::{Compression, ZstdLevel};
use parquet::file::properties::WriterProperties;
use serde_json::Value;
use std::sync::Arc;

/// Rows per Parquet row group.
const ROW_GROUP_ROWS: usize = 131_072;
/// The in-memory size of one slice written at a time: an eighth of the data file ceiling.
const SLICE_BYTES: u64 = ICEBERG_DATA_FILE_BYTES / 8;

fn iceberg_type(ty: &IcebergType, next: &mut i32) -> Type {
    let mut id = || {
        *next += 1;
        *next
    };
    match ty {
        IcebergType::Boolean => Type::Primitive(PrimitiveType::Boolean),
        IcebergType::Int => Type::Primitive(PrimitiveType::Int),
        IcebergType::Long => Type::Primitive(PrimitiveType::Long),
        IcebergType::Float => Type::Primitive(PrimitiveType::Float),
        IcebergType::Double => Type::Primitive(PrimitiveType::Double),
        IcebergType::String => Type::Primitive(PrimitiveType::String),
        IcebergType::Binary => Type::Primitive(PrimitiveType::Binary),
        IcebergType::Fixed(n) => Type::Primitive(PrimitiveType::Fixed(u64::from(*n))),
        IcebergType::TimestamptzNs => Type::Primitive(PrimitiveType::TimestamptzNs),
        IcebergType::List(item) => {
            let element = id();
            Type::List(ListType { element_field: NestedField::list_element(element, iceberg_type(item, next), false).into() })
        }
        IcebergType::Map(value) => {
            let (key, value_id) = (id(), id());
            Type::Map(MapType {
                key_field: NestedField::map_key_element(key, Type::Primitive(PrimitiveType::String)).into(),
                value_field: NestedField::map_value_element(value_id, iceberg_type(value, next), false).into(),
            })
        }
        IcebergType::Struct(fields) => {
            let ids: Vec<i32> = fields.iter().map(|_| id()).collect();
            Type::Struct(StructType::new(fields.iter().zip(ids).map(|((name, t), id)| NestedField::optional(id, name, iceberg_type(t, next)).into()).collect()))
        }
    }
}

/// The Iceberg schema a plan creates its table with; the catalog may assign other ids.
pub(super) fn schema(plan: &IcebergPlan) -> Result<Schema> {
    let mut next = plan.columns.len() as i32;
    let fields: Vec<Arc<NestedField>> = plan.columns.iter().enumerate().map(|(i, c)| NestedField::optional(i as i32 + 1, &c.name, iceberg_type(&c.ty, &mut next)).into()).collect();
    Ok(Schema::builder().with_schema_id(0).with_fields(fields).build()?)
}

/// A schema's names, types and nullability, its field ids left out, for telling a source
/// schema change from a catalog that numbered the same fields its own way.
pub(super) fn shape(schema: &Schema) -> Value {
    fn ty(t: &Type) -> Value {
        match t {
            Type::Primitive(p) => Value::String(p.to_string()),
            Type::Struct(s) => Value::Array(s.fields().iter().map(|f| serde_json::json!([f.name, f.required, ty(&f.field_type)])).collect()),
            Type::List(l) => serde_json::json!(["list", l.element_field.required, ty(&l.element_field.field_type)]),
            Type::Map(m) => serde_json::json!(["map", ty(&m.key_field.field_type), m.value_field.required, ty(&m.value_field.field_type)]),
        }
    }
    ty(&Type::Struct(schema.as_struct().clone()))
}

fn cells<'a>(values: &'a [&'a Value]) -> impl Iterator<Item = Option<&'a Value>> + 'a {
    values.iter().map(|v| (!v.is_null()).then_some(*v))
}

fn nulls(values: &[&Value]) -> Option<NullBuffer> {
    values.iter().any(|v| v.is_null()).then(|| NullBuffer::from(values.iter().map(|v| !v.is_null()).collect::<Vec<bool>>()))
}

fn integer(column: &str, v: &Value) -> Result<i64> {
    match v {
        Value::Number(n) => n.as_i64(),
        Value::String(s) => s.parse().ok(),
        _ => None,
    }
    .ok_or_else(|| anyhow!("column `{column}` holds `{v}`, which its stored integer type does not"))
}

fn float(column: &str, v: &Value) -> Result<f64> {
    match v {
        Value::Number(n) => n.as_f64(),
        Value::String(s) if s == "NaN" => Some(f64::NAN),
        Value::String(s) if s == "inf" => Some(f64::INFINITY),
        Value::String(s) if s == "-inf" => Some(f64::NEG_INFINITY),
        Value::String(s) => s.parse().ok(),
        _ => None,
    }
    .ok_or_else(|| anyhow!("column `{column}` holds `{v}`, which its stored float type does not"))
}

fn text(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

fn bytes(column: &str, v: &Value) -> Result<Vec<u8>> {
    let s = v.as_str().ok_or_else(|| anyhow!("column `{column}` holds `{v}`, not base64 bytes"))?;
    base64::engine::general_purpose::STANDARD.decode(s).with_context(|| format!("column `{column}`: base64"))
}

/// Offsets and the flattened children of a list or map column.
fn offsets<'a, T>(values: &[&'a Value], children: impl Fn(&'a Value) -> Option<Vec<T>>, column: &str) -> Result<(Vec<usize>, Vec<T>)> {
    let (mut ends, mut flat) = (vec![0usize], Vec::new());
    for v in values {
        if !v.is_null() {
            flat.extend(children(v).ok_or_else(|| anyhow!("column `{column}` holds `{v}`, not a list or object"))?);
        }
        ends.push(flat.len());
    }
    Ok((ends, flat))
}

/// The Arrow array of type `data_type` holding `values`, cell-encoded as the read face
/// answers them (`read.respond.cell-encoding`).
fn array(data_type: &DataType, values: &[&Value], column: &str) -> Result<ArrayRef> {
    Ok(match data_type {
        DataType::Boolean => Arc::new(cells(values).map(|v| v.map(|v| v.as_bool().ok_or_else(|| anyhow!("column `{column}` holds `{v}`, not a boolean"))).transpose()).collect::<Result<BooleanArray>>()?),
        DataType::Int32 => Arc::new(cells(values).map(|v| v.map(|v| integer(column, v).and_then(|n| i32::try_from(n).map_err(Into::into))).transpose()).collect::<Result<Int32Array>>()?),
        DataType::Int64 => Arc::new(cells(values).map(|v| v.map(|v| integer(column, v)).transpose()).collect::<Result<Int64Array>>()?),
        DataType::Float32 => Arc::new(cells(values).map(|v| v.map(|v| float(column, v).map(|f| f as f32)).transpose()).collect::<Result<Float32Array>>()?),
        DataType::Float64 => Arc::new(cells(values).map(|v| v.map(|v| float(column, v)).transpose()).collect::<Result<Float64Array>>()?),
        DataType::Utf8 => Arc::new(cells(values).map(|v| v.map(text)).collect::<StringArray>()),
        DataType::LargeUtf8 => Arc::new(cells(values).map(|v| v.map(text)).collect::<LargeStringArray>()),
        DataType::Binary => Arc::new(BinaryArray::from_iter(cells(values).map(|v| v.map(|v| bytes(column, v)).transpose()).collect::<Result<Vec<_>>>()?)),
        DataType::LargeBinary => Arc::new(LargeBinaryArray::from_iter(cells(values).map(|v| v.map(|v| bytes(column, v)).transpose()).collect::<Result<Vec<_>>>()?)),
        DataType::FixedSizeBinary(n) => {
            let decoded = cells(values).map(|v| v.map(|v| bytes(column, v)).transpose()).collect::<Result<Vec<_>>>()?;
            Arc::new(FixedSizeBinaryArray::try_from_sparse_iter_with_size(decoded.into_iter(), *n)?)
        }
        DataType::Timestamp(TimeUnit::Nanosecond, tz) => {
            let nanos = cells(values)
                .map(|v| {
                    v.map(|v| {
                        let s = v.as_str().ok_or_else(|| anyhow!("column `{column}` holds `{v}`, not an ISO-8601 timestamp"))?;
                        let at = contextful_core::time::Instant::parse(s).map_err(|e| anyhow!("column `{column}`: {e}"))?;
                        Ok(i64::try_from(at.unix_nanos())?)
                    })
                    .transpose()
                })
                .collect::<Result<Vec<Option<i64>>>>()?;
            Arc::new(TimestampNanosecondArray::from(nanos).with_timezone_opt(tz.clone()))
        }
        DataType::List(item) | DataType::LargeList(item) => {
            let (ends, flat) = offsets(values, |v| v.as_array().map(|a| a.iter().collect()), column)?;
            let child = array(item.data_type(), &flat, column)?;
            match data_type {
                DataType::List(_) => Arc::new(ListArray::try_new(item.clone(), OffsetBuffer::from_lengths(ends.windows(2).map(|w| w[1] - w[0])), child, nulls(values))?),
                _ => Arc::new(LargeListArray::try_new(item.clone(), OffsetBuffer::from_lengths(ends.windows(2).map(|w| w[1] - w[0])), child, nulls(values))?),
            }
        }
        DataType::Struct(fields) => Arc::new(structure(fields, values, column)?),
        DataType::Map(entries, sorted) => {
            let DataType::Struct(kv) = entries.data_type() else { bail!("column `{column}`: a map's entries are a struct") };
            let (ends, flat) = offsets(values, |v| v.as_object().map(|o| o.iter().collect::<Vec<_>>()), column)?;
            let keys: Vec<Value> = flat.iter().map(|(k, _)| Value::String((*k).clone())).collect();
            let key_refs: Vec<&Value> = keys.iter().collect();
            let value_refs: Vec<&Value> = flat.iter().map(|(_, v)| *v).collect();
            let key_array = array(kv[0].data_type(), &key_refs, column)?;
            let value_array = array(kv[1].data_type(), &value_refs, column)?;
            let entries_array = StructArray::try_new(kv.clone(), vec![key_array, value_array], None)?;
            Arc::new(MapArray::try_new(entries.clone(), OffsetBuffer::from_lengths(ends.windows(2).map(|w| w[1] - w[0])), entries_array, nulls(values), *sorted)?)
        }
        other => bail!("column `{column}`: no encoding for Arrow type {other}"),
    })
}

fn structure(fields: &Fields, values: &[&Value], column: &str) -> Result<StructArray> {
    let mut children = Vec::with_capacity(fields.len());
    for field in fields {
        let child: Vec<&Value> = values.iter().map(|v| v.get(field.name()).unwrap_or(&Value::Null)).collect();
        children.push(array(field.data_type(), &child, &format!("{column}.{}", field.name()))?);
    }
    let mut valid = BooleanBufferBuilder::new(values.len());
    for v in values {
        valid.append(!v.is_null());
    }
    let nulls = values.iter().any(|v| v.is_null()).then(|| NullBuffer::new(valid.finish()));
    Ok(StructArray::try_new(fields.clone(), children, nulls)?)
}

/// One page of read-face rows as a batch under the table's Arrow schema; a column the
/// schema holds and the page does not reads as null.
pub(super) fn batch(schema: &Arc<ArrowSchema>, columns: &[String], rows: &[Vec<Value>]) -> Result<RecordBatch> {
    let null = Value::Null;
    let mut arrays = Vec::with_capacity(schema.fields().len());
    for field in schema.fields() {
        let at = columns.iter().position(|c| c == field.name());
        let values: Vec<&Value> = rows.iter().map(|r| at.and_then(|i| r.get(i)).unwrap_or(&null)).collect();
        arrays.push(array(field.data_type(), &values, field.name())?);
    }
    Ok(RecordBatch::try_new(schema.clone(), arrays)?)
}

/// The Arrow schema, field ids attached, of an Iceberg schema.
pub(super) fn arrow_schema(schema: &Schema) -> Result<Arc<ArrowSchema>> {
    Ok(Arc::new(iceberg::arrow::schema_to_arrow_schema(schema)?))
}

/// Parquet data files written into memory, each uploaded once it reaches the ceiling or the
/// export ends. Every uploaded location is pushed onto `uploaded` before the next file opens,
/// so a failure deletes exactly what this export wrote.
pub(super) struct Files<'a> {
    rt: &'a tokio::runtime::Runtime,
    io: FileIO,
    schema: Arc<Schema>,
    location: String,
    spec_id: i32,
    export: uuid::Uuid,
    storage: &'a Storage,
    open: Option<ParquetWriter>,
    count: usize,
    done: Vec<DataFile>,
}

impl<'a> Files<'a> {
    pub fn new(rt: &'a tokio::runtime::Runtime, schema: Arc<Schema>, location: &str, spec_id: i32, storage: &'a Storage) -> Files<'a> {
        Files { rt, io: FileIO::new_with_memory(), schema, location: location.trim_end_matches('/').to_string(), spec_id, export: uuid::Uuid::new_v4(), storage, open: None, count: 0, done: Vec::new() }
    }

    fn properties() -> WriterProperties {
        WriterProperties::builder().set_compression(Compression::ZSTD(ZstdLevel::try_new(1).unwrap_or_default())).set_max_row_group_row_count(Some(ROW_GROUP_ROWS)).build()
    }

    /// Write `batch` in slices, closing the open file before a slice, at its in-memory size,
    /// would carry it past the ceiling (`run.export-iceberg.file-size`). Encoded Parquet is no
    /// larger than the Arrow buffers it encodes, so the estimate keeps each file under it.
    pub fn write(&mut self, batch: &RecordBatch, uploaded: &mut Vec<String>) -> Result<()> {
        let rows = batch.num_rows();
        if rows == 0 {
            return Ok(());
        }
        let per_row = (batch.get_array_memory_size() as u64).div_ceil(rows as u64).max(1);
        let step = usize::try_from((SLICE_BYTES / per_row).max(1)).unwrap_or(rows).min(rows);
        let mut offset = 0;
        while offset < rows {
            let len = step.min(rows - offset);
            let estimate = per_row * len as u64;
            if let Some(open) = &self.open {
                if open.current_written_size() as u64 + estimate > ICEBERG_DATA_FILE_BYTES {
                    self.close(uploaded)?;
                }
            }
            if self.open.is_none() {
                let path = format!("{}/data/{}-{:05}.parquet", self.location, self.export, self.count);
                self.count += 1;
                let output = self.io.new_output(&path)?;
                let builder = ParquetWriterBuilder::new(Self::properties(), self.schema.clone());
                self.open = Some(self.rt.block_on(builder.build(output))?);
            }
            let Some(writer) = self.open.as_mut() else { bail!("no data file is open") };
            self.rt.block_on(writer.write(&batch.slice(offset, len)))?;
            offset += len;
        }
        Ok(())
    }

    fn close(&mut self, uploaded: &mut Vec<String>) -> Result<()> {
        let Some(writer) = self.open.take() else { return Ok(()) };
        let path = writer.current_file_path();
        let builders = self.rt.block_on(writer.close())?;
        let bytes = self.rt.block_on(async { self.io.new_input(&path)?.read().await })?;
        // Recorded before the upload: a store that kept the object while the answer was lost
        // still has it deleted.
        uploaded.push(path.clone());
        self.storage.put(&path, &bytes)?;
        self.rt.block_on(self.io.delete(&path))?;
        for mut builder in builders {
            self.done.push(builder.content(DataContentType::Data).partition_spec_id(self.spec_id).build().map_err(|e| anyhow!("building the data file entry: {e}"))?);
        }
        Ok(())
    }

    /// Close the open file and answer every data file this export wrote.
    pub fn finish(mut self, uploaded: &mut Vec<String>) -> Result<Vec<DataFile>> {
        self.close(uploaded)?;
        Ok(self.done)
    }
}

