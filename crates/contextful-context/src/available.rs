//! `store.available-time` at landing: each row's availability instant, resolved from the
//! source before the batch reconciles, and the flags only the engine sets.

use crate::land::Batch;
use crate::store::Store;
use crate::{ContextError, Result};
use contextful_core::store::declare::{AvailableTime, TableDecl};
use contextful_core::store::reconcile::ColumnType;
use contextful_core::store::reserve::{AVAILABILITY_FLAGS, AVAILABLE_IMPUTED, AVAILABLE_RECONSTRUCTED};
use contextful_core::time::Instant;
use serde_json::{Map, Value};
use std::collections::BTreeMap;

/// The latest available vintage for each key and valid-time line.
pub(crate) struct VintageHistory {
    latest: BTreeMap<String, (String, Value)>,
    columns: Vec<String>,
    valid_from: Option<String>,
    available: String,
    hash: String,
}

impl VintageHistory {
    pub(crate) fn load(store: &Store, decl: &TableDecl, before_commit_seq: Option<i64>) -> Result<Option<Self>> {
        if !decl.is_vintage() { return Ok(None) }
        let available = decl.available_time.as_ref().expect("a vintage has availability time").column.clone();
        let hash = decl.content_hash_column().expect("a vintage has a content hash").to_string();
        let mut columns = decl.primary_key().to_vec();
        columns.extend(decl.valid_time.as_ref().map(|valid| valid.from.clone()));
        let valid_from = decl.valid_time.as_ref().map(|valid| valid.from.clone());
        let mut history = Self { latest: BTreeMap::new(), columns, valid_from, available, hash };
        let mut owned = history.columns.clone();
        owned.extend([history.available.clone(), history.hash.clone(), "_commit_seq".into()]);
        let names: Vec<&str> = owned.iter().map(String::as_str).collect();
        for row in crate::rows::table_rows(store, decl, &names)? {
            if before_commit_seq.is_some_and(|seq| row.get("_commit_seq").and_then(Value::as_i64).is_none_or(|held| held >= seq)) { continue }
            history.observe(&row);
        }
        Ok(Some(history))
    }

    fn key(&self, row: &Map<String, Value>) -> String {
        let values: Vec<Value> = self.columns.iter().map(|name| {
            let value = row.get(name).cloned().unwrap_or(Value::Null);
            if self.valid_from.as_deref() != Some(name.as_str()) { return value }
            let Value::String(raw) = &value else { return value };
            let text = if raw.len() == 10 { format!("{raw}T00:00:00Z") } else { raw.clone() };
            Instant::parse(&text).map(|at| Value::String(at.to_rfc3339_nanos())).unwrap_or(value)
        }).collect();
        serde_json::to_string(&values).expect("key values serialize")
    }

    pub(crate) fn observe(&mut self, row: &Map<String, Value>) {
        let (Some(Value::String(at)), Some(hash)) = (row.get(&self.available), row.get(&self.hash)) else { return };
        let key = self.key(row);
        if self.latest.get(&key).is_none_or(|(latest, _)| at >= latest) {
            self.latest.insert(key, (at.clone(), hash.clone()));
        }
    }

    pub(crate) fn observe_part(&mut self, store: &Store, path: &std::path::Path) -> Result<()> {
        if !path.is_file() { return Ok(()) }
        let mut owned = self.columns.clone();
        owned.extend([self.available.clone(), self.hash.clone()]);
        let names: Vec<&str> = owned.iter().map(String::as_str).collect();
        for batch in store.read_parquet(path)? {
            for row in crate::rows::batch_rows(&batch, &names)? { self.observe(&row) }
        }
        Ok(())
    }

    /// A repeated content hash adds no vintage even when its source date precedes the
    /// currently latest availability instant.
    pub(crate) fn mask(&mut self, rows: &[Map<String, Value>]) -> Vec<bool> {
        let mut order: Vec<usize> = (0..rows.len()).collect();
        order.sort_by_key(|&i| rows[i].get(&self.available).and_then(Value::as_str).unwrap_or("").to_string());
        let mut keep = vec![true; rows.len()];
        for i in order {
            let row = &rows[i];
            let key = self.key(row);
            let same = self.latest.get(&key).is_some_and(|(_, hash)| row.get(&self.hash) == Some(hash));
            if same { keep[i] = false } else { self.observe(row) }
        }
        keep
    }

    pub(crate) fn retain(&mut self, rows: &mut Vec<Map<String, Value>>) {
        let keep = self.mask(rows);
        let mut i = 0;
        rows.retain(|_| { let accepted = keep[i]; i += 1; accepted });
    }

    pub(crate) fn projected_columns(&self) -> Vec<&str> {
        let mut names: Vec<&str> = self.columns.iter().map(String::as_str).collect();
        names.extend([self.available.as_str(), self.hash.as_str()]);
        names
    }
}

