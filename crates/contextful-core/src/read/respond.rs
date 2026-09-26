//! `read.respond`: the one response projection every transport serializes, the cell
//! encoding, and the row ceiling with its over-fetched probe row.

use crate::time::Instant;
use serde::Serialize;
use serde_json::{Map, Value};

/// Rows fetched past the row ceiling, whose presence sets `truncated`
/// (`read.respond.row-ceiling`).
pub const ROW_CEILING_OVERFETCH: u64 = 1;

/// Largest decimal width, in digits, encoded as a JSON number
/// (`read.respond.wide-number-shape`).
const NUMBER_DECIMAL_DIGITS: u8 = 15;

/// One result cell, typed by its column's SQL type. The adapter executing a statement
/// maps the engine's values onto these; the encoding below is the only one.
#[derive(Debug, Clone, PartialEq)]
pub enum Cell {
    Null,
    Boolean(bool),
    /// An integer of the given width in bits; signedness is in the value.
    Integer { value: i128, bits: u8 },
    /// An unsigned integer wider than `i128`.
    UnsignedHuge(u128),
    Float(f64),
    /// A decimal of `width` digits, `scale` of them fractional, as its scaled integer.
    Decimal { value: i128, width: u8, scale: u8 },
    Timestamp(Instant),
    /// Days since 1970-01-01.
    Date(i32),
    /// Microseconds since midnight.
    Time(i64),
    Interval { months: i32, days: i32, nanos: i64 },
    Text(String),
    Blob(Vec<u8>),
    /// An enum value, by its label.
    Enum(String),
    /// A list, struct, map or union, by its text form.
    Container(String),
}

impl Cell {
    /// The cell's JSON (`read.respond.cell-encoding`, `read.respond.wide-number-shape`).
    /// SQL NULL alone is `null`; the JSON type follows the column's SQL type, never the
    /// value on this page.
    pub fn to_json(&self) -> Value {
        match self {
            Cell::Null => Value::Null,
            Cell::Boolean(b) => Value::Bool(*b),
            Cell::Integer { value, bits } if *bits <= 32 => Value::from(*value as i64),
            Cell::Integer { value, .. } => Value::String(value.to_string()),
            Cell::UnsignedHuge(v) => Value::String(v.to_string()),
            Cell::Float(f) if f.is_nan() => Value::String("NaN".into()),
            Cell::Float(f) if f.is_infinite() => Value::String(if *f > 0.0 { "inf" } else { "-inf" }.into()),
            Cell::Float(f) => serde_json::Number::from_f64(*f).map_or(Value::Null, Value::Number),
            Cell::Decimal { value, width, scale } => {
                let text = decimal_text(*value, *scale);
                if *width <= NUMBER_DECIMAL_DIGITS {
                    text.parse::<f64>().ok().and_then(serde_json::Number::from_f64).map_or(Value::String(text), Value::Number)
                } else {
                    Value::String(text)
                }
            }
            Cell::Timestamp(t) => Value::String(t.to_rfc3339()),
            Cell::Date(days) => Value::String(date_text(*days)),
            Cell::Time(micros) => Value::String(time_text(*micros)),
            Cell::Interval { months, days, nanos } => Value::String(duration_text(*months, *days, *nanos)),
            Cell::Text(s) | Cell::Enum(s) | Cell::Container(s) => Value::String(s.clone()),
            Cell::Blob(b) => Value::String(b.iter().map(|x| format!("\\x{x:02X}")).collect()),
        }
    }
}

fn decimal_text(value: i128, scale: u8) -> String {
    if scale == 0 {
        return value.to_string();
    }
    let digits = value.unsigned_abs().to_string();
    let scale = usize::from(scale);
    let padded = format!("{digits:0>width$}", width = scale + 1);
    let (int, frac) = padded.split_at(padded.len() - scale);
    format!("{}{int}.{frac}", if value < 0 { "-" } else { "" })
}

fn date_text(days: i32) -> String {
    let t = time::OffsetDateTime::UNIX_EPOCH + time::Duration::days(i64::from(days));
    format!("{:04}-{:02}-{:02}", t.year(), u8::from(t.month()), t.day())
}

fn time_text(micros: i64) -> String {
    let (secs, frac) = (micros.div_euclid(1_000_000), micros.rem_euclid(1_000_000));
    let base = format!("{:02}:{:02}:{:02}", secs / 3600, (secs / 60) % 60, secs % 60);
    if frac == 0 {
        base
    } else {
        format!("{base}.{frac:06}")
    }
}

/// An interval as an ISO-8601 duration: `P<m>M<d>DT<s>S`, zero as `PT0S`.
fn duration_text(months: i32, days: i32, nanos: i64) -> String {
    let mut out = String::from("P");
    if months != 0 {
        out.push_str(&format!("{months}M"));
    }
    if days != 0 {
        out.push_str(&format!("{days}D"));
    }
    if nanos != 0 {
        let secs = nanos / 1_000_000_000;
        let frac = (nanos % 1_000_000_000).unsigned_abs();
        let sign = if nanos < 0 && secs == 0 { "-" } else { "" };
        if frac == 0 {
            out.push_str(&format!("T{sign}{secs}S"));
        } else {
            let frac = format!("{frac:09}");
            out.push_str(&format!("T{sign}{secs}.{}S", frac.trim_end_matches('0')));
        }
    }
    if out == "P" {
        out.push_str("T0S");
    }
    out
}

/// The one response projection (`read.respond.one-projection`): `columns`, `rows`,
/// `truncated`, and each optional `contextful.*` block present only where it applies.
/// No type list rides beside the rows (`read.respond.type-is-the-cell`).
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Response {
    pub columns: Vec<String>,
    pub rows: Vec<Vec<Value>>,
    pub truncated: bool,
    #[serde(flatten)]
    pub blocks: Map<String, Value>,
}

impl Response {
    /// A response over `fetched` rows, read with an over-fetch of one past `ceiling`.
    /// `truncated` is set exactly when the probe row is present, and the probe row is not
    /// delivered (`read.respond.truncation-is-exact`). Fewer rows, zero included, is an
    /// ordinary success (`read.respond.zero-rows-is-success`).
    pub fn cut(columns: Vec<String>, mut fetched: Vec<Vec<Value>>, ceiling: Option<u64>) -> Response {
        let truncated = match ceiling {
            Some(c) => {
                let c = usize::try_from(c).unwrap_or(usize::MAX);
                let probe = fetched.len() > c;
                fetched.truncate(c);
                probe
            }
            None => false,
        };
        Response { columns, rows: fetched, truncated, blocks: Map::new() }
    }

    /// How many rows to read for `ceiling`: the ceiling plus the probe row.
    pub fn fetch_count(ceiling: Option<u64>) -> Option<u64> {
        ceiling.map(|c| c.saturating_add(ROW_CEILING_OVERFETCH))
    }

    /// Attach a `contextful.<name>` block.
    pub fn with_block(mut self, name: &str, block: Value) -> Response {
        self.blocks.insert(format!("contextful.{name}"), block);
        self
    }

    pub fn to_json(&self) -> Value {
        serde_json::to_value(self).expect("a response serializes")
    }
}

/// What rides under `internals: true` alone (`read.respond.internals-opt-in`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Internals {
    pub sql: String,
    pub engine: &'static str,
    pub limit: Option<u64>,
    pub row_count: u64,
    pub elapsed_ms: u64,
}
