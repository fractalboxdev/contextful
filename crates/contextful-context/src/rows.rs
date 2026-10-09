//! A table's rows as JSON objects, read off its current file list and projected to the
//! columns a caller names. Bytes render as padded base64 and a fixed-size float vector as
//! a number array (`read.respond.bytes-and-vectors`); a list renders as an array, and a
//! struct or a map as an object (`read.respond.nested-values`).

use crate::error::{ContextError, Result};
use crate::scan::scan;
use crate::store::Store;
use arrow_array::cast::AsArray;
use arrow_array::types::{
    Float16Type, Float32Type, Float64Type, Int16Type, Int32Type, Int64Type, Int8Type, TimestampMicrosecondType,
    TimestampMillisecondType, TimestampNanosecondType, TimestampSecondType, UInt16Type, UInt32Type, UInt64Type, UInt8Type,
};
use arrow_array::{Array, RecordBatch};
use arrow_cast::display::{ArrayFormatter, FormatOptions};
use arrow_schema::{DataType, TimeUnit};
use contextful_core::store::bound_time::Bounds;
use contextful_core::store::declare::TableDecl;
use contextful_core::store::reconcile::encode_binary;
use contextful_core::time::Instant;
use serde_json::{Map, Value};

const DAY_NANOS: i128 = 86_400 * 1_000_000_000;

fn number(x: f64) -> Value {
    serde_json::Number::from_f64(x).map(Value::Number).unwrap_or_else(|| Value::String(x.to_string()))
}

/// An instant as fixed-width RFC 3339 with nanoseconds, so text order is time order.
fn instant(column: &str, nanos: i128) -> Result<String> {
    Instant::from_unix_nanos(nanos)
        .map(Instant::to_rfc3339_nanos)
        .map_err(|e| ContextError::Invalid(format!("column `{column}`: {e}")))
}

fn cell(column: &str, array: &dyn Array, i: usize) -> Result<Value> {
    if array.is_null(i) {
        return Ok(Value::Null);
    }
    Ok(match array.data_type() {
        DataType::Utf8 => Value::String(array.as_string::<i32>().value(i).to_string()),
        DataType::LargeUtf8 => Value::String(array.as_string::<i64>().value(i).to_string()),
        DataType::Utf8View => Value::String(array.as_string_view().value(i).to_string()),
        DataType::Boolean => Value::Bool(array.as_boolean().value(i)),
        DataType::Int8 => Value::from(array.as_primitive::<Int8Type>().value(i)),
        DataType::Int16 => Value::from(array.as_primitive::<Int16Type>().value(i)),
        DataType::Int32 => Value::from(array.as_primitive::<Int32Type>().value(i)),
        DataType::Int64 => Value::from(array.as_primitive::<Int64Type>().value(i)),
        DataType::UInt8 => Value::from(array.as_primitive::<UInt8Type>().value(i)),
        DataType::UInt16 => Value::from(array.as_primitive::<UInt16Type>().value(i)),
        DataType::UInt32 => Value::from(array.as_primitive::<UInt32Type>().value(i)),
        DataType::UInt64 => Value::from(array.as_primitive::<UInt64Type>().value(i)),
        DataType::Float16 => number(f64::from(array.as_primitive::<Float16Type>().value(i).to_f32())),
        DataType::Float32 => number(f64::from(array.as_primitive::<Float32Type>().value(i))),
        DataType::Float64 => number(array.as_primitive::<Float64Type>().value(i)),
        DataType::Timestamp(unit, _) => {
            let nanos = match unit {
                TimeUnit::Second => i128::from(array.as_primitive::<TimestampSecondType>().value(i)) * 1_000_000_000,
                TimeUnit::Millisecond => i128::from(array.as_primitive::<TimestampMillisecondType>().value(i)) * 1_000_000,
                TimeUnit::Microsecond => i128::from(array.as_primitive::<TimestampMicrosecondType>().value(i)) * 1_000,
                TimeUnit::Nanosecond => i128::from(array.as_primitive::<TimestampNanosecondType>().value(i)),
            };
            Value::String(instant(column, nanos)?)
        }
        DataType::Date32 => {
            let days = i128::from(array.as_primitive::<arrow_array::types::Date32Type>().value(i));
            Value::String(instant(column, days * DAY_NANOS)?[..10].to_string())
        }
        DataType::Date64 => {
            let millis = i128::from(array.as_primitive::<arrow_array::types::Date64Type>().value(i));
            Value::String(instant(column, millis.div_euclid(86_400_000) * DAY_NANOS)?[..10].to_string())
        }
        DataType::Decimal32(..) | DataType::Decimal64(..) | DataType::Decimal128(..) | DataType::Decimal256(..) => {
            let f = ArrayFormatter::try_new(array, &FormatOptions::default()).map_err(|e| ContextError::Invalid(format!("column `{column}`: {e}")))?;
            Value::String(f.value(i).to_string())
        }
        DataType::Binary => Value::String(encode_binary(array.as_binary::<i32>().value(i))),
        DataType::LargeBinary => Value::String(encode_binary(array.as_binary::<i64>().value(i))),
        DataType::BinaryView => Value::String(encode_binary(array.as_binary_view().value(i))),
        DataType::FixedSizeBinary(_) => Value::String(encode_binary(array.as_fixed_size_binary().value(i))),
        DataType::FixedSizeList(item, _) if matches!(item.data_type(), DataType::Float16 | DataType::Float32 | DataType::Float64) => {
            let v = array.as_fixed_size_list().value(i);
            Value::Array((0..v.len()).map(|j| cell(column, v.as_ref(), j)).collect::<Result<_>>()?)
        }
        DataType::List(_) => {
            let v = array.as_list::<i32>().value(i);
            Value::Array((0..v.len()).map(|j| cell(column, v.as_ref(), j)).collect::<Result<_>>()?)
        }
        DataType::LargeList(_) => {
            let v = array.as_list::<i64>().value(i);
            Value::Array((0..v.len()).map(|j| cell(column, v.as_ref(), j)).collect::<Result<_>>()?)
        }
        DataType::Struct(fields) => {
            let s = array.as_struct();
            Value::Object(fields.iter().zip(s.columns()).map(|(f, c)| Ok((f.name().clone(), cell(column, c.as_ref(), i)?))).collect::<Result<_>>()?)
        }
        DataType::Map(..) => {
            // An entry's key is non-null text, so each entry is one member of an object.
            let entries = array.as_map().value(i);
            let (keys, values) = (entries.column(0), entries.column(1));
            let mut out = Map::new();
            for j in 0..entries.len() {
                let key = match cell(column, keys.as_ref(), j)? {
                    Value::String(k) => k,
                    other => other.to_string(),
                };
                out.insert(key, cell(column, values.as_ref(), j)?);
            }
            Value::Object(out)
        }
        DataType::Null => Value::Null,
        other => return Err(ContextError::ColumnType { column: column.to_string(), data_type: other.to_string() }),
    })
}