/// `batch` with each row's availability instant resolved and the availability flags set by
/// the engine alone: a producer's own flags are dropped. A date-only value resolves to the
/// start of the next day in the declared zone (`store.available-time.date-end`); a null
/// value fills from valid time plus the row's class lag, flagged `_available_imputed`
/// (`store.available-time.lag-fill`); a row available before `live_since` is flagged
/// `_available_reconstructed` (`store.available-time.live-since`). A row still holding no
/// instant lands as null, for the landing's null refusal to name.
pub(crate) fn resolve(decl: &TableDecl, mut batch: Batch) -> Result<Batch> {
    for row in &mut batch.rows {
        for flag in AVAILABILITY_FLAGS {
            row.remove(flag);
        }
    }
    let Some(available) = &decl.available_time else { return Ok(batch) };
    let invalid = |e: String| ContextError::Invalid(format!("table `{}`: {e}", decl.name));
    let live_since = available.live_since.as_deref().map(Instant::parse).transpose().map_err(|e| invalid(format!("available_time live_since: {e}")))?;
    for row in &mut batch.rows {
        let instant = match row.get(&available.column) {
            Some(Value::String(text)) => Some(instant(text, available).map_err(invalid)?),
            None | Some(Value::Null) => {
                let filled = lagged(decl, available, row).map_err(invalid)?;
                if filled.is_some() {
                    row.insert(AVAILABLE_IMPUTED.into(), Value::Bool(true));
                }
                filled
            }
            Some(_) => None,
        };
        let Some(at) = instant else { continue };
        row.insert(available.column.clone(), Value::String(at.to_rfc3339_nanos()));
        if live_since.is_some_and(|live| at < live) {
            row.insert(AVAILABLE_RECONSTRUCTED.into(), Value::Bool(true));
        }
    }
    batch.types.insert(available.column.clone(), ColumnType::Timestamp);
    for flag in AVAILABILITY_FLAGS {
        if batch.rows.iter().any(|row| row.contains_key(flag)) {
            batch.types.insert(flag.into(), ColumnType::Boolean);
        }
    }
    Ok(batch)
}

/// A sourced instant: RFC 3339 as sent, or a date resolved to the start of the next day in
/// the declared zone, UTC when none is declared.
fn instant(text: &str, available: &AvailableTime) -> std::result::Result<Instant, String> {
    let b = text.as_bytes();
    if !(b.len() == 10 && b[4] == b'-' && b[7] == b'-') {
        return Instant::parse(text).map_err(|e| format!("availability instant `{text}`: {e}"));
    }
    let date: jiff::civil::Date = text.parse().map_err(|e| format!("availability date `{text}`: {e}"))?;
    let zone = available.zone.as_deref().unwrap_or("UTC");
    let tz = jiff::tz::TimeZone::get(zone).map_err(|e| format!("available_time zone `{zone}`: {e}"))?;
    let next = date.tomorrow().map_err(|e| format!("availability date `{text}`: {e}"))?;
    let start = next.to_zoned(tz).map_err(|e| format!("availability date `{text}` in `{zone}`: {e}"))?;
    Instant::from_unix_nanos(start.timestamp().as_nanosecond()).map_err(|e| e.to_string())
}

/// Valid-time `to`, else `from`, plus the lag the row's class maps to; `None` when the table
/// declares no lag, the row names no listed class, or carries no valid-time instant.
fn lagged(decl: &TableDecl, available: &AvailableTime, row: &Map<String, Value>) -> std::result::Result<Option<Instant>, String> {
    let (Some(lag), Some(valid)) = (&available.lag, &decl.valid_time) else { return Ok(None) };
    let Some(Value::String(class)) = row.get(&lag.class_column) else { return Ok(None) };
    let Some(secs) = available.lag_secs(class).map_err(|e| e.0)? else { return Ok(None) };
    let base = valid.to.iter().chain(std::iter::once(&valid.from)).find_map(|column| match row.get(column) {
        Some(Value::String(text)) => Some(text.clone()),
        _ => None,
    });
    let Some(base) = base else { return Ok(None) };
    let base = if base.len() == 10 { format!("{base}T00:00:00Z") } else { base };
    let base = Instant::parse(&base).map_err(|e| format!("valid-time instant `{base}`: {e}"))?;
    Ok(Some(base.plus_secs(secs)))
}

/// The latest availability instant among `rows`, on a table declaring `available_time`:
/// what a sidecar built over them records as `available_through`
/// (`read.horizon.rank-statistics`). `None` where the table declares none or no row
/// carries an instant.
pub(crate) fn latest(decl: &TableDecl, rows: &arrow_array::RecordBatch) -> Result<Option<Instant>> {
    use arrow_array::Array;
    let Some(available) = &decl.available_time else { return Ok(None) };
    let Some(column) = rows.column_by_name(&available.column) else { return Ok(None) };
    let nanos = arrow_cast::cast(column, &arrow_schema::DataType::Timestamp(arrow_schema::TimeUnit::Nanosecond, None))
        .map_err(|e| ContextError::Invalid(format!("table `{}`: `{}` holds no instant: {e}", decl.name, available.column)))?;
    let nanos = nanos.as_any().downcast_ref::<arrow_array::TimestampNanosecondArray>().expect("a cast to nanoseconds yields nanoseconds");
    let latest = (0..nanos.len()).filter(|i| !nanos.is_null(*i)).map(|i| nanos.value(i)).max();
    latest.map(|n| Instant::from_unix_nanos(i128::from(n)).map_err(|e| ContextError::Invalid(e.to_string()))).transpose()
}
