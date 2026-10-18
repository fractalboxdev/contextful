//! `store.available-time` at landing: each row's availability instant, resolved from the
//! source before the batch reconciles, and the flags only the engine sets.

use crate::land::Batch;
use crate::store::Store;
use crate::{ContextError, Result};
use contextful_core::store::declare::{AvailableTime, TableDecl};
use contextful_core::store::reconcile::ColumnType;
use contextful_core::store::reserve::{AVAILABILITY_FLAGS, AVAILABLE_IMPUTED, AVAILABLE_RECONSTRUCTED};
use contextful_core::time::Instant;
use arrow_array::{Array, ArrayRef, BooleanArray, RecordBatch, Scalar, StringArray};
use contextful_core::store::bound_time::Bounds;
use parquet::arrow::arrow_reader::statistics::StatisticsConverter;
use parquet::arrow::arrow_reader::ParquetRecordBatchReaderBuilder;
use parquet::arrow::ProjectionMask;
use serde_json::{Map, Value};
use std::collections::BTreeMap;
use std::fs::File;
use std::path::Path;
use std::sync::Arc;

/// The landing rows a vintage comparison matches, one value list per key and valid-time
/// column (`store.available-time.vintage-scope`).
struct Scope {
    probes: Vec<Probe>,
    rows: usize,
}

/// One comparison column's landing values. A valid-time column bounds a row group only as
/// an instant, since the comparison key normalizes its text.
struct Probe {
    name: String,
    values: Vec<Value>,
    instant_only: bool,
}

impl Scope {
    fn of<'a>(columns: &[String], valid_from: Option<&str>, landing: impl IntoIterator<Item = &'a Map<String, Value>>) -> Self {
        let mut probes: Vec<Probe> =
            columns.iter().map(|name| Probe { name: name.clone(), values: Vec::new(), instant_only: valid_from == Some(name.as_str()) }).collect();
        let mut rows = 0;
        for row in landing {
            rows += 1;
            for probe in &mut probes {
                probe.values.push(row.get(&probe.name).cloned().unwrap_or(Value::Null));
            }
        }
        Self { probes, rows }
    }

    /// Hand `each` the record batches, projected to `names`, of each row group of `path`
    /// that every probe admits.
    fn each_admitted_batch(&self, store: &Store, path: &Path, names: &[&str], each: &mut dyn FnMut(RecordBatch) -> Result<()>) -> Result<()> {
        if !path.is_file() { return Ok(()) }
        let pq = |e: parquet::errors::ParquetError| ContextError::Parquet { path: path.to_path_buf(), message: e.to_string() };
        let builder = store.parquet_builder(path)?;
        let mut admitted = vec![true; builder.metadata().num_row_groups()];
        for probe in &self.probes {
            probe.exclude_by_bounds(&builder, &mut admitted);
            probe.exclude_by_bloom(&builder, &mut admitted);
        }
        let selected: Vec<usize> = (0..admitted.len()).filter(|g| admitted[*g]).collect();
        if selected.is_empty() { return Ok(()) }
        let roots: Vec<usize> = builder.schema().fields().iter().enumerate().filter(|(_, f)| names.contains(&f.name().as_str())).map(|(i, _)| i).collect();
        let mask = ProjectionMask::roots(builder.parquet_schema(), roots);
        for batch in builder.with_row_groups(selected).with_projection(mask).build().map_err(pq)? {
            each(batch.map_err(|e| ContextError::Parquet { path: path.to_path_buf(), message: e.to_string() })?)?;
        }
        Ok(())
    }
}

impl Probe {
    /// The landing values cast to the stored column's type; `None` where one is null or
    /// fails the cast, or the stored type orders otherwise than the comparison key matches.
    fn typed(&self, field: &arrow_schema::Field) -> Option<ArrayRef> {
        use arrow_schema::DataType as T;
        let instant = matches!(field.data_type(), T::Timestamp(..) | T::Date32 | T::Date64);
        let ordered = matches!(field.data_type(), T::Utf8 | T::LargeUtf8 | T::Utf8View | T::Int8 | T::Int16 | T::Int32 | T::Int64 | T::UInt8 | T::UInt16 | T::UInt32 | T::UInt64);
        if instant { return self.instants(field.data_type()) }
        if !(ordered && !self.instant_only) { return None }
        let text: Option<Vec<String>> = self
            .values
            .iter()
            .map(|v| match v {
                Value::String(s) => Some(s.clone()),
                Value::Number(n) => Some(n.to_string()),
                _ => None,
            })
            .collect();
        let options = arrow_cast::CastOptions { safe: false, ..Default::default() };
        arrow_cast::cast_with_options(&StringArray::from(text?), field.data_type(), &options).ok()
    }

