//! `store.lay-out.folded-index`: the Parquet segments listing every run a snapshot's chain
//! folds, which a pass writes beside its snapshot and a read trusts in place of the chain walk.

use crate::error::{ContextError, IoPath, Result};
use crate::store::Store;
use arrow_array::{Array, ArrayRef, Int64Array, RecordBatch, StringArray, TimestampNanosecondArray, UInt64Array};
use arrow_schema::{DataType, Field, Schema, TimeUnit};
use contextful_core::run::journal::sha256_hex;
use contextful_core::store::lay_out::{FoldedIndex, FoldedRow, FoldedSegment, RunManifest, SnapshotId, SnapshotManifest};
use contextful_core::store::resolve::TableState;
use contextful_core::time::Instant;
use std::collections::BTreeMap;
use std::fs;
use std::path::Path;
use std::sync::Arc;

/// Segments of one size tier merge once this many share it (`store.fold.tiered-merge`).
const TIER_WIDTH: usize = 4;

/// The folded-runs index of snapshot `id`, which folds `unfolded` over `state`'s chain, its new
/// segments written under `staging`: the parent's valid index carried by reference with the
/// runs walked above it and the runs this pass folds, merged in tiers. `None` when the parent's
/// chain carries no valid index and its walk ends at a manifest no longer on disk
/// (`store.fold.folded-index`).
pub(crate) fn write(store: &Store, state: &TableState, id: &SnapshotId, unfolded: &[&RunManifest], staging: &Path) -> Result<Option<FoldedIndex>> {
    let present: BTreeMap<String, &RunManifest> = state.runs.iter().map(|r| (r.key(), r)).collect();
    // A part whose footer does not read leaves the row's count absent, as a run whose manifest is gone does.
    let row = |key: &str, snapshot: &SnapshotId, run: Option<&RunManifest>| FoldedRow {
        run_key: key.to_string(),
        snapshot_id: snapshot.clone(),
        committed_at: run.map(|r| r.committed_at),
        commit_seq: run.and_then(|r| r.commit_seq),
        batch_digest: run.and_then(|r| r.batch_digest.clone()),
        rows: run.and_then(|r| run_rows(store, &state.table, r).ok()),
    };
    let (mut segments, mut inline, walked): (Vec<FoldedSegment>, Vec<FoldedRow>, &[SnapshotManifest]) = match &state.index {
        // The index the walk stopped at, its segments carried by reference and its inline rows
        // copied, and the snapshots above it.
        Some(ix) => {
            let holder = &state.chain[ix.at];
            let index = holder.folded_runs.clone();
            let carried = index.as_ref().map(|f| f.segments.clone()).unwrap_or_default();
            let carried = carried.into_iter().map(|s| FoldedSegment { origin: Some(s.origin.clone().unwrap_or_else(|| holder.snapshot_id.clone())), ..s }).collect();
            (carried, index.map(|f| f.inline).unwrap_or_default(), &state.chain[..ix.at])
        }
        // No index: the whole chain, when the walk reached a root.
        None if state.chain.last().is_none_or(|s| s.parent.is_none()) => (Vec::new(), Vec::new(), &state.chain[..]),
        None => return Ok(None),
    };
    inline.extend(walked.iter().flat_map(|s| s.includes_runs.iter().map(|k| row(k, &s.snapshot_id, present.get(k).copied()))));
    inline.extend(unfolded.iter().map(|r| row(&r.key(), id, Some(r))));
    let dir = staging.join(FoldedIndex::DIR);
    let mut ordinal = 0usize;
    let mut fresh = |rows: Vec<FoldedRow>| -> Result<FoldedSegment> {
        let seg = write_segment(store, &dir, ordinal, rows)?;
        ordinal += 1;
        Ok(seg)
    };
    // Inline rows reaching the bound flush into a segment of their own.
    if inline.len() >= FoldedIndex::INLINE_MAX {
        segments.push(fresh(std::mem::take(&mut inline))?);
    }
    // Four segments of one tier merge into one, until every tier holds fewer than four.
    let table_dir = staging.parent().and_then(Path::parent).and_then(Path::parent).map(Path::to_path_buf).unwrap_or_default();
    loop {
        let mut tiers: BTreeMap<u32, Vec<usize>> = BTreeMap::new();
        for (i, s) in segments.iter().enumerate() {
            tiers.entry(tier(s.rows)).or_default().push(i);
        }
        let Some(group) = tiers.into_values().find(|g| g.len() >= TIER_WIDTH) else { break };
        let merging: Vec<usize> = group.into_iter().take(TIER_WIDTH).collect();
        let mut rows = Vec::new();
        for i in &merging {
            let s = &segments[*i];
            let path = match &s.origin {
                Some(_) => table_dir.join(s.location(id)),
                None => staging.join(&s.name),
            };
            rows.extend(read_segment(store, &path)?);
        }
        let merged = fresh(rows)?;
        let mut kept: Vec<FoldedSegment> = segments.into_iter().enumerate().filter(|(i, _)| !merging.contains(i)).map(|(_, s)| s).collect();
        kept.push(merged);
        segments = kept;
    }
    Ok(Some(FoldedIndex { through: id.clone(), segments, inline }))
}

