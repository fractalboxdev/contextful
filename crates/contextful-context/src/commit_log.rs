//! A leased pipeline's commit log under the store root: `cursors/<pipeline-id>/<seq>.json`,
//! each entry created only where none holds its sequence number.

use crate::error::{IoPath, Result};
use crate::store::{create_new_file, Store};
use contextful_core::store::commit_log::{admit, file_name, parse_seq, CommitEntry, Kind};
use std::path::PathBuf;

fn dir(store: &Store, pipeline_id: &str) -> Result<PathBuf> {
    contextful_core::store::lay_out::is_path_segment(pipeline_id)
        .then(|| store.root().join("cursors").join(pipeline_id))
        .ok_or_else(|| crate::ContextError::Invalid(format!("pipeline id `{pipeline_id}` is not a path segment")))
}

/// Every entry of the pipeline's log, in sequence order, each with its sequence number.
pub fn read_numbered(store: &Store, pipeline_id: &str) -> Result<Vec<(u64, CommitEntry)>> {
    let d = dir(store, pipeline_id)?;
    let entries = match std::fs::read_dir(&d) {
        Ok(e) => e,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(crate::ContextError::Io { path: d, source: e }),
    };
    let mut out = Vec::new();
    for entry in entries {
        let path = entry.at(&d)?.path();
        let Some(seq) = path.file_name().and_then(|n| parse_seq(&n.to_string_lossy())) else { continue };
        let text = std::fs::read_to_string(&path).at(&path)?;
        let e: CommitEntry = serde_json::from_str(&text).map_err(|e| crate::ContextError::Invalid(format!("{}: {e}", path.display())))?;
        out.push((seq, e));
    }
    out.sort_by_key(|(s, _)| *s);
    Ok(out)
}

/// Every entry of the pipeline's log, in sequence order.
pub fn read(store: &Store, pipeline_id: &str) -> Result<Vec<CommitEntry>> {
    Ok(read_numbered(store, pipeline_id)?.into_iter().map(|(_, e)| e).collect())
}

/// Append `entry` at the next free sequence number. An entry for the table carrying a
/// higher fence, read before the create or found holding the slot, refuses with
/// `LeaseFenced`; the create is the commit point.
pub fn append(store: &Store, pipeline_id: &str, entry: &CommitEntry) -> Result<u64> {
    let d = dir(store, pipeline_id)?;
    std::fs::create_dir_all(&d).at(&d)?;
    loop {
        let log = read_numbered(store, pipeline_id)?;
        let entries: Vec<CommitEntry> = log.iter().map(|(_, e)| e.clone()).collect();
        admit(&entries, &entry.table, entry.fence)?;
        let seq = log.last().map_or(1, |(s, _)| s + 1);
        let bytes = serde_json::to_vec_pretty(entry).map_err(|e| crate::ContextError::Invalid(e.to_string()))?;
        if create_new_file(&d.join(file_name(seq)), &bytes)? {
            return Ok(seq);
        }
    }
}

/// Record a lease acquisition under `fence` for `table`.
pub fn open_fence(store: &Store, pipeline_id: &str, table: &str, fence: u64) -> Result<u64> {
    append(store, pipeline_id, &CommitEntry { kind: Kind::Acquire, table: table.to_string(), run_id: None, cursor: None, fence })
}