    /// The landing values as instants of the stored temporal type, each text parsed as the
    /// comparison key parses it: a date alone is midnight UTC.
    fn instants(&self, data_type: &arrow_schema::DataType) -> Option<ArrayRef> {
        use arrow_array::types::{TimestampMicrosecondType, TimestampMillisecondType, TimestampNanosecondType, TimestampSecondType};
        use arrow_array::{Date32Array, Date64Array, PrimitiveArray};
        use arrow_schema::{DataType as T, TimeUnit};
        let nanos: Option<Vec<i128>> = self
            .values
            .iter()
            .map(|v| {
                let Value::String(raw) = v else { return None };
                let text = if raw.len() == 10 { format!("{raw}T00:00:00Z") } else { raw.clone() };
                Instant::parse(&text).ok().map(Instant::unix_nanos)
            })
            .collect();
        let scaled = |per: i128| -> Option<Vec<i64>> { nanos.as_ref()?.iter().map(|n| i64::try_from(n.div_euclid(per)).ok()).collect() };
        Some(match data_type {
            T::Timestamp(unit, tz) => match unit {
                TimeUnit::Second => Arc::new(PrimitiveArray::<TimestampSecondType>::from(scaled(1_000_000_000)?).with_timezone_opt(tz.clone())) as ArrayRef,
                TimeUnit::Millisecond => Arc::new(PrimitiveArray::<TimestampMillisecondType>::from(scaled(1_000_000)?).with_timezone_opt(tz.clone())),
                TimeUnit::Microsecond => Arc::new(PrimitiveArray::<TimestampMicrosecondType>::from(scaled(1_000)?).with_timezone_opt(tz.clone())),
                TimeUnit::Nanosecond => Arc::new(PrimitiveArray::<TimestampNanosecondType>::from(scaled(1)?).with_timezone_opt(tz.clone())),
            },
            T::Date32 => Arc::new(Date32Array::from(scaled(86_400_000_000_000)?.into_iter().map(|d| i32::try_from(d).ok()).collect::<Option<Vec<i32>>>()?)),
            T::Date64 => Arc::new(Date64Array::from(scaled(86_400_000_000_000)?.into_iter().map(|d| d * 86_400_000).collect::<Vec<i64>>())),
            _ => return None,
        })
    }

    /// Clear each row group whose footer bounds on this column exclude every landing value.
    fn exclude_by_bounds(&self, builder: &ParquetRecordBatchReaderBuilder<File>, admitted: &mut [bool]) {
        let schema = builder.schema();
        let Ok(field) = schema.field_with_name(&self.name) else { return };
        let Some(values) = self.typed(field) else { return };
        let Ok(converter) = StatisticsConverter::try_new(&self.name, schema, builder.parquet_schema()) else { return };
        let groups = builder.metadata().row_groups();
        let (Ok(mins), Ok(maxes)) = (converter.row_group_mins(groups.iter()), converter.row_group_maxes(groups.iter())) else { return };
        let Ok(order) = arrow_ord::sort::sort_to_indices(&values, None, None) else { return };
        let (Some(&first), Some(&last)) = (order.values().first(), order.values().last()) else { return };
        let (low, high) = (Scalar::new(values.slice(first as usize, 1)), Scalar::new(values.slice(last as usize, 1)));
        let (Ok(below), Ok(above)) = (arrow_ord::cmp::lt(&high, &mins), arrow_ord::cmp::gt(&low, &maxes)) else { return };
        let outside = |b: &BooleanArray, g: usize| b.is_valid(g) && b.value(g);
        for (g, keep) in admitted.iter_mut().enumerate() {
            if outside(&below, g) || outside(&above, g) { *keep = false }
        }
    }

