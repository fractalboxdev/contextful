//! A pass over an append table that carries parts by reference
//! (`store.fold.carry-by-reference`): it writes the rows its unfolded runs land as one delta
//! part per partition, rewrites only a part one of those rows replaces or retention trims,
//! names every other part of the current snapshot at its origin, and merges a partition's
//! parts once one size tier fills (`store.fold.tiered-merge`). Sidecars extend by a segment
//! over the delta while no stored row changes (`store.index.sidecar-segments`).

use super::merge::merge_into;
use super::{claim, dedupe, partition, sort, escape};
use crate::error::{ContextError, IoPath, Result};
use crate::parquet_io;
use crate::store::{FileLock, Store};
use arrow_array::{Array, ArrayRef, BooleanArray, RecordBatch, TimestampNanosecondArray};
use arrow_ord::sort::SortOptions;
use arrow_row::{RowConverter, SortField};
use arrow_schema::Schema as ArrowSchema;
use arrow_select::concat::concat_batches;
use arrow_select::filter::filter_record_batch;
use contextful_core::store::declare::TableDecl;
use contextful_core::store::fold::{size_tier, RetentionReport, FOLD_MERGE_FANOUT};
use contextful_core::store::index::{IndexEntry, IndexKind, INDEXES_DIR};
use contextful_core::store::lay_out::{part_name, PartEntry, RunManifest, SnapshotId, RUNS_DIR, SNAPSHOTS_DIR};
use contextful_core::store::reserve::TIEBREAK;
use contextful_core::store::resolve::TableState;
use contextful_core::time::Instant;
use parquet::arrow::arrow_reader::ParquetRecordBatchReaderBuilder;
use parquet::basic::{LogicalType, TimeUnit};
use parquet::file::statistics::Statistics;
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fs::{self, File};
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// What a carrying pass reads.
pub(super) struct Input<'a> {
    pub store: &'a Store,
    pub decl: &'a TableDecl,
    pub state: &'a TableState,
    pub unfolded: &'a [&'a RunManifest],
    pub target: Arc<ArrowSchema>,
    pub cutoff: Option<Instant>,
    /// Declared sidecars the pass builds in full, by their path under `indexes/`.
    pub built: &'a [String],
    pub now: Instant,
}

/// What a carrying pass staged.
pub(super) struct Output {
    pub snapshot_id: SnapshotId,
    pub staging: PathBuf,
    pub in_flight: FileLock,
    pub parts: Vec<PartEntry>,
    pub indexes: Vec<IndexEntry>,
    pub row_count: u64,
    pub retention: Option<RetentionReport>,
    /// Bytes per partition, for the partition plan's warnings.
    pub sizes: Vec<u64>,
}

/// One part of the snapshot the pass stages: carried at its origin, or written into staging.
struct Slot {
    entry: PartEntry,
    path: PathBuf,
    live: bool,
}

impl Slot {
    fn rows(&self) -> u64 {
        self.entry.rows.unwrap_or(0)
    }

    fn staged(&self) -> bool {
        self.entry.origin.is_none()
    }
}

/// The next unused part name in `dir` of the staging directory.
struct Names(BTreeMap<String, u32>);

impl Names {
    fn next(&mut self, dir: &str) -> String {
        let n = self.0.entry(dir.to_string()).or_insert(0);
        let name = if dir.is_empty() { part_name(*n) } else { format!("{dir}/{}", part_name(*n)) };
        *n += 1;
        name
    }
}

fn columns(b: &RecordBatch, names: &[String]) -> Result<Vec<ArrayRef>> {
    names
        .iter()
        .map(|c| b.column_by_name(c).cloned().ok_or_else(|| ContextError::Invalid(format!("no column `{c}`"))))
        .collect()
}

fn converter(target: &ArrowSchema, names: &[String], options: SortOptions) -> Result<RowConverter> {
    let fields = names
        .iter()
        .map(|c| Ok(SortField::new_with_options(target.field_with_name(c).map_err(arrow)?.data_type().clone(), options)))
        .collect::<Result<Vec<_>>>()?;
    RowConverter::new(fields).map_err(arrow)
}

