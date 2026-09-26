//! A guest batch crosses as Arrow IPC stream bytes (`connector.export.batch-encoding`);
//! the host reads them into the rows the run path lands.

use arrow_array::cast::AsArray;
use arrow_array::types::{Float64Type, Int32Type, Int64Type, TimestampMillisecondType};
use arrow_array::{Array, RecordBatch};
use arrow_ipc::reader::StreamReader;
use arrow_schema::{DataType, TimeUnit};
use contextful_core::run::ports::Row;
use contextful_core::run::{Failure, FailureTag};
use serde_json::Value;

fn incompatible(m: String) -> Failure {
    Failure::new(FailureTag::SchemaIncompatible, m)
}

/// Read every row of one IPC stream. A `bytes` column lands as lowercase hex, a
/// millisecond timestamp as epoch milliseconds, and a null as null.
pub fn rows(ipc: &[u8]) -> Result<Vec<Row>, Failure> {
    let reader = StreamReader::try_new(std::io::Cursor::new(ipc), None).map_err(|e| incompatible(format!("a batch is not Arrow IPC: {e}")))?;
    let mut out = Vec::new();
    for batch in reader {
        let batch = batch.map_err(|e| incompatible(format!("a batch is not Arrow IPC: {e}")))?;
        append(&batch, &mut out)?;
    }
    Ok(out)
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
        DataType::Binary => Value::String(col.as_binary::<i32>().value(i).iter().map(|b| format!("{b:02x}")).collect()),
        DataType::Timestamp(TimeUnit::Millisecond, _) => Value::from(col.as_primitive::<TimestampMillisecondType>().value(i)),
        other => return Err(incompatible(format!("column `{name}` is Arrow {other}, outside the connector taxonomy"))),
    })
}
