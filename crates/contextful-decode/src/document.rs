//! Markdown notes and plain text: a note's flat frontmatter, its first heading, and its
//! heading sections (`connector.source.document-grain`).
//!
//! Frontmatter is the block between a leading `---` line and the next `---` or `...` line.
//! It holds flat keys only: each value is a scalar, an inline `[a, b]` list or a block list
//! of `- item` lines. A nested map, a block scalar and a key carrying the reserved producer
//! prefix refuse the note (`connector.source.frontmatter-shape`). Headings are ATX headings
//! outside fenced code.

use contextful_core::connector::ConnectorError;
use contextful_core::run::{Failure, FailureTag};
use serde_json::Value;
use std::collections::BTreeSet;

/// The leading bytes of a compound-binary (OLE2) container: `.doc`, `.xls`, `.ppt` and
/// their kin (`connector.source.conversion-required`).
pub const COMPOUND_MAGIC: [u8; 8] = [0xD0, 0xCF, 0x11, 0xE0, 0xA1, 0xB1, 0x1A, 0xE1];

/// The prefix of the column namespace the engine reserves (`store.reserve.column-name`).
pub const RESERVED_PREFIX: &str = "_";

/// Whether `head` opens a compound-binary office container.
pub fn is_compound(head: &[u8]) -> bool {
    head.starts_with(&COMPOUND_MAGIC)
}

fn rejected(input: &str, line: usize, why: impl std::fmt::Display) -> Failure {
    Failure::deterministic(FailureTag::Permanent, ConnectorError::ConnectorFrontmatterRejected(format!("`{input}` frontmatter line {line}: {why}")).to_string())
}

/// One frontmatter value: a scalar's text, a list of scalars, or null for a key with none.
pub type Entry = (String, Value);

/// Split a note into its frontmatter entries, in declared order, and its body. A key in
/// `taken` names a column the source lands itself and refuses like a reserved key.
pub fn frontmatter<'a>(text: &'a str, input: &str, taken: &[&str]) -> Result<(Vec<Entry>, &'a str), Failure> {
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    let Some(rest) = text.strip_prefix("---\n").or_else(|| text.strip_prefix("---\r\n")) else {
        return Ok((Vec::new(), text));
    };
    let mut lines: Vec<&str> = Vec::new();
    let mut body: Option<&str> = None;
    let mut offset = 0usize;
    for raw in rest.split_inclusive('\n') {
        offset += raw.len();
        let line = raw.trim_end_matches(['\n', '\r']);
        if line == "---" || line == "..." {
            body = Some(&rest[offset..]);
            break;
        }
        lines.push(line);
    }
    let Some(body) = body else {
        return Err(rejected(input, 1, "the block opened by `---` never closes"));
    };
    let mut entries: Vec<Entry> = Vec::new();
    let mut seen = BTreeSet::new();
    let mut i = 0;
    while i < lines.len() {
        let n = i + 2;
        let line = lines[i];
        i += 1;
        if line.trim().is_empty() || line.trim_start().starts_with('#') {
            continue;
        }
        if line.starts_with([' ', '\t']) {
            return Err(rejected(input, n, "an indented line belongs to no key; a nested map is refused"));
        }
        let Some((key, value)) = line.split_once(':') else {
            return Err(rejected(input, n, "a line is `key: value`"));
        };
        let key = unquote(key.trim());
        if key.is_empty() {
            return Err(rejected(input, n, "a key is empty"));
        }
        if key.starts_with(RESERVED_PREFIX) {
            return Err(rejected(input, n, format!("key `{key}` carries the reserved producer prefix `{RESERVED_PREFIX}`")));
        }
        if taken.contains(&key.as_str()) {
            return Err(rejected(input, n, format!("key `{key}` names a column the source lands")));
        }
        if !seen.insert(key.clone()) {
            return Err(rejected(input, n, format!("key `{key}` is declared twice")));
        }
        let value = value.trim();
        let parsed = if value.is_empty() {
            // A value on the following indented lines: a block list, or a nested map.
            let mut items = Vec::new();
            while i < lines.len() && (lines[i].starts_with([' ', '\t', '-']) || lines[i].trim().is_empty()) {
                let item = lines[i].trim();
                i += 1;
                if item.is_empty() {
                    continue;
                }
                let Some(scalar) = item.strip_prefix('-').filter(|s| s.is_empty() || s.starts_with(' ')) else {
                    return Err(rejected(input, i + 1, format!("key `{key}` holds a nested map")));
                };
                items.push(Value::String(list_scalar(scalar.trim(), &key, input, i + 1)?));
            }
            if items.is_empty() {
                Value::Null
            } else {
                Value::Array(items)
            }
        } else if is_block_scalar(value) {
            return Err(rejected(input, n, format!("key `{key}` holds a block scalar `{value}`")));
        } else if value.starts_with('{') {
            return Err(rejected(input, n, format!("key `{key}` holds a nested map")));
        } else if let Some(inner) = value.strip_prefix('[') {
            let inner = inner.strip_suffix(']').ok_or_else(|| rejected(input, n, format!("key `{key}` opens a list it never closes")))?;
            let mut items = Vec::new();
            for item in split_list(inner) {
                items.push(Value::String(list_scalar(item.trim(), &key, input, n)?));
            }
            Value::Array(items)
        } else {
            Value::String(unquote(value))
        };
        entries.push((key, parsed));
    }
    Ok((entries, body))
}

