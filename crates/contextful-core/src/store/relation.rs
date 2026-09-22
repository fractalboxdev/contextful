//! The SQL relation a table registers as: `read_parquet` over the explicit file list,
//! the key's dedup view, and the valid-time wrap.

use super::bound_time::Bound;
use super::declare::TableDecl;
use super::reconcile::Column;
use super::StoreError;

/// Double-quote an identifier.
pub fn ident(name: &str) -> String {
    format!("\"{}\"", name.replace('"', "\"\""))
}

/// Single-quote a string literal.
pub fn literal(s: &str) -> String {
    format!("'{}'", s.replace('\'', "''"))
}

/// A zero-row relation over `columns`, each a literal null of its type.
fn zero_row(columns: &[Column]) -> String {
    let cols: Vec<String> =
        columns.iter().map(|c| format!("CAST(NULL AS {}) AS {}", c.ty.sql(), ident(&c.name))).collect();
    format!("SELECT {} WHERE false", cols.join(", "))
}

/// The table's FROM-source. `files` are absolute and sorted; `absent` holds each schema
/// column no listed file carries, which enters through a zero-row branch rather than
/// being invented by the scan (`store.reconcile.no-invented-column`). With no files the
/// table registers as a zero-row relation over its columns (`store.declare.empty-run`).
pub fn relation(
    decl: &TableDecl,
    files: &[String],
    schema_columns: &[Column],
    absent: &[Column],
    valid_as_of: Option<Bound>,
) -> Result<String, StoreError> {
    let mut base = if files.is_empty() {
        zero_row(schema_columns)
    } else {
        let list: Vec<String> = files.iter().map(|f| literal(f)).collect();
        format!("SELECT * FROM read_parquet([{}], union_by_name = true, hive_partitioning = false)", list.join(", "))
    };
    if !files.is_empty() && !absent.is_empty() {
        base = format!("{base} UNION ALL BY NAME {}", zero_row(absent));
    }

    let mut rel = if decl.is_keyed() {
        let pk: Vec<String> = decl.primary_key().iter().map(|k| ident(k)).collect();
        let mut order = vec![format!("{} DESC", ident(decl.order_by()))];
        if decl.order_by() != super::reserve::INGESTED_AT {
            order.push(format!("{} DESC", ident(super::reserve::INGESTED_AT)));
        }
        format!(
            "SELECT * EXCLUDE (__contextful_rn) FROM (SELECT *, ROW_NUMBER() OVER (PARTITION BY {} ORDER BY {}) AS __contextful_rn FROM ({base})) WHERE __contextful_rn = 1",
            pk.join(", "),
            order.join(", ")
        )
    } else {
        base
    };

    if let Some(b) = valid_as_of {
        let vt = decl.valid_time.as_ref().ok_or_else(|| {
            StoreError::StoreValidTimeUndeclared(format!("table `{}` declares no valid-time pair", decl.name))
        })?;
        let at = format!("TIMESTAMPTZ {}", literal(&b.at.to_rfc3339_nanos()));
        let from_cmp = if b.inclusive { "<=" } else { "<" };
        let mut pred = format!("{} {from_cmp} {at}", ident(&vt.from));
        if let Some(to) = &vt.to {
            pred = format!("{pred} AND ({} IS NULL OR {} > {at})", ident(to), ident(to));
        }
        rel = format!("SELECT * FROM ({rel}) WHERE {pred}");
    }
    Ok(rel)
}