/// The rows of one batch, each holding the named columns the batch carries. A named
/// column of a type no row value represents refuses; an unnamed one is never read.
pub fn batch_rows(batch: &RecordBatch, columns: &[&str]) -> Result<Vec<Map<String, Value>>> {
    let schema = batch.schema();
    let present: Vec<(&str, &dyn Array)> =
        columns.iter().filter_map(|c| schema.index_of(c).ok().map(|i| (*c, batch.column(i).as_ref()))).collect();
    (0..batch.num_rows())
        .map(|i| present.iter().map(|(c, a)| Ok((c.to_string(), cell(c, *a, i)?))).collect())
        .collect()
}

/// The rows `decl`'s table holds across its current file list, read from part footers.
pub fn table_row_count(store: &Store, decl: &TableDecl) -> Result<u64> {
    if store.try_schema(&decl.name)?.is_none() {
        return Ok(0);
    }
    let s = scan(store, decl, Bounds::default())?;
    let mut total = 0u64;
    for f in &s.files {
        total = total.saturating_add(store.parquet_row_count(&store.logical_path(f)?)?);
    }
    Ok(total)
}

/// Every row of `decl`'s table, handed to `each` one record batch at a time in file order,
/// holding the named columns (`run.select.parent-scan`).
pub fn table_row_batches(store: &Store, decl: &TableDecl, columns: &[&str], each: &mut dyn FnMut(Vec<Map<String, Value>>) -> Result<()>) -> Result<()> {
    if store.try_schema(&decl.name)?.is_none() {
        return Ok(());
    }
    let s = scan(store, decl, Bounds::default())?;
    for f in &s.files {
        store.each_parquet_batch(&store.logical_path(f)?, &mut |batch| each(batch_rows(&batch, columns)?))?;
    }
    Ok(())
}

/// Every row of `decl`'s table across its current file list, holding the named columns.
/// A table with no `schema.json` reads as no rows.
pub fn table_rows(store: &Store, decl: &TableDecl, columns: &[&str]) -> Result<Vec<Map<String, Value>>> {
    if store.try_schema(&decl.name)?.is_none() {
        return Ok(Vec::new());
    }
    let s = scan(store, decl, Bounds::default())?;
    let mut out = Vec::new();
    for f in &s.files {
        for batch in store.read_parquet(&store.logical_path(f)?)? {
            out.extend(batch_rows(&batch, columns)?);
        }
    }
    Ok(out)
}