/// `|` or `>` with optional chomping and indentation indicators.
fn is_block_scalar(value: &str) -> bool {
    let mut chars = value.chars();
    matches!(chars.next(), Some('|' | '>')) && chars.all(|c| c == '-' || c == '+' || c.is_ascii_digit())
}

fn list_scalar(item: &str, key: &str, input: &str, line: usize) -> Result<String, Failure> {
    if item.starts_with(['{', '[']) || (!item.starts_with(['"', '\'']) && item.contains(": ")) {
        return Err(rejected(input, line, format!("key `{key}` lists a nested value")));
    }
    Ok(unquote(item))
}

/// Split an inline list on commas outside quotes; an empty list has no items.
fn split_list(inner: &str) -> Vec<&str> {
    if inner.trim().is_empty() {
        return Vec::new();
    }
    let (mut out, mut start, mut quote) = (Vec::new(), 0usize, None::<char>);
    for (i, c) in inner.char_indices() {
        match (quote, c) {
            (None, '"' | '\'') => quote = Some(c),
            (Some(q), _) if c == q => quote = None,
            (None, ',') => {
                out.push(&inner[start..i]);
                start = i + 1;
            }
            _ => {}
        }
    }
    out.push(&inner[start..]);
    out
}

fn unquote(s: &str) -> String {
    for q in ['"', '\''] {
        if s.len() >= 2 && s.starts_with(q) && s.ends_with(q) {
            return s[1..s.len() - 1].to_string();
        }
    }
    s.to_string()
}

/// An ATX heading's level and text, outside fenced code.
fn heading(line: &str) -> Option<(usize, String)> {
    let indent = line.len() - line.trim_start_matches(' ').len();
    if indent > 3 {
        return None;
    }
    let t = &line[indent..];
    let level = t.len() - t.trim_start_matches('#').len();
    if !(1..=6).contains(&level) {
        return None;
    }
    let rest = &t[level..];
    if !(rest.is_empty() || rest.starts_with([' ', '\t'])) {
        return None;
    }
    let text = rest.trim().trim_end_matches('#').trim_end();
    Some((level, text.to_string()))
}

fn is_fence(line: &str) -> bool {
    let t = line.trim_start_matches(' ');
    line.len() - t.len() <= 3 && (t.starts_with("```") || t.starts_with("~~~"))
}

/// Each line of `body` with whether it opens a heading, fenced code excluded.
fn headings(body: &str) -> Vec<(&str, Option<(usize, String)>)> {
    let mut fenced = false;
    body.lines()
        .map(|line| {
            if is_fence(line) {
                fenced = !fenced;
                return (line, None);
            }
            (line, if fenced { None } else { heading(line) })
        })
        .collect()
}

/// The text of the note's first level-one heading.
pub fn title(body: &str) -> Option<String> {
    headings(body).into_iter().find_map(|(_, h)| h.filter(|(level, t)| *level == 1 && !t.is_empty()).map(|(_, t)| t))
}

/// A heading's page anchor: lowercased, letters, digits, `-` and `_` kept, spaces as `-`.
pub fn anchor(heading: &str) -> String {
    heading
        .trim()
        .to_lowercase()
        .chars()
        .filter_map(|c| match c {
            ' ' => Some('-'),
            c if c.is_alphanumeric() || c == '-' || c == '_' => Some(c),
            _ => None,
        })
        .collect()
}

/// One heading section of a note.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Section {
    /// The heading's text; `None` for the text before the first heading.
    pub heading: Option<String>,
    /// The heading's page anchor, a repeat suffixed `-1`, `-2`, ….
    pub anchor: Option<String>,
    /// The section's text, its heading line included, trimmed.
    pub text: String,
}

/// The note's heading sections in order: the non-blank text before the first heading, then
/// one section per heading running to the next.
pub fn sections(body: &str) -> Vec<Section> {
    let mut out: Vec<Section> = Vec::new();
    let mut current = Section { heading: None, anchor: None, text: String::new() };
    let mut used: BTreeSet<String> = BTreeSet::new();
    for (line, h) in headings(body) {
        if let Some((_, text)) = h {
            let done = std::mem::replace(&mut current, Section { heading: None, anchor: None, text: String::new() });
            if done.heading.is_some() || !done.text.trim().is_empty() {
                out.push(done);
            }
            let base = anchor(&text);
            let mut a = base.clone();
            let mut k = 0;
            while !used.insert(a.clone()) {
                k += 1;
                a = format!("{base}-{k}");
            }
            current = Section { heading: Some(text), anchor: Some(a), text: String::new() };
        }
        current.text.push_str(line);
        current.text.push('\n');
    }
    if current.heading.is_some() || !current.text.trim().is_empty() {
        out.push(current);
    }
    for s in &mut out {
        s.text = s.text.trim().to_string();
    }
    out
}
