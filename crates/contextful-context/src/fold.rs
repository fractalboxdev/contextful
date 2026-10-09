//! The fold: a pass over one table that merges the committed runs its current snapshot
//! omits into a new snapshot, then publishes it by a version-checked pointer replace.

use crate::error::{ContextError, IoPath, Result};
use crate::parquet_io;
use crate::store::{FileLock, Store};
use arrow_array::{Array, ArrayRef, BooleanArray, RecordBatch, StringArray, TimestampNanosecondArray, UInt32Array};
use arrow_ord::sort::{lexsort_to_indices, SortColumn, SortOptions};
use arrow_row::{RowConverter, SortField};
use arrow_select::concat::concat_batches;
use arrow_select::filter::filter_record_batch;
use arrow_select::take::take_record_batch;
use contextful_core::store::declare::{TableDecl, WriteMode};
use contextful_core::store::fold::{FoldOutcome, RetentionReport};
use contextful_core::store::index::{IndexEntry, IndexKind};
pub use contextful_core::store::lay_out::escape;
use contextful_core::store::lay_out::{
    part_name, PartEntry, Pointer, RunManifest, SnapshotId, SnapshotManifest, MANIFEST_FILE, POINTER_FILE, STAGING_SUFFIX,
};
use contextful_core::run::derive::emit::{superseded, superseded_within_version, SUPERSEDE_COLUMNS};
use contextful_core::run::derive::task::{is_derive_key, TASK_VERSION};
use contextful_core::store::reserve::TIEBREAK;
use contextful_core::store::StoreError;
use contextful_core::time::Instant;
use parquet::arrow::arrow_reader::ParquetRecordBatchReaderBuilder;
use parquet::basic::{LogicalType, TimeUnit};
use parquet::file::statistics::Statistics;
use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, File};
use std::path::PathBuf;

/// The directory name a partition value takes when null.
pub const NULL_PARTITION: &str = "__HIVE_DEFAULT_PARTITION__";

/// How long a commit waits for the pointer lock before reading contention as a lost
/// condition. The lock spans a pointer read and a replace, so a live holder clears it in
/// well under this.
pub const POINTER_LOCK_WAIT: std::time::Duration = std::time::Duration::from_millis(250);

/// A snapshot written into staging and not yet published.
#[derive(Debug, Clone)]
pub struct Staged {
    pub table: String,
    pub manifest: SnapshotManifest,
    pub staging: PathBuf,
    /// The pointer's ETag when the pass started.
    pub etag: String,
    pub runs: usize,
    pub retention: Option<RetentionReport>,
    /// What the partition plan warns about (`store.index.partition-warnings`).
    pub warnings: Vec<String>,
    /// Held for the pass's life, so a concurrent pass's collection reads this staging
    /// directory as in flight rather than as one an earlier pass abandoned.
    pub(crate) _in_flight: Option<std::sync::Arc<FileLock>>,
}

/// The lock file inside a staging directory that marks its pass as in flight.
pub const STAGING_LOCK: &str = "_staging.lock";

/// What a pass's first half produced.
#[derive(Debug, Clone)]
pub enum Prepared {
    NothingLanded,
    Staged(Box<Staged>),
}

/// What a pointer replace did.
#[derive(Debug, Clone, PartialEq)]
pub enum Committed {
    Published(Box<SnapshotManifest>),
    /// The pointer moved since the pass started; nothing was published
    /// (`store.fold.lost-pointer`).
    Lost,
}

/// One pass over one table: stage, publish, then collect what retention and earlier
/// passes left behind. Retention ages from the fold, not from a landing
/// (`store.fold.retention`), so a pass with nothing to fold still collects. A collection
/// that fails is reported, beside a published snapshot that stays published, since the
/// pointer has already moved (`store.fold.collection-failed`).
pub fn fold(store: &Store, decl: &TableDecl, now: Instant) -> Result<FoldOutcome> {
    fold_under(store, decl, now, None)
}