fn arrow(e: arrow_schema::ArrowError) -> ContextError {
    ContextError::Invalid(e.to_string())
}

/// `target` narrowed to `names`.
fn narrowed(target: &ArrowSchema, names: &[String]) -> Result<Arc<ArrowSchema>> {
    let fields = names.iter().map(|c| target.field_with_name(c).cloned().map_err(arrow)).collect::<Result<Vec<_>>>()?;
    Ok(Arc::new(ArrowSchema::new(fields)))
}

/// Whether each row's `column` instant is at or after `cutoff`; a null expires.
fn unexpired(b: &RecordBatch, column: &str, cutoff: Instant, table: &str) -> Result<Vec<bool>> {
    let c = b.column_by_name(column).ok_or_else(|| ContextError::Invalid(format!("table `{table}`: no retention column `{column}`")))?;
    let times = c.as_any().downcast_ref::<TimestampNanosecondArray>().ok_or_else(|| {
        ContextError::Invalid(format!("table `{table}`: retention column `{column}` is not a nanosecond timestamp"))
    })?;
    Ok((0..b.num_rows()).map(|i| !times.is_null(i) && i128::from(times.value(i)) >= cutoff.unix_nanos()).collect())
}

/// The exact minimum and maximum of a nanosecond timestamp `column` over a plaintext part's
/// footer, or `None` where a row group's statistics are absent or inexact.
fn footer_bounds(path: &Path, column: &str) -> Result<Option<(i128, i128)>> {
    let file = File::open(path).at(path)?;
    let metadata = ParquetRecordBatchReaderBuilder::try_new(file)
        .map_err(|e| ContextError::Parquet { path: path.to_path_buf(), message: e.to_string() })?
        .metadata()
        .clone();
    let schema = metadata.file_metadata().schema_descr();
    let Some(index) = schema.columns().iter().position(|c| {
        c.path().parts().len() == 1 && c.name() == column && matches!(c.logical_type_ref(), Some(LogicalType::Timestamp { unit: TimeUnit::NANOS, .. }))
    }) else {
        return Ok(None);
    };
    let mut bounds: Option<(i128, i128)> = None;
    for group in metadata.row_groups() {
        let Some(Statistics::Int64(stats)) = group.column(index).statistics() else { return Ok(None) };
        let (Some(min), Some(max)) = (stats.min_opt(), stats.max_opt()) else { return Ok(None) };
        if !stats.min_is_exact() || !stats.max_is_exact() || stats.null_count_opt().is_none_or(|n| n > 0) {
            return Ok(None);
        }
        let (min, max) = (i128::from(*min), i128::from(*max));
        bounds = Some(bounds.map_or((min, max), |(lo, hi)| (lo.min(min), hi.max(max))));
    }
    Ok(bounds)
}

/// Whether `dir` is a partition directory of `by`: one `<column>=<value>` segment per
/// column, outermost first.
fn partition_path(dir: &str, by: &[String]) -> bool {
    if by.is_empty() {
        return dir.is_empty();
    }
    let segments: Vec<&str> = dir.split('/').collect();
    segments.len() == by.len() && segments.iter().zip(by).all(|(s, c)| s.split_once('=').is_some_and(|(k, _)| k == c))
}

/// The batches of the part at `path`, each conformed to `target`, decoded one at a time.
fn conformed<'a>(store: &'a Store, path: &Path, target: &'a Arc<ArrowSchema>) -> Result<Box<dyn Iterator<Item = Result<RecordBatch>> + 'a>> {
    let batches = store.parquet_batches(path, None)?;
    Ok(Box::new(batches.map(move |b| b.and_then(|b| parquet_io::conform(&b, target)))))
}

