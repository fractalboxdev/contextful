//! A batch shaped like a read-face result: one column of each type an Iceberg target
//! cannot take as is.

use std::sync::Arc;

use anyhow::Result;
use arrow_array::builder::{FixedSizeListBuilder, Float32Builder};
use arrow_array::{
    Decimal256Array, LargeStringArray, RecordBatch, TimestampNanosecondArray, UInt32Array,
    UInt64Array,
};
use arrow_buffer::i256;
use arrow_schema::{DataType, Field, Schema, TimeUnit};

pub const DIM: i32 = 4;

pub fn batch(rows: usize) -> Result<RecordBatch> {
    let schema = Schema::new(vec![
        Field::new("id", DataType::UInt64, false),
        Field::new("hits", DataType::UInt32, false),
        Field::new(
            "embedding",
            DataType::FixedSizeList(Arc::new(Field::new("item", DataType::Float32, true)), DIM),
            true,
        ),
        Field::new("amount", DataType::Decimal256(38, 6), true),
        Field::new("label", DataType::LargeUtf8, true),
        Field::new("observed_at", DataType::Timestamp(TimeUnit::Nanosecond, Some("UTC".into())), false),
    ]);
    let mut emb = FixedSizeListBuilder::new(Float32Builder::new(), DIM);
    for i in 0..rows {
        for d in 0..DIM {
            emb.values().append_value(i as f32 + d as f32 / 10.0);
        }
        emb.append(true);
    }
    // 2026-01-01T00:00:00.000000001Z plus i microseconds and 1 ns: the ns digit survives
    // only in a nanosecond column.
    let base: i64 = 1_767_225_600_000_000_000;
    Ok(RecordBatch::try_new(
        Arc::new(schema),
        vec![
            Arc::new(UInt64Array::from_iter_values((0..rows as u64).map(|i| i + 1))),
            Arc::new(UInt32Array::from_iter_values((0..rows as u32).map(|i| u32::MAX - i))),
            Arc::new(emb.finish()),
            Arc::new(
                Decimal256Array::from_iter_values(
                    (0..rows).map(|i| i256::from_i128(i as i128 * 1_000_001)),
                )
                .with_precision_and_scale(38, 6)?,
            ),
            Arc::new(LargeStringArray::from_iter_values((0..rows).map(|i| format!("k{:03}", i % 1000)))),
            Arc::new(
                TimestampNanosecondArray::from_iter_values(
                    (0..rows as i64).map(|i| base + i * 1_000 + 1),
                )
                .with_timezone("UTC"),
            ),
        ],
    )?)
}