/// [`fold`] under a compaction lease: the snapshot manifest and the pointer it publishes
/// carry the lease's `fence` (`store.fold.compaction-lease`).
pub fn fold_under(store: &Store, decl: &TableDecl, now: Instant, fence: Option<u64>) -> Result<FoldOutcome> {
    store.check_writable("compact")?;
    match prepare_under(store, decl, now, fence)? {
        Prepared::NothingLanded => Ok(match merge_ledgers(store, decl, now).and_then(|_| collect(store, decl, now)) {
            Ok(collected) => match decl.retain_rows_secs().map_err(|e| ContextError::Invalid(e.to_string()))? {
                Some(age) => FoldOutcome::NothingLandedRetained { cutoff: now.minus_secs(age), collected },
                None => FoldOutcome::NothingLanded,
            },
            Err(e) => FoldOutcome::Failed(format!("nothing to fold; collection failed: {e}")),
        }),
        Prepared::Staged(staged) => {
            let runs = staged.runs;
            let retention = staged.retention.clone();
            let warnings = staged.warnings.clone();
            match commit(store, *staged)? {
                Committed::Published(m) => {
                    let collected = merge_ledgers(store, decl, now).and_then(|_| collect(store, decl, now));
                    Ok(FoldOutcome::Folded {
                    snapshot_id: m.snapshot_id.to_string(),
                    runs,
                    rows: m.row_count,
                    retention,
                    collected: collected.as_ref().cloned().unwrap_or_default(),
                    collection: collected.err().map(|e| e.to_string()),
                    warnings,
                })},
                Committed::Lost => Ok(FoldOutcome::Failed("the pointer moved during the pass; nothing was published".into())),
            }
        }
    }
}

/// Merge the table's committed request ledgers under the snapshot its pointer names, before
/// collection removes the run manifests that date them (`store.reserve.ledger-fold`).
fn merge_ledgers(store: &Store, decl: &TableDecl, now: Instant) -> Result<()> {
    if let Some((pointer, _)) = store.pointer(&decl.name)? {
        crate::ledger::fold_ledgers(store, &decl.name, &pointer.snapshot_id.to_string(), now)?;
    }
    Ok(())
}

/// Stage the next snapshot: select the committed runs the current snapshot omits,
/// dedupe by key or union, reconcile to the merged schema, sort by `cluster_by`,
/// partition, and write Parquet and the manifest under `<id>.staging/`
/// (`store.fold.pass`).
pub fn prepare(store: &Store, decl: &TableDecl, now: Instant) -> Result<Prepared> {
    prepare_under(store, decl, now, None)
}