/// The rows run `r`'s parts hold: each part's recorded count, else its footer's.
fn run_rows(store: &Store, table: &str, r: &RunManifest) -> Result<u64> {
    let mut total = 0u64;
    for p in &r.parts {
        let n = match p.rows {
            Some(n) => n,
            None => store.parquet_row_count(&store.logical_path(&format!("tables/{table}/{}/{}/{}/{}", contextful_core::store::lay_out::RUNS_DIR, r.run_id, r.node_id, p.name))?)?,
        };
        total = total.saturating_add(n);
    }
    Ok(total)
}

/// The size tier of a segment of `rows` rows: the power of 4 at or below it.
fn tier(rows: u64) -> u32 {
    rows.max(1).ilog(TIER_WIDTH as u64)
}

fn schema() -> Arc<Schema> {
    Arc::new(Schema::new(vec![
        Field::new("run_key", DataType::Utf8, false),
        Field::new("snapshot_id", DataType::Utf8, false),
        Field::new("committed_at", DataType::Timestamp(TimeUnit::Nanosecond, Some("UTC".into())), true),
        Field::new("commit_seq", DataType::Int64, true),
        Field::new("batch_digest", DataType::Utf8, true),
        Field::new("rows", DataType::UInt64, true),
    ]))
}

/// Write `rows`, sorted by run key, as segment `ordinal` under `dir`.
fn write_segment(store: &Store, dir: &Path, ordinal: usize, mut rows: Vec<FoldedRow>) -> Result<FoldedSegment> {
    rows.sort_by(|a, b| a.run_key.cmp(&b.run_key));
    fs::create_dir_all(dir).at(dir)?;
    let name = format!("segment-{ordinal:05}.runs");
    let path = dir.join(&name);
    let columns: Vec<ArrayRef> = vec![
        Arc::new(StringArray::from_iter_values(rows.iter().map(|r| r.run_key.as_str()))),
        Arc::new(StringArray::from_iter_values(rows.iter().map(|r| r.snapshot_id.to_string()))),
        Arc::new(TimestampNanosecondArray::from(rows.iter().map(|r| r.committed_at.and_then(|t| i64::try_from(t.unix_nanos()).ok())).collect::<Vec<_>>()).with_timezone("UTC")),
        Arc::new(Int64Array::from(rows.iter().map(|r| r.commit_seq).collect::<Vec<_>>())),
        Arc::new(StringArray::from(rows.iter().map(|r| r.batch_digest.as_deref()).collect::<Vec<_>>())),
        Arc::new(UInt64Array::from(rows.iter().map(|r| r.rows).collect::<Vec<_>>())),
    ];
    let batch = RecordBatch::try_new(schema(), columns).map_err(|e| ContextError::Parquet { path: path.clone(), message: e.to_string() })?;
    store.write_parquet(&path, &batch)?;
    let bytes = fs::read(&path).at(&path)?;
    Ok(FoldedSegment { name: format!("{}/{name}", FoldedIndex::DIR), origin: None, sha256: sha256_hex(&bytes), rows: rows.len() as u64 })
}

