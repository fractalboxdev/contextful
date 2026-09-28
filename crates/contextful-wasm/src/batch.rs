//! A guest batch crosses as Arrow IPC stream bytes (`connector.export.batch-encoding`);
//! the host reads them into the rows the run path lands, beside the column types the
//! Arrow schema fixes (`connector.export.arrow-types`).

use arrow_array::cast::AsArray;
use arrow_array::types::{Float16Type, Float32Type, Float64Type, Int32Type, Int64Type, TimestampMillisecondType};
use arrow_array::{Array, RecordBatch};
use arrow_ipc::reader::StreamReader;
use arrow_schema::{DataType, TimeUnit};
use contextful_core::run::ports::Row;
use contextful_core::run::{Failure, FailureTag};
use contextful_core::store::reconcile::{encode_binary, ColumnType, FloatItem};
use serde_json::Value;
use std::collections::BTreeMap;

fn incompatible(m: String) -> Failure {
    Failure::new(FailureTag::SchemaIncompatible, m)
}

/// One IPC stream as rows, and the types of the columns no JSON value carries on its own,
/// spelled as a declaration spells them.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Read {
    pub rows: Vec<Row>,
    pub types: BTreeMap<String, String>,
}

/// Read every row of one IPC stream. A `bytes` column lands as padded base64, a
/// fixed-size float list as a number array, a millisecond timestamp as epoch
/// milliseconds, and a null as null.
pub fn read(ipc: &[u8]) -> Result<Read, Failure> {
    let reader = StreamReader::try_new(std::io::Cursor::new(ipc), None).map_err(|e| incompatible(format!("a batch is not Arrow IPC: {e}")))?;
    let mut out = Read::default();
    for (name, ty) in reader.schema().fields().iter().filter_map(|f| Some((f.name(), declared(f.data_type())?))) {
        out.types.insert(name.clone(), ty.spell());
    }
    for batch in reader {
        let batch = batch.map_err(|e| incompatible(format!("a batch is not Arrow IPC: {e}")))?;
        append(&batch, &mut out.rows)?;
    }
    Ok(out)
}

/// The rows of one IPC stream alone.
pub fn rows(ipc: &[u8]) -> Result<Vec<Row>, Failure> {
    read(ipc).map(|r| r.rows)
}

/// The store type an Arrow type fixes where a JSON value cannot.
fn declared(ty: &DataType) -> Option<ColumnType> {
    match ty {
        DataType::Binary => Some(ColumnType::Binary),
        DataType::FixedSizeBinary(n) => u32::try_from(*n).ok().map(ColumnType::FixedSizeBinary),
        DataType::FixedSizeList(item, n) => {
            let item = match item.data_type() {
                DataType::Float32 => FloatItem::Float32,
                DataType::Float16 => FloatItem::Float16,
                _ => return None,
            };
            u32::try_from(*n).ok().map(|n| ColumnType::FixedSizeList(item, n))
        }
        _ => None,
    }
}

fn append(batch: &RecordBatch, out: &mut Vec<Row>) -> Result<(), Failure> {
    let schema = batch.schema();
    let start = out.len();
    out.extend((0..batch.num_rows()).map(|_| Row::new()));
    for (field, col) in schema.fields().iter().zip(batch.columns()) {
        for i in 0..batch.num_rows() {
            let v = if col.is_null(i) { Value::Null } else { cell(col.as_ref(), i, field.name())? };
            out[start + i].insert(field.name().clone(), v);
        }
    }
    Ok(())
}

fn cell(col: &dyn Array, i: usize, name: &str) -> Result<Value, Failure> {
    Ok(match col.data_type() {
        DataType::Boolean => Value::Bool(col.as_boolean().value(i)),
        DataType::Int32 => Value::from(col.as_primitive::<Int32Type>().value(i)),
        DataType::Int64 => Value::from(col.as_primitive::<Int64Type>().value(i)),
        DataType::Float64 => serde_json::Number::from_f64(col.as_primitive::<Float64Type>().value(i)).map_or(Value::Null, Value::Number),
        DataType::Utf8 => Value::String(col.as_string::<i32>().value(i).to_string()),
        DataType::LargeUtf8 => Value::String(col.as_string::<i64>().value(i).to_string()),
        DataType::Binary => Value::String(encode_binary(col.as_binary::<i32>().value(i))),
        DataType::FixedSizeBinary(_) => Value::String(encode_binary(col.as_fixed_size_binary().value(i))),
        DataType::FixedSizeList(item, _) if matches!(item.data_type(), DataType::Float32 | DataType::Float16) => {
            let v = col.as_fixed_size_list().value(i);
            let floats: Vec<f64> = match item.data_type() {
                DataType::Float32 => v.as_primitive::<Float32Type>().values().iter().map(|x| f64::from(*x)).collect(),
                _ => v.as_primitive::<Float16Type>().values().iter().map(|x| x.to_f64()).collect(),
            };
            Value::Array(floats.into_iter().map(|x| serde_json::Number::from_f64(x).map_or(Value::Null, Value::Number)).collect())
        }
        DataType::Timestamp(TimeUnit::Millisecond, _) => Value::from(col.as_primitive::<TimestampMillisecondType>().value(i)),
        other => return Err(incompatible(format!("column `{name}` is Arrow {other}, outside the connector taxonomy"))),
    })
}