/// [`prepare`] with the compaction lease's `fence` stamped into the staged manifest.
pub fn prepare_under(store: &Store, decl: &TableDecl, now: Instant, fence: Option<u64>) -> Result<Prepared> {
    let table = decl.name.as_str();
    // The pointer, then the runs, then the schema: a landing writes its schema before
    // its manifest, so every run read here has its columns in the schema read after. A
    // table no schema declares refuses there, whatever the runs read found.
    let etag = store.pointer_etag(table)?;
    let state = store.state(decl)?;
    let schema = store.schema(table)?;
    decl.validate(&schema)?;
    decl.validate_indexes(&schema)?;

    let unfolded_runs = state.unfolded_runs();
    let unfolded: Vec<String> = unfolded_runs.iter().map(|r| r.key()).collect();
    if unfolded.is_empty() && decl.retain_rows.is_none() {
        return Ok(Prepared::NothingLanded);
    }
    let table_dir = store.table_dir(table)?;
    let inputs: Vec<String> = match decl.write_mode() {
        WriteMode::Replace => state.resolve(None)?.files(),
        WriteMode::Append => {
            let mut files = Vec::new();
            if let Some(s) = state.chain.first() {
                files.extend(s.parts.iter().map(|p| format!("data/snapshots/{}/{}", s.snapshot_id, p.name)));
            }
            for r in &unfolded_runs {
                files.extend(r.parts.iter().map(|p| format!("data/runs/{}/{}/{}", r.run_id, r.node_id, p.name)));
            }
            files
        }
    };

    let target = parquet_io::arrow_schema(&schema);
    let mut batches = Vec::new();
    let mut footer_expired = 0;
    let mut footer_partitions = BTreeSet::new();
    let cutoff = decl.retain_rows_secs().map_err(|e| ContextError::Invalid(e.to_string()))?.map(|age| now.minus_secs(age));
    for f in &inputs {
        let path = table_dir.join(f);
        let partition_dir = f.split('/').filter(|part| decl.partition_by().iter().any(|key| part.starts_with(&format!("{key}=")))).collect::<Vec<_>>().join("/");
        // A keyed file can hold the winner that suppresses a live older row.
        // Its rows must reach dedupe before retention can discard the winner.
        if store.parquet_key().is_none() && !decl.is_keyed() && (decl.partition_by().is_empty() || !partition_dir.is_empty()) {
            if let (Some(rule), Some(cutoff)) = (&decl.retain_rows, cutoff) {
                if let Some(n) = footer_expired_rows(&path, &rule.column, cutoff)? {
                    footer_expired += n;
                    if !decl.partition_by().is_empty() { footer_partitions.insert(partition_dir); }
                    continue;
                }
            }
        }
        for b in store.read_parquet(&path)? {
            batches.push(parquet_io::conform(&b, &target)?);
        }
    }
    let invalid = |e: arrow_schema::ArrowError| ContextError::Invalid(format!("table `{table}`: {e}"));
    let mut rows = concat_batches(&target, &batches).map_err(invalid)?;
    if decl.is_keyed() {
        let mut line: Vec<String> = decl.primary_key().to_vec();
        if let Some(vt) = &decl.valid_time {
            line.push(vt.from.clone());
        }
        rows = dedupe(&rows, &line, decl.order_by()).map_err(invalid)?;
    }
    if is_derive_key(decl.primary_key()) {
        rows = supersede(&rows, decl.retain_versions == Some(true))?;
    }
    let retention = if let Some(rule) = &decl.retain_rows {
        let cutoff = cutoff.expect("declared retention has a parsed age");
        let column = column(&rows, &rule.column).map_err(invalid)?;
        let times = column.as_any().downcast_ref::<TimestampNanosecondArray>().ok_or_else(|| {
            ContextError::Invalid(format!("table `{table}`: retention column `{}` is not a nanosecond timestamp", rule.column))
        })?;
        let keep: BooleanArray = (0..rows.num_rows()).map(|i| Some(!times.is_null(i) && i128::from(times.value(i)) >= cutoff.unix_nanos())).collect();
        let count = keep.iter().filter(|v| *v == Some(false)).count() as u64;
        let filtered = filter_record_batch(&rows, &keep).map_err(invalid)?;
        let mut before: BTreeSet<String> = if decl.partition_by().is_empty() { BTreeSet::new() } else {
            partition(&rows, decl.partition_by()).map_err(invalid)?.into_iter().map(|(name, _)| name).collect()
        };
        before.extend(footer_partitions);
        let after: BTreeSet<String> = if decl.partition_by().is_empty() { BTreeSet::new() } else {
            partition(&filtered, decl.partition_by()).map_err(invalid)?.into_iter().map(|(name, _)| name).collect()
        };
        rows = filtered;
        Some(RetentionReport { cutoff, rows_expired: count + footer_expired, partitions_dropped: before.difference(&after).count() as u64 })
    } else { None };
    if unfolded.is_empty() && retention.as_ref().is_none_or(|r| r.rows_expired == 0) {
        return Ok(Prepared::NothingLanded);
    }
    // A keyed table declaring no clustering sorts by its key, so each part's footer bounds
    // on the key form a sparse map a point lookup probes (`store.index.kinds`).
    if !decl.cluster_by().is_empty() {
        rows = sort(&rows, decl.cluster_by()).map_err(invalid)?;
    } else if decl.is_keyed() {
        rows = sort(&rows, decl.primary_key()).map_err(invalid)?;
    }
    crate::vector::check_identifiers(&rows, decl)?;

    let parent = state.chain.first().map(|s| s.snapshot_id.clone());
    let (snapshot_id, staging, in_flight) = claim(store, table, SnapshotId::next(now, parent.as_ref()), now)?;
    // Every declared sidecar lands inside staging beside the Parquet, so the rename that
    // publishes the snapshot publishes its sidecars with it (`store.fold.partial-snapshot`).
    let mut indexes = Vec::new();
    for index in decl.indexes() {
        let entry = match index.kind {
            IndexKind::Vector => IndexEntry::Vector(crate::vector::build(&staging, &snapshot_id, &rows, decl, index, &store.sealing())?),
            IndexKind::Fulltext => IndexEntry::Fulltext(crate::fulltext::build(&staging, &snapshot_id, &rows, decl, index, &store.sealing())?),
        };
        indexes.push(entry);
    }
    let mut parts = Vec::new();
    let mut sizes = Vec::new();
    if rows.num_rows() > 0 {
        for (dir, batch) in partition(&rows, decl.partition_by()).map_err(invalid)? {
            let name = if dir.is_empty() { part_name(0) } else { format!("{dir}/{}", part_name(0)) };
            let path = staging.join(&name);
            store.write_parquet_blooming(&path, &batch, decl.bloom_filter())?;
            sizes.push(fs::metadata(&path).at(&path)?.len());
            parts.push(PartEntry { name, key_version: store.sealing().key_version() });
        }
    }
    let warnings = contextful_core::store::index::partition_warnings(decl.partition_by(), &sizes);
    let manifest = SnapshotManifest {
        snapshot_id,
        parent,
        ancestors: SnapshotManifest::ancestors_after(state.chain.first()),
        table: table.to_string(),
        created_at: now,
        includes_runs: unfolded.clone(),
        primary_key: decl.primary_key().to_vec(),
        order_by: Some(decl.order_by().to_string()),
        row_count: rows.num_rows() as u64,
        valid_time: decl.valid_time.clone(),
        parts,
        indexes,
        fence,
        commit_seq: unfolded_runs.iter().map(|r| r.commit_seq).chain(state.chain.first().map(|s| s.commit_seq)).flatten().max(),
        publish: None,
    };
    let bytes = serde_json::to_vec_pretty(&manifest).expect("a manifest serializes");
    store.metadata().write(&staging.join(MANIFEST_FILE), &bytes)?;
    Ok(Prepared::Staged(Box::new(Staged {
        table: table.to_string(),
        manifest,
        staging,
        etag,
        runs: unfolded.len(),
        retention,
        warnings,
        _in_flight: Some(std::sync::Arc::new(in_flight)),
    })))
}

