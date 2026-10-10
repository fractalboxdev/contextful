//! Canonical TOML (`topology.package.canonical-toml`): every table's keys in byte order,
//! tables after the values they nest in, scalars, keys and spacing as the format-preserving
//! editor writes them fresh, and every comment kept on the item it precedes or trails.

use contextful_core::surface::SurfaceError;
use contextful_snapshot::ControlError;
use toml_edit::{Array, Decor, DocumentMut, InlineTable, Item, RawString, Table, Value};

/// The canonical text of a configuration, or `ApplyValidationRefused` for text that is not
/// TOML (`surface.apply.validation`).
pub fn canonical(text: &str) -> Result<String, ControlError> {
    let mut doc: DocumentMut = text
        .parse()
        .map_err(|e| SurfaceError::ApplyValidationRefused(format!("the configuration is not TOML: {e}")))?;
    let mut position = 0;
    table(doc.as_table_mut(), &mut position, true);
    let trailing = comment_lines(doc.trailing());
    doc.set_trailing(if trailing.is_empty() { String::new() } else { format!("\n{trailing}") });
    let out = doc.to_string();
    Ok(format!("{}\n", out.trim_start_matches('\n').trim_end_matches('\n')))
}

fn raw(raw: Option<&RawString>) -> &str {
    raw.and_then(RawString::as_str).unwrap_or_default()
}

/// The comment lines of a prefix, each trimmed and ended by a newline.
fn comment_lines(prefix: &RawString) -> String {
    raw(Some(prefix)).lines().map(str::trim).filter(|l| l.starts_with('#')).map(|l| format!("{l}\n")).collect()
}

/// A trailing comment as ` # …`, or nothing.
fn trailing_comment(suffix: Option<&RawString>) -> String {
    match raw(suffix).find('#') {
        Some(at) => format!(" {}", raw(suffix)[at..].trim_end()),
        None => String::new(),
    }
}

fn has_comment(decor: &Decor) -> bool {
    raw(decor.prefix()).contains('#') || raw(decor.suffix()).contains('#')
}

/// A scalar rewritten in the editor's own representation, its decor untouched.
fn scalar(value: &mut Value) {
    match value {
        Value::String(f) => f.fmt(),
        Value::Integer(f) => f.fmt(),
        Value::Float(f) => f.fmt(),
        Value::Boolean(f) => f.fmt(),
        Value::Datetime(f) => f.fmt(),
        Value::Array(a) => array(a),
        Value::InlineTable(t) => inline(t),
    }
}

fn array(a: &mut Array) {
    let commented = a.iter().any(|v| has_comment(v.decor())) || raw(Some(a.trailing())).contains('#');
    for v in a.iter_mut() {
        scalar(v);
    }
    if !commented {
        a.fmt();
        a.set_trailing_comma(false);
        a.set_trailing("");
    }
}

fn inline(t: &mut InlineTable) {
    t.sort_values();
    let commented = t.iter().any(|(_, v)| has_comment(v.decor()));
    for (mut key, v) in t.iter_mut() {
        key.fmt();
        scalar(v);
    }
    if !commented {
        t.fmt();
    }
}

/// A key-value line: comments above the key, one space around `=`, a trailing comment.
fn entry(key: &mut toml_edit::KeyMut<'_>, value: &mut Value) {
    let above = comment_lines(key.leaf_decor().prefix().cloned().as_ref().unwrap_or(&RawString::default()));
    let after = trailing_comment(value.decor().suffix());
    key.fmt();
    key.leaf_decor_mut().set_prefix(above);
    key.leaf_decor_mut().set_suffix(" ");
    scalar(value);
    value.decor_mut().set_prefix(" ");
    value.decor_mut().set_suffix(after);
}

/// A table: its values sorted, then its subtables in key order, each header after a blank
/// line and the comments above it.
fn table(t: &mut Table, position: &mut usize, root: bool) {
    if !root {
        let above = comment_lines(t.decor().prefix().cloned().as_ref().unwrap_or(&RawString::default()));
        let after = trailing_comment(t.decor().suffix());
        t.decor_mut().set_prefix(format!("\n{above}"));
        t.decor_mut().set_suffix(after);
    }
    t.set_position(*position);
    *position += 1;
    t.sort_values();
    let mut keys: Vec<String> = t.iter().map(|(k, _)| k.to_owned()).collect();
    keys.sort();
    for name in &keys {
        if let Some((mut key, Item::Value(value))) = t.get_key_value_mut(name) {
            entry(&mut key, value);
        }
    }
    for name in &keys {
        let Some((mut key, item)) = t.get_key_value_mut(name) else { continue };
        match item {
            Item::Table(sub) => {
                key.fmt();
                table(sub, position, false);
            }
            Item::ArrayOfTables(list) => {
                key.fmt();
                for sub in list.iter_mut() {
                    table(sub, position, false);
                }
            }
            Item::Value(_) | Item::None => {}
        }
    }
}
