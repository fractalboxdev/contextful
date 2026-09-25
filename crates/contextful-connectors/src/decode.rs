//! The one decoder set every source shares (`connector.source.body-format`): JSON, JSON
//! Lines and delimited text. Input a decoder cannot read refuses whole, naming the input
//! and the position inside it.

use contextful_core::connector::ConnectorError;
use contextful_core::run::ports::Row;
use contextful_core::run::{Failure, FailureTag, RunError};
use serde_json::Value;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format {
    Json,
    Jsonl,
    Csv,
}

impl Format {
    pub fn parse(s: &str) -> Result<Format, ConnectorError> {
        match s {
            "json" => Ok(Format::Json),
            "jsonl" => Ok(Format::Jsonl),
            "csv" => Ok(Format::Csv),
            other => Err(ConnectorError::ConnectorFormatKeyRejected(format!("format `{other}` is none of json, jsonl and csv"))),
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Format::Json => "json",
            Format::Jsonl => "jsonl",
            Format::Csv => "csv",
        }
    }
}

fn unreadable(input: &str, position: String, why: impl std::fmt::Display) -> Failure {
    Failure::deterministic(FailureTag::Permanent, RunError::PipelineUnreadableInput(format!("`{input}` at {position}: {why}")).to_string())
}

fn object(v: Value, input: &str, position: String) -> Result<Row, Failure> {
    match v {
        Value::Object(m) => Ok(m),
        other => Err(unreadable(input, position, format!("a record is an object, found {other}"))),
    }
}

/// Decode `body` into records, and the parsed JSON body a pagination pointer reads.
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
    }
}

/// One delimited field: its text, and whether it was quoted.
fn fields(line: &str, n: usize, input: &str) -> Result<Vec<(String, bool)>, Failure> {
    let mut out = Vec::new();
    let mut chars = line.chars().peekable();
    loop {
        let mut field = String::new();
        let mut quoted = false;
        if chars.peek() == Some(&'"') {
            quoted = true;
            chars.next();
            loop {
                match chars.next() {
                    Some('"') if chars.peek() == Some(&'"') => {
                        chars.next();
                        field.push('"');
                    }
                    Some('"') => break,
                    Some(c) => field.push(c),
                    None => return Err(unreadable(input, format!("line {n}"), "a quoted field is unclosed")),
                }
            }
        }
        while let Some(&c) = chars.peek() {
            if c == ',' {
                break;
            }
            field.push(c);
            chars.next();
        }
        out.push((field, quoted));
        if chars.next().is_none() {
            return Ok(out);
        }
    }
}

/// A header row, then one record per line: every cell a string, an empty unquoted field null.
fn csv(text: &str, input: &str) -> Result<Vec<Row>, Failure> {
    let mut lines = text.lines().enumerate().filter(|(_, l)| !l.is_empty());
    let Some((_, header)) = lines.next() else { return Ok(Vec::new()) };
    let names: Vec<String> = fields(header, 1, input)?.into_iter().map(|(f, _)| f).collect();
    let mut rows = Vec::new();
    for (i, line) in lines {
        let cells = fields(line, i + 1, input)?;
        if cells.len() > names.len() {
            return Err(unreadable(input, format!("line {}", i + 1), format!("{} cells under a {}-column header", cells.len(), names.len())));
        }
        let mut row = Row::new();
        for (name, (cell, quoted)) in names.iter().zip(cells) {
            row.insert(name.clone(), if cell.is_empty() && !quoted { Value::Null } else { Value::String(cell) });
        }
        rows.push(row);
    }
    Ok(rows)
}