/// Stage a carrying pass, or `None` when nothing landed, expired or needs a build.
pub(super) fn stage(input: Input<'_>) -> Result<Option<Output>> {
    let Input { store, decl, state, unfolded, target, cutoff, built, now } = input;
    let table = decl.name.as_str();
    let invalid = |e: arrow_schema::ArrowError| ContextError::Invalid(format!("table `{table}`: {e}"));
    let table_dir = store.table_dir(table)?;
    let prior = state.chain.first();
    let by = decl.partition_by();

    // The current snapshot's parts, each named at the directory holding its file.
    let mut slots: Vec<Slot> = Vec::new();
    if let Some(s) = prior {
        for p in &s.parts {
            let origin = p.holder(&s.snapshot_id).clone();
            let path = table_dir.join(SNAPSHOTS_DIR).join(origin.to_string()).join(&p.name);
            let rows = match p.rows {
                Some(r) => r,
                None => store.parquet_row_count(&path)?,
            };
            let entry = PartEntry { name: p.name.clone(), key_version: p.key_version, origin: Some(origin), rows: Some(rows) };
            slots.push(Slot { entry, path, live: true });
        }
    }

    // The delta: every unfolded run's rows, one per key and line on a keyed table.
    let mut batches = Vec::new();
    for r in unfolded {
        for p in &r.parts {
            let path = table_dir.join(RUNS_DIR).join(&r.run_id).join(&r.node_id).join(&p.name);
            for b in store.read_parquet(&path)? {
                batches.push(parquet_io::conform(&b, &target)?);
            }
        }
    }
    let mut delta = concat_batches(&target, &batches).map_err(invalid)?;
    drop(batches);
    if decl.is_keyed() {
        delta = dedupe(&delta, &decl.dedup_line(), decl.order_by()).map_err(invalid)?;
    }
    let delta_partitions: BTreeSet<String> = partition(&delta, by).map_err(invalid)?.into_iter().filter(|(_, b)| b.num_rows() > 0).map(|(d, _)| d).collect();
    let mut before: BTreeSet<String> = slots.iter().filter(|s| s.rows() > 0).map(|s| s.entry.partition().to_string()).collect();
    before.extend(delta_partitions.iter().cloned());

    // Winner selection between the delta and the stored rows sharing its keys: a stored
    // row ranking higher keeps its place and drops the delta row; otherwise the stored row
    // leaves its part (`store.declare.dedup-view`).
    let mut stored_drop: Vec<BTreeSet<u64>> = vec![BTreeSet::new(); slots.len()];
    if decl.is_keyed() && delta.num_rows() > 0 && !slots.is_empty() {
        let line = decl.dedup_line();
        let rank: Vec<String> =
            std::iter::once(decl.order_by()).chain(TIEBREAK.into_iter().filter(|c| *c != decl.order_by())).map(str::to_string).collect();
        let keys = converter(&target, &line, SortOptions { descending: false, nulls_first: false })?;
        let ranks = converter(&target, &rank, SortOptions { descending: true, nulls_first: false })?;
        let delta_keys = keys.convert_columns(&columns(&delta, &line)?).map_err(invalid)?;
        let delta_ranks = ranks.convert_columns(&columns(&delta, &rank)?).map_err(invalid)?;
        let index: HashMap<Vec<u8>, usize> = (0..delta.num_rows()).map(|j| (delta_keys.row(j).as_ref().to_vec(), j)).collect();
        let mut keep = vec![true; delta.num_rows()];
        let mut projection = line.clone();
        projection.extend(rank.iter().filter(|c| !line.contains(c)).cloned());
        let sub = narrowed(&target, &projection)?;
        // A partition named by the dedup line holds every row of its keys, provided every
        // part sits in a directory of the current `partition_by`: erasure leaves one root
        // part, and a changed `partition_by` leaves parts under the old columns.
        let scoped = by.iter().all(|c| line.contains(c)) && slots.iter().all(|s| partition_path(s.entry.partition(), by));
        for (k, slot) in slots.iter().enumerate() {
            if scoped && !delta_partitions.contains(slot.entry.partition()) {
                continue;
            }
            let mut offset = 0u64;
            for b in store.parquet_batches(&slot.path, Some(&projection))? {
                let b = parquet_io::conform(&b?, &sub)?;
                let stored_keys = keys.convert_columns(&columns(&b, &line)?).map_err(invalid)?;
                let stored_ranks = ranks.convert_columns(&columns(&b, &rank)?).map_err(invalid)?;
                for i in 0..b.num_rows() {
                    if let Some(&j) = index.get(stored_keys.row(i).as_ref()) {
                        if stored_ranks.row(i) < delta_ranks.row(j) {
                            keep[j] = false;
                        } else {
                            stored_drop[k].insert(offset + i as u64);
                        }
                    }
                }
                offset += b.num_rows() as u64;
            }
        }
        if keep.iter().any(|k| !k) {
            delta = filter_record_batch(&delta, &BooleanArray::from(keep)).map_err(invalid)?;
        }
    }

    // Row retention: a part whose exact footer bounds clear the cutoff stays carried or
    // leaves whole; any other part counts its expired rows and is rewritten when it holds one.
    let mut expired = 0u64;
    let mut drop_whole = vec![false; slots.len()];
    let mut trim = vec![false; slots.len()];
    if let (Some(rule), Some(cutoff)) = (&decl.retain_rows, cutoff) {
        let column = vec![rule.column.clone()];
        let sub = narrowed(&target, &column)?;
        for (k, slot) in slots.iter().enumerate() {
            let bounds = if store.parquet_key().is_none() { footer_bounds(&slot.path, &rule.column)? } else { None };
            match bounds {
                Some((_, max)) if max < cutoff.unix_nanos() => {
                    drop_whole[k] = true;
                    expired += slot.rows() - stored_drop[k].len() as u64;
                }
                Some((min, _)) if min >= cutoff.unix_nanos() => {}
                _ => {
                    let mut offset = 0u64;
                    let mut n = 0u64;
                    for b in store.parquet_batches(&slot.path, Some(&column))? {
                        let b = parquet_io::conform(&b?, &sub)?;
                        let alive = unexpired(&b, &rule.column, cutoff, table)?;
                        n += alive.iter().enumerate().filter(|(i, a)| !**a && !stored_drop[k].contains(&(offset + *i as u64))).count() as u64;
                        offset += b.num_rows() as u64;
                    }
                    if n > 0 {
                        trim[k] = true;
                        expired += n;
                    }
                }
            }
        }
        let alive = unexpired(&delta, &rule.column, cutoff, table)?;
        let gone = alive.iter().filter(|a| !**a).count() as u64;
        if gone > 0 {
            expired += gone;
            delta = filter_record_batch(&delta, &BooleanArray::from(alive)).map_err(invalid)?;
        }
    }
    if unfolded.is_empty() && expired == 0 && built.is_empty() {
        return Ok(None);
    }

    let parent = prior.map(|s| s.snapshot_id.clone());
    let (snapshot_id, staging, in_flight) = claim(store, table, SnapshotId::next(now, parent.as_ref()), now)?;
    let mut names = Names(BTreeMap::new());
    let mut touched: BTreeSet<String> = BTreeSet::new();
    let mut changed = false;
    let bloom = crate::available::bloom_columns(decl);

    // Rewrite each part a delta row replaces or retention trims, streaming its batches.
    for k in 0..slots.len() {
        if drop_whole[k] {
            slots[k].live = false;
            changed = true;
            continue;
        }
        if stored_drop[k].is_empty() && !trim[k] {
            continue;
        }
        changed = true;
        let dir = slots[k].entry.partition().to_string();
        let name = names.next(&dir);
        let path = staging.join(&name);
        let mut writer = store.parquet_writer(&path, target.clone(), &bloom, slots[k].rows())?;
        let mut offset = 0u64;
        for b in conformed(store, &slots[k].path, &target)? {
            let b = b?;
            let alive = match (&decl.retain_rows, cutoff) {
                (Some(rule), Some(cutoff)) => unexpired(&b, &rule.column, cutoff, table)?,
                _ => vec![true; b.num_rows()],
            };
            let keep: BooleanArray = (0..b.num_rows()).map(|i| Some(alive[i] && !stored_drop[k].contains(&(offset + i as u64)))).collect();
            offset += b.num_rows() as u64;
            writer.write(&filter_record_batch(&b, &keep).map_err(invalid)?)?;
        }
        let rows = writer.close()?;
        slots[k].live = false;
        if rows == 0 {
            fs::remove_file(&path).at(&path)?;
        } else {
            let entry = PartEntry { rows: Some(rows), ..PartEntry::new(name, store.sealing().key_version()) };
            slots.push(Slot { entry, path, live: true });
        }
        touched.insert(dir);
    }

    // One delta part per partition the landed rows reach, sorted as a whole snapshot sorts.
    let sort_columns = super::sort_order(decl);
    if !sort_columns.is_empty() && delta.num_rows() > 0 {
        delta = sort(&delta, &sort_columns).map_err(invalid)?;
    }
    for (dir, batch) in partition(&delta, by).map_err(invalid)? {
        if batch.num_rows() == 0 {
            continue;
        }
        let name = names.next(&dir);
        let path = staging.join(&name);
        store.write_parquet_blooming(&path, &batch, &bloom)?;
        let entry = PartEntry { rows: Some(batch.num_rows() as u64), ..PartEntry::new(name, store.sealing().key_version()) };
        slots.push(Slot { entry, path, live: true });
        touched.insert(dir);
    }

    // Tiered merge: within a touched partition, the parts of the lowest size tier holding
    // `FOLD_MERGE_FANOUT` or more merge into one, until no tier does.
    for dir in &touched {
        loop {
            let mut tiers: BTreeMap<u32, Vec<usize>> = BTreeMap::new();
            for (k, s) in slots.iter().enumerate().filter(|(_, s)| s.live && s.entry.partition() == dir) {
                tiers.entry(size_tier(s.rows())).or_default().push(k);
            }
            let Some((_, members)) = tiers.into_iter().find(|(_, m)| m.len() >= FOLD_MERGE_FANOUT) else { break };
            let name = names.next(dir);
            let path = staging.join(&name);
            let rows: u64 = members.iter().map(|k| slots[*k].rows()).sum();
            let mut writer = store.parquet_writer(&path, target.clone(), &bloom, rows)?;
            let inputs = members.iter().map(|k| conformed(store, &slots[*k].path, &target)).collect::<Result<Vec<_>>>()?;
            merge_into(inputs, &target, &sort_columns, &mut writer)?;
            let rows = writer.close()?;
            for k in &members {
                slots[*k].live = false;
                if slots[*k].staged() {
                    fs::remove_file(&slots[*k].path).at(&slots[*k].path)?;
                }
            }
            let entry = PartEntry { rows: Some(rows), ..PartEntry::new(name, store.sealing().key_version()) };
            slots.push(Slot { entry, path, live: true });
        }
    }
    slots.retain(|s| s.live);
    slots.sort_by(|a, b| a.entry.location(&snapshot_id).cmp(&b.entry.location(&snapshot_id)));

    let after: BTreeSet<String> = slots.iter().map(|s| s.entry.partition().to_string()).collect();
    let retention = match (&decl.retain_rows, cutoff) {
        (Some(_), Some(cutoff)) => Some(RetentionReport {
            cutoff,
            rows_expired: expired,
            partitions_dropped: if by.is_empty() { 0 } else { before.difference(&after).count() as u64 },
        }),
        _ => None,
    };

    // Sidecars: extended by a segment over the delta while no stored row changed, built in
    // full over every row otherwise.
    let mut indexes = Vec::new();
    if !decl.indexes().is_empty() {
        let id_column = decl.id_column()?.unwrap_or_default().to_string();
        let mut whole: Option<RecordBatch> = None;
        let mut checked = false;
        for index in decl.indexes() {
            let path = index.path(escape);
            let short = path.strip_prefix(&format!("{INDEXES_DIR}/")).unwrap_or(&path).to_string();
            let previous = prior.and_then(|s| s.indexes.iter().find(|e| e.path() == Some(path.as_str())).map(|e| (s, e)));
            let extendable = !changed
                && !built.contains(&short)
                && previous.is_some_and(|(_, e)| e.kind() == Some(index.kind));
            if let (true, Some((s, entry))) = (extendable, previous) {
                if !checked {
                    check_new_identifiers(store, decl, &slots, &delta, &id_column)?;
                    checked = true;
                }
                let sealing = store.sealing();
                let entry = match (index.kind, entry) {
                    (IndexKind::Vector, IndexEntry::Vector(p)) => {
                        IndexEntry::Vector(crate::vector::extend(&staging, &snapshot_id, &delta, decl, index, &sealing, p, &s.snapshot_id)?)
                    }
                    (IndexKind::Fulltext, IndexEntry::Fulltext(p)) => {
                        IndexEntry::Fulltext(crate::fulltext::extend(&staging, &snapshot_id, &delta, decl, index, &sealing, p, &s.snapshot_id)?)
                    }
                    _ => unreachable!("an extendable entry matches its declaration's kind"),
                };
                indexes.push(entry);
                continue;
            }
            if whole.is_none() {
                let mut all = Vec::new();
                for s in &slots {
                    for b in conformed(store, &s.path, &target)? {
                        all.push(b?);
                    }
                }
                let rows = concat_batches(&target, &all).map_err(invalid)?;
                crate::vector::check_identifiers(&rows, decl)?;
                whole = Some(rows);
            }
            let rows = whole.as_ref().expect("the snapshot's rows are read");
            let sealing = store.sealing();
            indexes.push(match index.kind {
                IndexKind::Vector => IndexEntry::Vector(crate::vector::build(&staging, &snapshot_id, rows, decl, index, &sealing)?),
                IndexKind::Fulltext => IndexEntry::Fulltext(crate::fulltext::build(&staging, &snapshot_id, rows, decl, index, &sealing)?),
            });
        }
    }

    let mut sizes: BTreeMap<String, u64> = BTreeMap::new();
    for s in &slots {
        *sizes.entry(s.entry.partition().to_string()).or_default() += fs::metadata(&s.path).at(&s.path)?.len();
    }
    let row_count = slots.iter().map(Slot::rows).sum();
    Ok(Some(Output {
        snapshot_id,
        staging,
        in_flight,
        parts: slots.into_iter().map(|s| s.entry).collect(),
        indexes,
        row_count,
        retention,
        sizes: sizes.into_values().collect(),
    }))
}