/// A whole part whose exact footer maximum precedes the cutoff needs no row read.
/// Unknown, coarse or inexact statistics fall back to the ordinary row filter.
fn footer_expired_rows(path: &std::path::Path, column: &str, cutoff: Instant) -> Result<Option<u64>> {
    let file = File::open(path).at(path)?;
    let metadata = ParquetRecordBatchReaderBuilder::try_new(file)
        .map_err(|e| ContextError::Parquet { path: path.to_path_buf(), message: e.to_string() })?
        .metadata().clone();
    let schema = metadata.file_metadata().schema_descr();
    let Some(index) = schema.columns().iter().position(|c| {
        c.path().parts().len() == 1 && c.name() == column && matches!(c.logical_type_ref(), Some(LogicalType::Timestamp { unit: TimeUnit::NANOS, .. }))
    }) else { return Ok(None) };
    let mut rows = 0_u64;
    for group in metadata.row_groups() {
        let Some(Statistics::Int64(stats)) = group.column(index).statistics() else { return Ok(None) };
        let Some(max) = stats.max_opt() else { return Ok(None) };
        if !stats.max_is_exact() || i128::from(*max) >= cutoff.unix_nanos() {
            return Ok(None);
        }
        rows += u64::try_from(group.num_rows()).unwrap_or_default();
    }
    Ok(Some(rows))
}

/// Ids a pass tries past its first candidate before refusing.
pub const CLAIM_ATTEMPTS: usize = 64;

