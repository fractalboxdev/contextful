//! `read.respond`: cell encoding, number shape and the probe row.

use super::at;
use contextful_core::read::respond::{Cell, Response, ROW_CEILING_OVERFETCH};
use serde_json::{json, Value};

/// SQL NULL is JSON `null` and nothing else is. Non-finite floats are `"NaN"`, `"inf"`, `"-inf"`; temporal values are ISO-8601 strings, intervals ISO-8601 durations; an enum is its label; a union is its text form.
// spec: read.respond.cell-encoding@96f082b6
#[test]
fn cells_encode_by_their_sql_type() {
    assert_eq!(Cell::Null.to_json(), Value::Null);
    assert_eq!(Cell::Float(f64::NAN).to_json(), json!("NaN"));
    assert_eq!(Cell::Float(f64::INFINITY).to_json(), json!("inf"));
    assert_eq!(Cell::Float(f64::NEG_INFINITY).to_json(), json!("-inf"));
    assert_eq!(Cell::Timestamp(at("2026-02-14T09:31:00Z")).to_json(), json!("2026-02-14T09:31:00Z"));
    assert_eq!(Cell::Date(20498).to_json(), json!("2026-02-14"));
    assert_eq!(Cell::Time(34_260_500_000).to_json(), json!("09:31:00.500000"));
    assert_eq!(Cell::Interval { months: 14, days: 3, nanos: 90_500_000_000 }.to_json(), json!("P14M3DT90.5S"));
    assert_eq!(Cell::Interval { months: 0, days: 0, nanos: 0 }.to_json(), json!("PT0S"));
    assert_eq!(Cell::Enum("shipped".into()).to_json(), json!("shipped"));
    assert_eq!(Cell::Container("1".into()).to_json(), json!("1"));
    // A list, struct and map are arrays and objects of their own cells.
    let nested = Cell::Struct(vec![
        ("tags".into(), Cell::List(vec![Cell::Text("a".into()), Cell::Null])),
        ("attrs".into(), Cell::Map(vec![(Cell::Text("k".into()), Cell::Float(f64::NAN)), (Cell::Integer { value: 7, bits: 64 }, Cell::Boolean(true))])),
    ]);
    assert_eq!(nested.to_json(), json!({"tags": ["a", null], "attrs": {"k": "NaN", "7": true}}));
    // Empty text and a false boolean are values, not nulls.
    assert_eq!(Cell::Text(String::new()).to_json(), json!(""));
    assert_eq!(Cell::Boolean(false).to_json(), json!(false));
}

/// Binary is padded base64, and a fixed-size float array, a vector column included, is a JSON array holding each element as a float cell.
// spec: read.respond.bytes-and-vectors@59c872e7
#[test]
fn bytes_are_base64_and_vectors_are_number_arrays() {
    assert_eq!(Cell::Blob(vec![0xAA, 0x01]).to_json(), json!("qgE="));
    assert_eq!(Cell::Blob(vec![0xFF, 0xEE, 0xDD]).to_json(), json!("/+7d"));
    assert_eq!(Cell::Blob(Vec::new()).to_json(), json!(""));
    assert_eq!(Cell::Vector(vec![0.5, -1.0, 0.0]).to_json(), json!([0.5, -1.0, 0.0]));
    // Each element encodes as a float cell does, a non-finite one included.
    assert_eq!(Cell::Vector(vec![f64::NAN, f64::INFINITY]).to_json(), json!(["NaN", "inf"]));
}

/// A column's JSON encoding follows its SQL type alone: integers of 32 bits or fewer, finite floats and decimals of 15 digits or fewer are numbers; wider integers and decimals are exact decimal strings.
// spec: read.respond.wide-number-shape@71ca709f
#[test]
fn wide_numbers_are_exact_strings_whatever_their_value() {
    assert_eq!(Cell::Integer { value: 1299, bits: 32 }.to_json(), json!(1299));
    assert_eq!(Cell::Integer { value: u32::MAX.into(), bits: 32 }.to_json(), json!(4294967295u64));
    // A small value in a wide column is still a string, so the column's shape is fixed.
    assert_eq!(Cell::Integer { value: 1299, bits: 64 }.to_json(), json!("1299"));
    assert_eq!(Cell::Integer { value: 9_007_199_254_740_993, bits: 64 }.to_json(), json!("9007199254740993"));
    assert_eq!(Cell::UnsignedHuge(u128::MAX).to_json(), json!(u128::MAX.to_string()));
    assert_eq!(Cell::Float(0.25).to_json(), json!(0.25));
    assert_eq!(Cell::Decimal { value: 12345, width: 15, scale: 2 }.to_json(), json!(123.45));
    assert_eq!(Cell::Decimal { value: -5, width: 18, scale: 3 }.to_json(), json!("-0.005"));
    assert_eq!(Cell::Decimal { value: 7, width: 18, scale: 0 }.to_json(), json!("7"));
}

/// No separate type list rides the envelope; a cell's JSON type is the declaration.
// spec: read.respond.type-is-the-cell@90d506a7
#[test]
fn the_envelope_carries_no_type_list() {
    let r = Response::cut(vec!["order_id".into(), "total_cents".into()], vec![vec![json!("A-1"), json!("1299")]], None);
    let v = r.to_json();
    let keys: Vec<&str> = v.as_object().unwrap().keys().map(String::as_str).collect();
    assert_eq!(keys, ["columns", "rows", "truncated"]);
}

/// `truncated` is set exactly when the over-fetched probe row is present, never by comparing a returned count against a requested limit.
// spec: read.respond.truncation-is-exact@3ebf2e57
#[test]
fn truncation_follows_the_probe_row() {
    assert_eq!(ROW_CEILING_OVERFETCH, 1);
    assert_eq!(Response::fetch_count(Some(2)), Some(3));
    let row = |i: i64| vec![json!(i)];
    let probe = Response::cut(vec!["n".into()], vec![row(1), row(2), row(3)], Some(2));
    assert!(probe.truncated);
    assert_eq!(probe.rows, [row(1), row(2)]);
    // Exactly the ceiling, with no probe row, is complete.
    let exact = Response::cut(vec!["n".into()], vec![row(1), row(2)], Some(2));
    assert!(!exact.truncated);
    assert_eq!(exact.rows.len(), 2);
}

/// Fewer rows than the requested limit, zero included, is a success.
// spec: read.respond.zero-rows-is-success@d7b62901
#[test]
fn zero_rows_is_an_ordinary_response() {
    let r = Response::cut(vec!["n".into()], Vec::new(), Some(10));
    assert_eq!(r.to_json(), json!({ "columns": ["n"], "rows": [], "truncated": false }));
}