/// Refuse a delta whose identifiers repeat among themselves or name a stored row
/// (`store.index.id-unique`). On a table whose dedup line is its identifier alone, a delta
/// row sharing a stored identifier replaced that row, so a pass extending a sidecar meets
/// none; any other table counts each delta identifier across the staged snapshot's parts.
fn check_new_identifiers(store: &Store, decl: &TableDecl, slots: &[Slot], delta: &RecordBatch, id_column: &str) -> Result<()> {
    crate::vector::check_identifiers(delta, decl)?;
    if decl.dedup_line() == [id_column.to_string()] {
        return Ok(());
    }
    let mut seen: HashMap<String, u32> =
        crate::vector::identifiers(delta, &decl.name, id_column)?.into_iter().flatten().map(|id| (id, 0)).collect();
    if seen.is_empty() {
        return Ok(());
    }
    let projection = vec![id_column.to_string()];
    for s in slots {
        for b in store.parquet_batches(&s.path, Some(&projection))? {
            let b = b?;
            if b.column_by_name(id_column).is_none() {
                continue;
            }
            for id in crate::vector::identifiers(&b, &decl.name, id_column)?.into_iter().flatten() {
                let Some(n) = seen.get_mut(&id) else { continue };
                *n += 1;
                if *n > 1 {
                    return Err(contextful_core::store::StoreError::StoreIndexIdNotUnique(format!(
                        "table `{}`: `id_column` `{id_column}` holds `{id}` on two rows of the snapshot; nothing was published",
                        decl.name
                    ))
                    .into());
                }
            }
        }
    }
    Ok(())
}
