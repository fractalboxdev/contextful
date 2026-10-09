//! A table's rows as JSON objects, projected to the columns a caller names.

use crate::support::{decl, Fixture};
use arrow_array::builder::{ListBuilder, StringBuilder};
use arrow_array::{
    ArrayRef, Date32Array, Date64Array, Decimal128Array, Float32Array, Int8Array, LargeStringArray, ListArray, RecordBatch,
    StringViewArray, TimestampMicrosecondArray, TimestampMillisecondArray, TimestampSecondArray, UInt64Array,
};
use contextful_context::rows::{batch_rows, table_rows};
use serde_json::json;
use std::sync::Arc;

fn batch(columns: Vec<(&str, ArrayRef)>) -> RecordBatch {
    RecordBatch::try_from_iter(columns).unwrap()
}

#[test]
fn every_timestamp_unit_text_integer_float_date_and_decimal_reads_as_a_value() {
    let b = batch(vec![
        ("ts_s", Arc::new(TimestampSecondArray::from(vec![1_700_000_000]).with_timezone("UTC")) as ArrayRef),
        ("ts_ms", Arc::new(TimestampMillisecondArray::from(vec![1_700_000_000_123])) as ArrayRef),
        ("ts_us", Arc::new(TimestampMicrosecondArray::from(vec![1_700_000_000_123_456])) as ArrayRef),
        ("large", Arc::new(LargeStringArray::from(vec!["doc-1"])) as ArrayRef),
        ("view", Arc::new(StringViewArray::from(vec!["media/a.wav"])) as ArrayRef),
        ("small", Arc::new(Int8Array::from(vec![-3])) as ArrayRef),
        ("big", Arc::new(UInt64Array::from(vec![u64::MAX])) as ArrayRef),
        ("ratio", Arc::new(Float32Array::from(vec![0.5])) as ArrayRef),
        ("day", Arc::new(Date32Array::from(vec![19_000])) as ArrayRef),
        ("day64", Arc::new(Date64Array::from(vec![1_641_600_000_000])) as ArrayRef),
        ("amount", Arc::new(Decimal128Array::from(vec![12_345]).with_precision_and_scale(10, 2).unwrap()) as ArrayRef),
    ]);
    let names = ["ts_s", "ts_ms", "ts_us", "large", "view", "small", "big", "ratio", "day", "day64", "amount"];
    let rows = batch_rows(&b, &names).unwrap();
    assert_eq!(
        serde_json::Value::Object(rows[0].clone()),
        json!({
            "ts_s": "2023-11-14T22:13:20.000000000Z",
            "ts_ms": "2023-11-14T22:13:20.123000000Z",
            "ts_us": "2023-11-14T22:13:20.123456000Z",
            "large": "doc-1",
            "view": "media/a.wav",
            "small": -3,
            "big": u64::MAX,
            "ratio": 0.5,
            "day": "2022-01-08",
            "day64": "2022-01-08",
            "amount": "123.45",
        })
    );
}

#[test]
fn a_requested_column_of_an_unsupported_type_fails_naming_it_and_an_unrequested_one_is_never_read() {
    let waited = arrow_array::DurationSecondArray::from(vec![5]);
    let b = batch(vec![("doc_id", Arc::new(LargeStringArray::from(vec!["d1"])) as ArrayRef), ("waited", Arc::new(waited) as ArrayRef)]);
    let err = batch_rows(&b, &["doc_id", "waited"]).unwrap_err().to_string();
    assert!(err.contains("`waited`") && err.contains("Duration"), "{err}");
    // A list is an array of its items (`read.respond.nested-values`).
    let mut list = ListBuilder::new(StringBuilder::new());
    list.values().append_value("x");
    list.append(true);
    let tags: ListArray = list.finish();
    let l = batch(vec![("tags", Arc::new(tags) as ArrayRef)]);
    assert_eq!(serde_json::Value::Object(batch_rows(&l, &["tags"]).unwrap()[0].clone()), json!({"tags": ["x"]}));
    let rows = batch_rows(&b, &["doc_id"]).unwrap();
    assert_eq!(serde_json::Value::Object(rows[0].clone()), json!({"doc_id": "d1"}));
}

#[test]
fn a_table_read_carries_only_the_named_columns() {
    let f = Fixture::new();
    let d = decl("name = \"documents\"");
    f.land(&d, "r1", json!([{"doc_id": "d1", "path": "a.txt", "extra": 1}]), "2026-01-01T00:00:00Z").unwrap();
    let rows = table_rows(&f.store, &d, &["doc_id", "path", "absent"]).unwrap();
    assert_eq!(serde_json::Value::Object(rows[0].clone()), json!({"doc_id": "d1", "path": "a.txt"}));
}

/// A table counts its rows from part footers and hands them over one record batch at a time, in the order a
/// whole read returns them.
#[test]
fn a_table_counts_its_rows_and_streams_them_batch_by_batch() {
    use contextful_context::rows::{table_row_batches, table_row_count};
    let f = Fixture::new();
    let d = decl("name = \"documents\"");
    assert_eq!(table_row_count(&f.store, &d).unwrap(), 0);
    f.land(&d, "r1", json!([{"doc_id": "d1", "path": "a.txt"}, {"doc_id": "d2", "path": "b.txt"}]), "2026-01-01T00:00:00Z").unwrap();
    f.land(&d, "r2", json!([{"doc_id": "d3", "path": "c.txt"}]), "2026-01-02T00:00:00Z").unwrap();
    assert_eq!(table_row_count(&f.store, &d).unwrap(), 3);
    let mut batches = Vec::new();
    table_row_batches(&f.store, &d, &["doc_id"], &mut |rows| {
        batches.push(rows);
        Ok(())
    })
    .unwrap();
    assert!(batches.len() >= 2, "each part is read apart: {batches:?}");
    let streamed: Vec<_> = batches.concat();
    assert_eq!(streamed, table_rows(&f.store, &d, &["doc_id"]).unwrap());
}
