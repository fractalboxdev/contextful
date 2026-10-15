//! The SQL relation a table registers as: `read_parquet` over the explicit file list,
//! the key's dedup view, and the valid-time wrap.

use super::bound_time::{Bound, Bounds, Vintage};
use super::declare::TableDecl;
use super::reconcile::Column;
use super::StoreError;
use crate::run::derive::emit::SUPERSEDE_COLUMNS;
use crate::run::derive::task::{is_derive_key, TASK_VERSION};

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
    relation_with_encryption(decl, files, schema_columns, absent, valid_as_of, None)
}

/// The relation with a named in-memory Parquet footer key when its files are encrypted.
pub fn relation_with_encryption(
    decl: &TableDecl,
    files: &[String],
    schema_columns: &[Column],
    absent: &[Column],
    valid_as_of: Option<Bound>,
    encryption_key: Option<&str>,
) -> Result<String, StoreError> {
    relation_bounded(decl, files, schema_columns, absent, Bounds { valid_as_of, ..Bounds::default() }, encryption_key)
}

/// The relation under the row-narrowing bounds: `valid_as_of` over the declared pair and
/// `available_as_of` over the declared availability column. `as_of` selects `files`
/// before this point and is ignored here.
pub fn relation_bounded(
    decl: &TableDecl,
    files: &[String],
    schema_columns: &[Column],
    absent: &[Column],
    bounds: Bounds,
    encryption_key: Option<&str>,
) -> Result<String, StoreError> {
    let mut base = if files.is_empty() {
        zero_row(schema_columns)
    } else {
        let list: Vec<String> = files.iter().map(|f| literal(f)).collect();
        // Parquet holds no fixed-size list, so the engine reads a vector as a variable list;
        // the one cast the relation carries restores its dimension (`store.reconcile.half-width`).
        let vectors: Vec<String> = schema_columns
            .iter()
            .filter(|c| c.ty.holds_vector() && !absent.iter().any(|a| a.name == c.name))
            .map(|c| format!("CAST({} AS {}) AS {}", ident(&c.name), c.ty.sql(), ident(&c.name)))
            .collect();
        let replace = if vectors.is_empty() { String::new() } else { format!(" REPLACE ({})", vectors.join(", ")) };
        match encryption_key {
            Some(key) => {
                // DuckDB 1.5.5's Parquet metadata reuse omits the encrypted reader's utility.
                let source = list.iter().map(|file| format!("SELECT * FROM read_parquet({file}, hive_partitioning = false, encryption_config = {{footer_key: {}}})", literal(key))).collect::<Vec<_>>().join(" UNION ALL BY NAME ");
                if replace.is_empty() { source } else { format!("SELECT *{replace} FROM ({source})") }
            }
            None => format!("SELECT *{replace} FROM read_parquet([{}], union_by_name = true, hive_partitioning = false)", list.join(", ")),
        }
    };
    if !files.is_empty() && !absent.is_empty() {
        base = format!("{base} UNION ALL BY NAME {}", zero_row(absent));
    }
    // The availability filter sits beneath the key's dedup, so a key resolves among the
    // rows public at the bound (`store.available-time.available-as-of`).
    if let Some(b) = bounds.available_as_of {
        let available = decl.available_time.as_ref().ok_or_else(|| {
            StoreError::StoreAvailableTimeUndeclared(format!("table `{}` declares no `available_time`", decl.name))
        })?;
        let cmp = if b.inclusive { "<=" } else { "<" };
        base = format!("SELECT * FROM ({base}) WHERE {} {cmp} TIMESTAMPTZ {}", ident(&available.column), literal(&b.at.to_rfc3339_nanos()));
    }
    let mut rel = if decl.is_keyed() {
        // A table declaring valid time keeps one row per key and valid-time line, and a
        // vintage table one per availability instant beside them, as the fold does.
        let pk: Vec<String> = decl.dedup_line().iter().map(|k| ident(k)).collect();
        let mut order = vec![format!("{} DESC", ident(decl.order_by()))];
        for c in super::reserve::TIEBREAK.into_iter().filter(|c| *c != decl.order_by()) {
            order.push(format!("{} DESC", ident(c)));
        }
        let deduped = format!(
            "SELECT * EXCLUDE (__contextful_rn) FROM (SELECT *, ROW_NUMBER() OVER (PARTITION BY {} ORDER BY {}) AS __contextful_rn FROM ({base})) WHERE __contextful_rn = 1",
            pk.join(", "),
            order.join(", ")
        );
        match (&decl.available_time, decl.content_hash_column()) {
            (Some(available), Some(hash)) => vintage(decl, &deduped, &available.column, hash, bounds.vintage),
            _ => deduped,
        }
    } else {
        base
    };
    if is_derive_key(decl.primary_key()) && SUPERSEDE_COLUMNS.iter().all(|n| schema_columns.iter().any(|c| c.name == *n)) {
        let by_version = decl.retain_versions == Some(true) && schema_columns.iter().any(|c| c.name == TASK_VERSION);
        rel = unsuperseded(&rel, by_version);
    }
    if let Some(retention) = &decl.retain_rows {
        let seconds = decl.retain_rows_secs().map_err(|e| StoreError::StoreRetentionColumnInvalid(e.to_string()))?.unwrap_or_default();
        let nanos = u128::from(seconds) * 1_000_000_000;
        rel = format!(
            "SELECT * FROM ({rel}) WHERE epoch_ns({}) >= CAST(epoch_ns(CURRENT_TIMESTAMP) AS HUGEINT) - CAST({nanos} AS HUGEINT)",
            ident(&retention.column)
        );
    }

    if let Some(b) = bounds.valid_as_of {
        let vt = decl.valid_time.as_ref().ok_or_else(|| {
            StoreError::StoreValidTimeUndeclared(format!("table `{}` declares no valid-time pair", decl.name))
        })?;
        let at = format!("TIMESTAMPTZ {}", literal(&b.at.to_rfc3339_nanos()));
        // An exclusive bound asks about the instant just before it: a row starting at
        // the bound is not yet valid, and a row ending at it still is.
        let (from_cmp, to_cmp) = if b.inclusive { ("<=", ">") } else { ("<", ">=") };
        let mut pred = format!("{} {from_cmp} {at}", ident(&vt.from));
        if let Some(to) = &vt.to {
            pred = format!("{pred} AND ({} IS NULL OR {} {to_cmp} {at})", ident(to), ident(to));
        }
        rel = format!("SELECT * FROM ({rel}) WHERE {pred}");
    }
    Ok(rel)
}