/// The rows of the segment at `path`.
fn read_segment(store: &Store, path: &Path) -> Result<Vec<FoldedRow>> {
    let invalid = |what: &str| ContextError::Parquet { path: path.to_path_buf(), message: format!("folded-runs segment: {what}") };
    let mut out = Vec::new();
    for b in store.read_parquet(path)? {
        let keys = b.column_by_name("run_key").and_then(|c| c.as_any().downcast_ref::<StringArray>()).ok_or_else(|| invalid("no run_key"))?;
        let ids = b.column_by_name("snapshot_id").and_then(|c| c.as_any().downcast_ref::<StringArray>()).ok_or_else(|| invalid("no snapshot_id"))?;
        let at = b.column_by_name("committed_at").and_then(|c| c.as_any().downcast_ref::<TimestampNanosecondArray>()).ok_or_else(|| invalid("no committed_at"))?;
        let seq = b.column_by_name("commit_seq").and_then(|c| c.as_any().downcast_ref::<Int64Array>()).ok_or_else(|| invalid("no commit_seq"))?;
        // A segment written before digests and row counts holds neither column.
        let digest = b.column_by_name("batch_digest").map(|c| c.as_any().downcast_ref::<StringArray>().ok_or_else(|| invalid("batch_digest is not text"))).transpose()?;
        let counts = b.column_by_name("rows").map(|c| c.as_any().downcast_ref::<UInt64Array>().ok_or_else(|| invalid("rows is not an unsigned count"))).transpose()?;
        for i in 0..b.num_rows() {
            out.push(FoldedRow {
                run_key: keys.value(i).to_string(),
                snapshot_id: SnapshotId::try_from(ids.value(i).to_string()).map_err(|e| invalid(&e))?,
                committed_at: (!at.is_null(i)).then(|| Instant::from_unix_nanos(i128::from(at.value(i)))).transpose().map_err(|e| invalid(&e.to_string()))?,
                commit_seq: (!seq.is_null(i)).then(|| seq.value(i)),
                batch_digest: digest.filter(|d| !d.is_null(i)).map(|d| d.value(i).to_string()),
                rows: counts.filter(|c| !c.is_null(i)).map(|c| c.value(i)),
            });
        }
    }
    Ok(out)
}

/// The rows of `m`'s folded-runs index when it is valid: `through` names `m`, and every
/// segment reads at its recorded digest. `None` otherwise, and the reader walks the chain
/// (`store.lay-out.folded-index-valid`).
pub(crate) fn read(store: &Store, table: &str, m: &SnapshotManifest) -> Option<BTreeMap<String, FoldedRow>> {
    let table_dir = store.table_dir(table).ok()?;
    validated(m, |location| {
        let path = table_dir.join(location);
        let bytes = fs::read(&path).ok()?;
        Some((sha256_hex(&bytes), read_segment(store, &path).ok()?))
    })
}

/// [`read`] over segments another copy of the store holds: `bytes_of` answers each segment's
/// bytes by its location relative to the table directory, and each decodes through a file
/// under `scratch`, removed after.
pub(crate) fn read_from(store: &Store, m: &SnapshotManifest, bytes_of: &dyn Fn(&str) -> Option<Vec<u8>>, scratch: &Path) -> Option<BTreeMap<String, FoldedRow>> {
    fs::create_dir_all(scratch).ok()?;
    validated(m, |location| {
        let bytes = bytes_of(location)?;
        let digest = sha256_hex(&bytes);
        let path = scratch.join(format!("{digest}.runs"));
        fs::write(&path, &bytes).ok()?;
        let rows = read_segment(store, &path).ok();
        let _ = fs::remove_file(&path);
        Some((digest, rows?))
    })
}

/// The index rows of `m` where `through` names `m` and every segment `segment` answers carries
/// its recorded digest and row count.
fn validated(m: &SnapshotManifest, segment: impl Fn(&str) -> Option<(String, Vec<FoldedRow>)>) -> Option<BTreeMap<String, FoldedRow>> {
    let index = m.folded_runs.as_ref().filter(|f| f.through == m.snapshot_id)?;
    let mut rows = BTreeMap::new();
    for seg in &index.segments {
        let (digest, read) = segment(&seg.location(&m.snapshot_id))?;
        if digest != seg.sha256 || read.len() as u64 != seg.rows {
            return None;
        }
        rows.extend(read.into_iter().map(|r| (r.run_key.clone(), r)));
    }
    rows.extend(index.inline.iter().map(|r| (r.run_key.clone(), r.clone())));
    Some(rows)
}
