//! A table's rows as JSON objects, read off its current file list.

use crate::error::Result;
use crate::parquet_io;
use crate::scan::scan;
use crate::store::Store;
use arrow_array::cast::AsArray;
use arrow_array::types::{Float64Type, Int32Type, Int64Type, TimestampNanosecondType};
use arrow_array::Array;
use arrow_schema::DataType;
use contextful_core::store::bound_time::Bounds;
use contextful_core::store::declare::TableDecl;
use contextful_core::time::Instant;
use serde_json::{Map, Value};

fn cell(array: &dyn Array, i: usize) -> Value {
    if array.is_null(i) {
        return Value::Null;
    }
    match array.data_type() {
        DataType::Utf8 => Value::String(array.as_string::<i32>().value(i).to_string()),
        DataType::Int64 => Value::from(array.as_primitive::<Int64Type>().value(i)),
        DataType::Int32 => Value::from(array.as_primitive::<Int32Type>().value(i)),
        DataType::Float64 => Value::from(array.as_primitive::<Float64Type>().value(i)),
        DataType::Boolean => Value::Bool(array.as_boolean().value(i)),
        DataType::Timestamp(_, _) => Instant::from_unix_nanos(i128::from(array.as_primitive::<TimestampNanosecondType>().value(i)))
            .map(|t| Value::String(t.to_rfc3339()))
            .unwrap_or(Value::Null),
        _ => Value::Null,
    }
}

/// Every row of `decl`'s table across its current file list, each column as JSON. A table
/// with no `schema.json` reads as no rows.
pub fn table_rows(store: &Store, decl: &TableDecl) -> Result<Vec<Map<String, Value>>> {
    if store.try_schema(&decl.name)?.is_none() {
        return Ok(Vec::new());
    }
    let s = scan(store, decl, Bounds::default())?;
    let mut out = Vec::new();
    for f in &s.files {
        for batch in parquet_io::read(&store.root().join(f))? {
            let schema = batch.schema();
            for i in 0..batch.num_rows() {
                let mut row = Map::new();
                for (c, field) in batch.columns().iter().zip(schema.fields()) {
                    row.insert(field.name().clone(), cell(c.as_ref(), i));
                }
                out.push(row);
            }
        }
    }
    Ok(out)
}