/// One vintage per key and valid-time line out of `deduped`, which holds one row per
/// availability instant. A vintage whose content hash equals its predecessor's is no
/// restatement and drops out (`store.available-time.vintage-hash`); of the rest, the
/// latest instant answers, or the earliest under `vintage = "first"`
/// (`store.available-time.vintage-resolution`, `store.available-time.vintage-first`).
fn vintage(decl: &TableDecl, deduped: &str, available: &str, hash: &str, which: Vintage) -> String {
    let mut line: Vec<String> = decl.primary_key().iter().map(|k| ident(k)).collect();
    line.extend(decl.valid_time.as_ref().map(|vt| ident(&vt.from)));
    let line = line.join(", ");
    let (available, hash) = (ident(available), ident(hash));
    let restated = format!(
        "SELECT * EXCLUDE (__contextful_prior) FROM (SELECT *, LAG({hash}) OVER (PARTITION BY {line} ORDER BY {available}) AS __contextful_prior FROM ({deduped})) \
         WHERE __contextful_prior IS NULL OR __contextful_prior IS DISTINCT FROM {hash}"
    );
    let direction = match which {
        Vintage::Latest => "DESC",
        Vintage::First => "ASC",
    };
    format!(
        "SELECT * EXCLUDE (__contextful_vintage) FROM (SELECT *, ROW_NUMBER() OVER (PARTITION BY {line} ORDER BY {available} {direction}) AS __contextful_vintage FROM ({restated})) WHERE __contextful_vintage = 1"
    )
}

/// `rel` less a derive table's superseded rows: each unit's rows landed under another key
/// before its latest passage, `ok` or `empty` landing (`run.emit.stale-supersedes`). The
/// fold drops the same rows, so a read before and after it answers alike. On a table
/// retaining versions, a unit's rows yield only within their own `task_version`
/// (`run.emit.version-retained`).
fn unsuperseded(rel: &str, by_version: bool) -> String {
    let [unit, key, kind, status, at, run, seq] = SUPERSEDE_COLUMNS.map(ident);
    let version = ident(TASK_VERSION);
    let (partition, carry, join) = if by_version {
        (format!("{unit}, {version}"), format!(", {version} AS __version"), format!(" AND __d.{version} IS NOT DISTINCT FROM __c.__version"))
    } else {
        (unit.clone(), String::new(), String::new())
    };
    let latest = format!(
        "SELECT {unit} AS __unit, {key} AS __key, {at} AS __at, {run} AS __run, {seq} AS __seq{carry} FROM (\
         SELECT *, ROW_NUMBER() OVER (PARTITION BY {partition} ORDER BY {at} DESC, {run} DESC, {seq} DESC) AS __n FROM ({rel}) \
         WHERE {kind} IS DISTINCT FROM 'marker' OR {status} IN ('ok', 'empty')) WHERE __n = 1"
    );
    format!(
        "SELECT __d.* FROM ({rel}) AS __d LEFT JOIN ({latest}) AS __c ON __d.{unit} = __c.__unit{join} \
         WHERE __c.__unit IS NULL OR __d.{key} IS NOT DISTINCT FROM __c.__key OR __d.{at} > __c.__at \
         OR (__d.{at} = __c.__at AND (__d.{run} > __c.__run OR (__d.{run} = __c.__run AND __d.{seq} >= __c.__seq)))"
    )
}
