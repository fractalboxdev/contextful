//! The type policy: each Arrow type the read face emits maps to one Iceberg type, is cast
//! losslessly, or is refused before any file is written.

use std::collections::HashMap;
use std::sync::Arc;

use anyhow::{bail, Result};
use arrow_array::{Array, RecordBatch};
use arrow_schema::{DataType, Field, Schema, TimeUnit};
use iceberg::arrow::{arrow_schema_to_schema_auto_assign_ids, schema_to_arrow_schema};
use iceberg::spec::FormatVersion;

pub struct Plan {
    pub iceberg_schema: iceberg::spec::Schema,
    pub properties: HashMap<String, String>,
}

/// The Arrow type an export column takes, or the refusal.
fn target(field: &Field, version: FormatVersion, props: &mut HashMap<String, String>) -> Result<DataType> {
    Ok(match field.data_type() {
        DataType::UInt8 | DataType::UInt16 => DataType::Int32,
        DataType::UInt32 => DataType::Int64,
        // No Iceberg integer holds 2^64-1; decimal(20,0) holds every u64 exactly.
        DataType::UInt64 => DataType::Decimal128(20, 0),
        DataType::Decimal256(p, s) if *p <= 38 => DataType::Decimal128(*p, *s),
        DataType::Decimal256(p, _) => bail!("{}: decimal precision {p} exceeds Iceberg's 38", field.name()),
        DataType::LargeUtf8 | DataType::Utf8View => DataType::Utf8,
        DataType::LargeBinary | DataType::BinaryView => DataType::Binary,
        DataType::FixedSizeList(item, n) => {
            // Iceberg has no fixed-size list in v2 or v3; the width survives as a property.
            props.insert(format!("contextful.fixed-size-list.{}", field.name()), n.to_string());
            let inner = target(item, version, props)?;
            DataType::List(Arc::new(Field::new("element", inner, item.is_nullable())))
        }
        DataType::Timestamp(TimeUnit::Nanosecond, tz) => match version {
            FormatVersion::V3 => DataType::Timestamp(TimeUnit::Nanosecond, tz.clone()),
            // A declared, lossy fallback for readers without v3: truncate to microseconds.
            _ if std::env::var("NS_TO_US").is_ok_and(|v| v == "1") => {
                DataType::Timestamp(TimeUnit::Microsecond, tz.clone())
            }
            _ => bail!("{}: nanosecond timestamp needs format v3 (timestamp_ns)", field.name()),
        },
        DataType::Timestamp(TimeUnit::Millisecond | TimeUnit::Second, tz) => {
            DataType::Timestamp(TimeUnit::Microsecond, tz.clone())
        }
        other => other.clone(),
    })
}

pub fn plan(source: &Schema, version: FormatVersion) -> Result<Plan> {
    let mut properties = HashMap::new();
    let mut fields = Vec::new();
    for f in source.fields() {
        let t = target(f, version, &mut properties)?;
        fields.push(Field::new(f.name(), t, f.is_nullable()));
    }
    let iceberg_schema = arrow_schema_to_schema_auto_assign_ids(&Schema::new(fields))?;
    Ok(Plan { iceberg_schema, properties })
}

/// Casts the source batch to the table's current schema, whose Arrow form carries the
/// field ids the Parquet writer stamps into each column.
pub fn cast(source: &RecordBatch, table: &iceberg::table::Table) -> Result<RecordBatch> {
    let schema = Arc::new(schema_to_arrow_schema(table.metadata().current_schema())?);
    let mut cols = Vec::new();
    for (i, f) in schema.fields().iter().enumerate() {
        let col = source.column_by_name(f.name()).unwrap_or_else(|| source.column(i));
        let cast = arrow_cast::cast(col, f.data_type())?;
        // The FixedSizeList→List cast drops the element field's id metadata; reattach it.
        let cast = match f.data_type() {
            DataType::List(element) => {
                let l = cast.as_any().downcast_ref::<arrow_array::ListArray>().expect("list");
                Arc::new(arrow_array::ListArray::new(
                    element.clone(),
                    l.offsets().clone(),
                    l.values().clone(),
                    l.nulls().cloned(),
                )) as arrow_array::ArrayRef
            }
            _ => cast,
        };
        cols.push(cast);
    }
    Ok(RecordBatch::try_new(schema, cols)?)
}

/// Prints iceberg-rust 0.10.1's own Arrow→Iceberg verdict next to the policy's, per type.
pub fn report() -> Result<()> {
    let item = Arc::new(Field::new("item", DataType::Float32, true));
    let cases = [
        DataType::UInt32,
        DataType::UInt64,
        DataType::FixedSizeList(item, 4),
        DataType::Decimal256(38, 6),
        DataType::Decimal256(50, 10),
        DataType::LargeUtf8,
        DataType::Timestamp(TimeUnit::Nanosecond, None),
        DataType::Timestamp(TimeUnit::Nanosecond, Some("UTC".into())),
    ];
    for dt in cases {
        let f = Field::new("c", dt.clone(), true);
        let native = match arrow_schema_to_schema_auto_assign_ids(&Schema::new(vec![f.clone()])) {
            Ok(s) => format!("{}", s.as_struct().fields()[0].field_type),
            Err(e) => format!("error: {}", e.message()),
        };
        let verdict = |v: FormatVersion| {
            let mut p = HashMap::new();
            match target(&f, v, &mut p) {
                Ok(t) => match arrow_schema_to_schema_auto_assign_ids(&Schema::new(vec![Field::new("c", t, true)])) {
                    Ok(s) => format!("{}", s.as_struct().fields()[0].field_type),
                    Err(e) => format!("error: {}", e.message()),
                },
                Err(e) => format!("refuse: {e}"),
            }
        };
        let (v2, v3) = (verdict(FormatVersion::V2), verdict(FormatVersion::V3));
        println!("{dt}\n  iceberg-rust: {native}\n  policy v2:    {v2}\n  policy v3:    {v3}");
    }
    Ok(())
}