/// Claim a snapshot id for a pass: the first id from `first` up whose snapshot and staging
/// directories are both absent, its staging directory created and its in-flight lock held.
/// A pass removes no directory it did not create, so two passes computing one id each
/// stage under their own, and the pointer decides which publishes.
pub(crate) fn claim(store: &Store, table: &str, first: SnapshotId, now: Instant) -> Result<(SnapshotId, PathBuf, FileLock)> {
    let snapshots = store.table_dir(table)?.join(contextful_core::store::lay_out::SNAPSHOTS_DIR);
    fs::create_dir_all(&snapshots).at(&snapshots)?;
    let mut id = first.clone();
    for _ in 0..CLAIM_ATTEMPTS {
        let staging = snapshots.join(format!("{id}{STAGING_SUFFIX}"));
        if !store.snapshot_dir(table, &id)?.exists() {
            match fs::create_dir(&staging) {
                // A collection may take the directory before its lock does; the pass moves on.
                Ok(()) => match FileLock::try_acquire(&staging.join(STAGING_LOCK)) {
                    Ok(Some(lock)) => return Ok((id, staging, lock)),
                    Ok(None) | Err(ContextError::Io { .. }) => {}
                    Err(e) => return Err(e),
                },
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
                Err(e) => return Err(ContextError::Io { path: staging, source: e }),
            }
        }
        id = SnapshotId::next(now, Some(&id));
    }
    Err(ContextError::Invalid(format!(
        "table `{table}`: {CLAIM_ATTEMPTS} snapshot ids from {first} are taken by other passes; nothing was staged"
    )))
}

/// Publish a staged snapshot: check it is whole, move it out of staging, and replace
/// `_pointer.json` only if its ETag still equals the one read at pass start — on a
/// filesystem, a replace under an exclusive lock file that re-checks the ETag
/// (`store.fold.pointer-commit`).
pub fn commit(store: &Store, mut staged: Staged) -> Result<Committed> {
    let table = staged.table.clone();
    let table = table.as_str();
    for p in &staged.manifest.parts {
        if !staged.staging.join(&p.name).is_file() {
            return Err(StoreError::StorePartialSnapshot(format!(
                "table `{table}`: snapshot {} names part `{}`, which is not in its directory; nothing was published",
                staged.manifest.snapshot_id, p.name
            ))
            .into());
        }
    }
    for idx in &staged.manifest.indexes {
        let path = idx.path().unwrap_or_default();
        if path.is_empty() || !staged.staging.join(path).exists() {
            return Err(StoreError::StorePartialSnapshot(format!(
                "table `{table}`: snapshot {} declares a sidecar absent from its directory; nothing was published",
                staged.manifest.snapshot_id
            ))
            .into());
        }
    }
    // The snapshot is whole, so the pass leaves staging: its in-flight mark goes first,
    // and the directory that moves carries data alone.
    staged._in_flight = None;
    let _ = fs::remove_file(staged.staging.join(STAGING_LOCK));

    // A directory already at the id belongs to another pass; this one publishes nothing
    // and leaves both directories for collection.
    let final_dir = store.snapshot_dir(table, &staged.manifest.snapshot_id)?;
    if final_dir.exists() {
        return Err(ContextError::Invalid(format!(
            "table `{table}`: snapshot {} is already in place from another pass; nothing was published",
            staged.manifest.snapshot_id
        )));
    }
    fs::rename(&staged.staging, &final_dir).at(&final_dir)?;

    let pointer = Pointer { snapshot_id: staged.manifest.snapshot_id.clone(), fence: staged.manifest.fence };
    let bytes = serde_json::to_vec_pretty(&pointer).expect("a pointer serializes");
    let table_dir = store.table_dir(table)?;
    // A held lock is another pass inside its own commit, not a verdict on this one: wait
    // briefly, since the lock spans a pointer read and a replace. The ETag then answers
    // whether that pass moved the pointer, so contention and the condition stay distinct.
    let lock_path = table_dir.join(format!("{POINTER_FILE}.lock"));
    let Some(_lock) = FileLock::acquire_within(&lock_path, POINTER_LOCK_WAIT)? else {
        return Ok(Committed::Lost);
    };
    if store.pointer_etag(table)? != staged.etag {
        return Ok(Committed::Lost);
    }
    store.metadata().replace(&table_dir.join(POINTER_FILE), &bytes)?;
    Ok(Committed::Published(Box::new(staged.manifest)))
}

