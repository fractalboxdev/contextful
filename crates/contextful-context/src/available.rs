//! `store.available-time` at landing: each row's availability instant, resolved from the
//! source before the batch reconciles, and the flags only the engine sets.

use crate::land::Batch;
use crate::{ContextError, Result};
use contextful_core::store::declare::{AvailableTime, TableDecl};
use contextful_core::store::reconcile::ColumnType;
use contextful_core::store::reserve::{AVAILABILITY_FLAGS, AVAILABLE_IMPUTED, AVAILABLE_RECONSTRUCTED};
use contextful_core::time::Instant;
use serde_json::{Map, Value};

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