    /// Clear each row group whose bloom filter on this column holds no landing value. Only a
    /// text or signed integer key column probes: its stored bytes are the value a key
    /// matches. A valid-time column never probes, since the comparison key normalizes its
    /// text and two spellings of one instant hash apart.
    fn exclude_by_bloom(&self, builder: &ParquetRecordBatchReaderBuilder<File>, admitted: &mut [bool]) {
        use arrow_schema::DataType as T;
        if self.instant_only { return }
        let schema = builder.schema();
        let Ok(field) = schema.field_with_name(&self.name) else { return };
        let text = matches!(field.data_type(), T::Utf8 | T::LargeUtf8 | T::Utf8View);
        let wide = matches!(field.data_type(), T::Int64);
        let narrow = matches!(field.data_type(), T::Int8 | T::Int16 | T::Int32);
        if !(text || wide || narrow) { return }
        let Ok(converter) = StatisticsConverter::try_new(&self.name, schema, builder.parquet_schema()) else { return };
        let Some(column) = converter.parquet_column_index() else { return };
        for (g, keep) in admitted.iter_mut().enumerate() {
            if !*keep { continue }
            let Ok(Some(filter)) = builder.get_row_group_column_bloom_filter(g, column) else { continue };
            let held = self.values.iter().any(|value| match value {
                Value::String(s) if text => filter.check(s.as_str()),
                Value::Number(n) if wide => n.as_i64().is_none_or(|v| filter.check(&v)),
                Value::Number(n) if narrow => n.as_i64().and_then(|v| i32::try_from(v).ok()).is_none_or(|v| filter.check(&v)),
                // A filter holds no null, and a value of another kind never equals a
                // stored text or integer key.
                Value::Null => true,
                _ => false,
            });
            if !held { *keep = false }
        }
    }
}

/// The latest available vintage for each key and valid-time line.
pub(crate) struct VintageHistory {
    latest: BTreeMap<String, (String, Value)>,
    columns: Vec<String>,
    valid_from: Option<String>,
    available: String,
    hash: String,
}

impl VintageHistory {
    /// The stored vintages of the keys `landing` carries, read before `before_commit_seq`
    /// where given. A part's row group is decoded only when its footer bounds on each key
    /// and valid-time column, and its key bloom filters, admit some landing row; the read
    /// holds the landing's key and valid-time values and one projected stored record batch
    /// at a time (`store.available-time.vintage-scope`).
    pub(crate) fn load<'a>(
        store: &Store,
        decl: &TableDecl,
        before_commit_seq: Option<i64>,
        landing: impl IntoIterator<Item = &'a Map<String, Value>>,
    ) -> Result<Option<Self>> {
        if !decl.is_vintage() { return Ok(None) }
        let available = decl.available_time.as_ref().expect("a vintage has availability time").column.clone();
        let hash = decl.content_hash_column().expect("a vintage has a content hash").to_string();
        let mut columns = decl.primary_key().to_vec();
        columns.extend(decl.valid_time.as_ref().map(|valid| valid.from.clone()));
        let valid_from = decl.valid_time.as_ref().map(|valid| valid.from.clone());
        let mut history = Self { latest: BTreeMap::new(), columns, valid_from, available, hash };
        let scope = Scope::of(&history.columns, history.valid_from.as_deref(), landing);
        if scope.rows == 0 || store.try_schema(&decl.name)?.is_none() { return Ok(Some(history)) }
        let mut owned = history.columns.clone();
        owned.extend([history.available.clone(), history.hash.clone(), "_commit_seq".into()]);
        let names: Vec<&str> = owned.iter().map(String::as_str).collect();
        for file in crate::scan::scan(store, decl, Bounds::default())?.files {
            scope.each_admitted_batch(store, &store.logical_path(&file)?, &names, &mut |batch| {
                store.count_vintage_rows(batch.num_rows());
                for row in crate::rows::batch_rows(&batch, &names)? {
                    if before_commit_seq.is_some_and(|seq| row.get("_commit_seq").and_then(Value::as_i64).is_none_or(|held| held >= seq)) { continue }
                    history.observe(&row);
                }
                Ok(())
            })?;
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
            store.count_vintage_rows(batch.num_rows());
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

    /// The columns that scope a vintage comparison's read: the key and valid-time `from`
    /// where declared.
    pub(crate) fn scoped_columns(decl: &TableDecl) -> Vec<String> {
        let mut names = decl.primary_key().to_vec();
        names.extend(decl.valid_time.as_ref().map(|valid| valid.from.clone()));
        names
    }

    /// The columns a vintage comparison reads of a vintage table's row: its key, valid-time
    /// `from` where declared, availability instant and content hash.
    pub(crate) fn compared_columns(decl: &TableDecl) -> Vec<String> {
        let mut names = Self::scoped_columns(decl);
        names.extend(decl.available_time.as_ref().map(|available| available.column.clone()));
        names.extend(decl.content_hash_column().map(str::to_string));
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
        // A snapshot document captured after it became available shows the page as later
        // updated, not as published (`store.snapshot.post-horizon-capture`).
        let captured_later = decl.snapshot.as_ref().and_then(|s| row.get(&s.captured_at)).and_then(Value::as_str).and_then(|t| Instant::parse(t).ok()).is_some_and(|captured| captured > at);
        if live_since.is_some_and(|live| at < live) || captured_later {
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