/// Remove every staging directory and every snapshot directory no pointer chain
/// reaches whose id precedes the current snapshot's (`store.fold.staging-collected`).
/// A staging directory whose pass still holds its lock is in flight, whatever id it
/// carries, and survives until that pass ends.
fn collect_unreachable(store: &Store, table: &str, chain: &[SnapshotManifest], held: &BTreeSet<String>) -> Result<Vec<String>> {
    let Some(current) = chain.first() else { return Ok(Vec::new()) };
    let mut removed = Vec::new();
    let reachable: BTreeSet<String> = chain.iter().map(|s| s.snapshot_id.to_string()).collect();
    let snapshots = store.table_dir(table)?.join(contextful_core::store::lay_out::SNAPSHOTS_DIR);
    for dir in crate::store::sorted_dirs(&snapshots)? {
        let name = dir.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        if reachable.contains(&name) || held.contains(&name) {
            continue;
        }
        let id = SnapshotId::try_from(name.trim_end_matches(STAGING_SUFFIX).to_string()).ok();
        if id.is_some_and(|id| id >= current.snapshot_id) {
            continue;
        }
        let _held = if name.ends_with(STAGING_SUFFIX) {
            match FileLock::try_acquire(&dir.join(STAGING_LOCK))? {
                Some(lock) => Some(lock),
                None => continue,
            }
        } else {
            None
        };
        fs::remove_dir_all(&dir).at(&dir)?;
        removed.push(format!("data/snapshots/{name}"));
    }
    Ok(removed)
}

/// Collect what the `retain_runs` window allows (`store.fold.retention`): a snapshot
/// superseded longer ago than the window, with its sidecars, and a run folded longer
/// ago than the window. Both age from the same fold, so a bounded read never meets a
/// snapshot whose omitted runs are gone.
///
/// A snapshot an unexpired hold names stays on disk whatever the window and whatever
/// collection took around it (`run.publish.hold`).
pub fn collect(store: &Store, decl: &TableDecl, now: Instant) -> Result<Vec<String>> {
    let table = decl.name.as_str();
    let held = &crate::build::held(store, table, now)?;
    let window = decl.retain_runs_secs().map_err(|e| ContextError::Invalid(e.to_string()))?;
    let cutoff = now.minus_secs(window);
    let (chain, _) = store.chain(table)?;
    let mut removed = collect_unreachable(store, table, &chain, held)?;
    let runs: BTreeMap<String, RunManifest> = store.committed_runs(table)?.into_iter().map(|r| (r.key(), r)).collect();
    let runs_dir = store.table_dir(table)?.join(contextful_core::store::lay_out::RUNS_DIR);
    for pair in chain.windows(2) {
        let (successor, superseded) = (&pair[0], &pair[1]);
        if successor.created_at <= cutoff && !held.contains(&superseded.snapshot_id.to_string()) {
            let dir = store.snapshot_dir(table, &superseded.snapshot_id)?;
            if dir.exists() {
                fs::remove_dir_all(&dir).at(&dir)?;
                removed.push(format!("data/snapshots/{}", superseded.snapshot_id));
            }
        }
    }
    for s in &chain {
        if s.created_at > cutoff {
            continue;
        }
        for r in s.includes_runs.iter().filter_map(|key| runs.get(key)) {
            let dir = runs_dir.join(&r.run_id);
            let node = dir.join(&r.node_id);
            if node.exists() {
                fs::remove_dir_all(&node).at(&node)?;
                removed.push(format!("data/runs/{}/{}", r.run_id, r.node_id));
            }
            if fs::read_dir(&dir).is_ok_and(|mut d| d.next().is_none()) {
                fs::remove_dir(&dir).at(&dir)?;
            }
        }
    }
    removed.sort();
    Ok(removed)
}

fn column<'a>(b: &'a RecordBatch, name: &str) -> std::result::Result<&'a ArrayRef, arrow_schema::ArrowError> {
    b.column_by_name(name).ok_or_else(|| arrow_schema::ArrowError::SchemaError(format!("no column `{name}`")))
}

/// Sort ascending over `keys` in declared order, nulls last (`store.index.clustering`).
fn sort(b: &RecordBatch, keys: &[String]) -> std::result::Result<RecordBatch, arrow_schema::ArrowError> {
    let cols: Vec<SortColumn> = keys
        .iter()
        .map(|k| Ok(SortColumn { values: column(b, k)?.clone(), options: Some(SortOptions { descending: false, nulls_first: false }) }))
        .collect::<std::result::Result<_, arrow_schema::ArrowError>>()?;
    let idx = lexsort_to_indices(&cols, None)?;
    take_record_batch(b, &idx)
}

