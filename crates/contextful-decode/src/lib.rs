//! The one decoder set every source shares (`connector.source.body-format`): JSON, JSON
//! Lines, delimited text and a workbook. Input a decoder cannot read refuses whole, naming
//! the input and the position inside it.
//!
//! The package reaches no mediated-request crate and no network stack, so an offline or
//! sandboxed host links the decoders alone (`topology.package.decode-network-free`).

mod ooxml;
#[cfg(feature = "pdf")]
pub mod pdf;
pub mod workbook;

use contextful_core::connector::ConnectorError;
use contextful_core::run::ports::Row;
use contextful_core::run::{Failure, FailureTag, RunError};
use serde_json::Value;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format {
    Json,
    Jsonl,
    Csv,
    /// An Office Open XML workbook, one worksheet per read.
    Workbook,
}

impl Format {
    pub fn parse(s: &str) -> Result<Format, ConnectorError> {
        match s {
            "json" => Ok(Format::Json),
            "jsonl" => Ok(Format::Jsonl),
            "csv" => Ok(Format::Csv),
            "xlsx" => Ok(Format::Workbook),
            other => Err(ConnectorError::ConnectorFormatKeyRejected(format!("format `{other}` is none of json, jsonl, csv and xlsx"))),
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Format::Json => "json",
            Format::Jsonl => "jsonl",
            Format::Csv => "csv",
            Format::Workbook => "xlsx",
        }
    }
}

pub(crate) fn unreadable(input: &str, position: String, why: impl std::fmt::Display) -> Failure {
    Failure::deterministic(FailureTag::Permanent, RunError::PipelineUnreadableInput(format!("`{input}` at {position}: {why}")).to_string())
}

fn object(v: Value, input: &str, position: String) -> Result<Row, Failure> {
    match v {
        Value::Object(m) => Ok(m),
        other => Err(unreadable(input, position, format!("a record is an object, found {other}"))),
    }
}

/// Decode `body` into records, and the parsed JSON body a pagination pointer reads. A
/// workbook lands its first sheet; [`workbook::rows`] selects another.
pub fn decode(format: Format, body: &[u8], records: Option<&str>, input: &str) -> Result<(Vec<Row>, Option<Value>), Failure> {
    match format {
        Format::Json => {
            let v: Value = serde_json::from_slice(body).map_err(|e| unreadable(input, format!("line {} column {}", e.line(), e.column()), e))?;
            let list = match records {
                Some(p) => v.pointer(p).cloned().ok_or_else(|| unreadable(input, format!("pointer `{p}`"), "the record pointer names nothing"))?,
                None => v.clone(),
            };
            let items = match list {
                Value::Array(items) => items,
                other => return Err(unreadable(input, format!("pointer `{}`", records.unwrap_or("")), format!("records are an array, found {other}"))),
            };
            let rows = items.into_iter().enumerate().map(|(i, item)| object(item, input, format!("record {i}"))).collect::<Result<_, _>>()?;
            Ok((rows, Some(v)))
        }
        Format::Jsonl => {
            let text = std::str::from_utf8(body).map_err(|e| unreadable(input, format!("byte {}", e.valid_up_to()), "the body is not UTF-8"))?;
            let mut rows = Vec::new();
            for (i, line) in text.lines().enumerate().filter(|(_, l)| !l.trim().is_empty()) {
                let v: Value = serde_json::from_str(line).map_err(|e| unreadable(input, format!("line {} column {}", i + 1, e.column()), e))?;
                rows.push(object(v, input, format!("line {}", i + 1))?);
            }
            Ok((rows, None))
        }
        Format::Csv => {
            let text = std::str::from_utf8(body).map_err(|e| unreadable(input, format!("byte {}", e.valid_up_to()), "the body is not UTF-8"))?;
            Ok((csv(text, input)?, None))
        }
        Format::Workbook => Ok((workbook::rows(body, None, 0, input)?, None)),
    }
}

/// A delimited body's records under RFC 4180: a quoted field may hold commas, doubled
/// quotes and line breaks. Each record carries the line it starts on and its fields,
/// each with whether it was quoted; an empty line is no record.
/// One record: the line it starts on, and each field with whether it was quoted.
type Record = (usize, Vec<(String, bool)>);

fn records(text: &str, input: &str) -> Result<Vec<Record>, Failure> {
    let mut out = Vec::new();
    let mut fields: Vec<(String, bool)> = Vec::new();
    let (mut field, mut quoted, mut in_quotes) = (String::new(), false, false);
    let (mut line, mut record_line) = (1usize, 1usize);
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if in_quotes {
            match c {
                '"' if chars.peek() == Some(&'"') => {
                    chars.next();
                    field.push('"');
                }
                '"' => in_quotes = false,
                '\n' => {
                    line += 1;
                    field.push(c);
                }
                _ => field.push(c),
            }
            continue;
        }
        match c {
            '"' if field.is_empty() && !quoted => {
                quoted = true;
                in_quotes = true;
            }
            ',' => fields.push((std::mem::take(&mut field), std::mem::replace(&mut quoted, false))),
            '\r' if chars.peek() == Some(&'\n') => {}
            '\n' => {
                fields.push((std::mem::take(&mut field), std::mem::replace(&mut quoted, false)));
                let blank = fields.len() == 1 && fields[0].0.is_empty() && !fields[0].1;
                let done = std::mem::take(&mut fields);
                if !blank {
                    out.push((record_line, done));
                }
                line += 1;
                record_line = line;
            }
            _ => field.push(c),
        }
    }
    if in_quotes {
        return Err(unreadable(input, format!("line {record_line}"), "a quoted field is unclosed"));
    }
    if !field.is_empty() || quoted || !fields.is_empty() {
        fields.push((field, quoted));
        out.push((record_line, fields));
    }
    Ok(out)
}

/// A header record, then one row per record: every cell a string, an empty unquoted
/// field null. A leading UTF-8 byte-order mark is no part of the first column's name.
fn csv(text: &str, input: &str) -> Result<Vec<Row>, Failure> {
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    let mut recs = records(text, input)?.into_iter();
    let Some((_, header)) = recs.next() else { return Ok(Vec::new()) };
    let names: Vec<String> = header.into_iter().map(|(f, _)| f).collect();
    let mut rows = Vec::new();
    for (line, cells) in recs {
        if cells.len() > names.len() {
            return Err(unreadable(input, format!("line {line}"), format!("{} cells under a {}-column header", cells.len(), names.len())));
        }
        let mut row = Row::new();
        for (name, (cell, quoted)) in names.iter().zip(cells) {
            row.insert(name.clone(), if cell.is_empty() && !quoted { Value::Null } else { Value::String(cell) });
        }
        rows.push(row);
    }
    Ok(rows)
}