/// Keep one row per `line`: the greatest `order_by`, then the greatest of each tiebreak
/// column in turn, nulls losing — the same survivor the dedup view picks
/// (`store.declare.dedup-view`).
fn dedupe(b: &RecordBatch, line: &[String], order_by: &str) -> std::result::Result<RecordBatch, arrow_schema::ArrowError> {
    let asc = SortOptions { descending: false, nulls_first: false };
    let desc = SortOptions { descending: true, nulls_first: false };
    let mut cols: Vec<SortColumn> = Vec::new();
    for k in line {
        cols.push(SortColumn { values: column(b, k)?.clone(), options: Some(asc) });
    }
    cols.push(SortColumn { values: column(b, order_by)?.clone(), options: Some(desc) });
    for c in TIEBREAK.into_iter().filter(|c| *c != order_by) {
        cols.push(SortColumn { values: column(b, c)?.clone(), options: Some(desc) });
    }
    let idx = lexsort_to_indices(&cols, None)?;
    let sorted = take_record_batch(b, &idx)?;
    let key_cols: Vec<ArrayRef> = line.iter().map(|k| column(&sorted, k).cloned()).collect::<std::result::Result<_, _>>()?;
    let conv = RowConverter::new(key_cols.iter().map(|c| SortField::new(c.data_type().clone())).collect())?;
    let rows = conv.convert_columns(&key_cols)?;
    let keep: BooleanArray = (0..sorted.num_rows()).map(|i| Some(i == 0 || rows.row(i) != rows.row(i - 1))).collect();
    filter_record_batch(&sorted, &keep)
}

/// Drop a derive table's superseded rows, so the snapshot and every sidecar built over it
/// hold each unit's rows under its latest established key alone (`run.emit.stale-supersedes`).
fn supersede(b: &RecordBatch, retain_versions: bool) -> Result<RecordBatch> {
    let mut columns = SUPERSEDE_COLUMNS.to_vec();
    if retain_versions {
        columns.push(TASK_VERSION);
    }
    let standing = crate::rows::batch_rows(b, &columns)?;
    let flags = if retain_versions { superseded_within_version(&standing) } else { superseded(&standing) };
    let keep: BooleanArray = flags.into_iter().map(|s| Some(!s)).collect();
    filter_record_batch(b, &keep).map_err(|e| ContextError::Invalid(format!("superseding derived rows: {e}")))
}

/// Split rows by the `partition_by` columns, outermost first, into `col=value`
/// directories; a value is written byte for byte, percent-escaped only in its directory
/// name (`store.index.tenant-verbatim`).
fn partition(b: &RecordBatch, by: &[String]) -> std::result::Result<Vec<(String, RecordBatch)>, arrow_schema::ArrowError> {
    if by.is_empty() {
        return Ok(vec![(String::new(), b.clone())]);
    }
    let texts: Vec<StringArray> = by
        .iter()
        .map(|k| {
            let c = arrow_cast::cast(column(b, k)?, &arrow_schema::DataType::Utf8)?;
            Ok(c.as_any().downcast_ref::<StringArray>().expect("cast to Utf8").clone())
        })
        .collect::<std::result::Result<_, arrow_schema::ArrowError>>()?;
    let mut groups: BTreeMap<String, Vec<u32>> = BTreeMap::new();
    for i in 0..b.num_rows() {
        let dir: Vec<String> = by
            .iter()
            .zip(&texts)
            .map(|(k, t)| {
                let v = if t.is_null(i) { NULL_PARTITION.to_string() } else { escape(t.value(i)) };
                format!("{k}={v}")
            })
            .collect();
        groups.entry(dir.join("/")).or_default().push(i as u32);
    }
    groups
        .into_iter()
        .map(|(dir, idx)| Ok((dir, take_record_batch(b, &UInt32Array::from(idx))?)))
        .collect()
}

/// The staging directory for a snapshot id, for callers that inspect a pass in flight.
pub fn staging_dir(store: &Store, table: &str, id: &SnapshotId) -> Result<PathBuf> {
    Ok(store.table_dir(table)?.join(contextful_core::store::lay_out::SNAPSHOTS_DIR).join(format!("{id}{STAGING_SUFFIX}")))
}
