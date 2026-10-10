//! A leased pipeline's commit log under the store root: `cursors/<pipeline-id>/<node-id>/<seq>.json`,
//! each entry created only where none holds its sequence number. The log is the node's
//! own: its fences come from the node's machine lease and compare within that node alone.

use crate::error::{IoPath, Result};
use crate::store::Store;
use contextful_core::store::commit_log::{admit, admit_kind, file_name, parse_seq, CommitEntry, Kind};
use std::path::PathBuf;

fn dir(store: &Store, pipeline_id: &str, node_id: &str) -> Result<PathBuf> {
    use contextful_core::store::lay_out::is_path_segment;
    (is_path_segment(pipeline_id) && is_path_segment(node_id))
        .then(|| store.root().join(contextful_core::store::lay_out::commit_log_dir(pipeline_id, node_id)))
        .ok_or_else(|| crate::ContextError::Invalid(format!("`{pipeline_id}/{node_id}` is not a pair of path segments")))
}

/// Every entry of the pipeline's log, in sequence order, each with its sequence number.
pub fn read_numbered(store: &Store, pipeline_id: &str, node_id: &str) -> Result<Vec<(u64, CommitEntry)>> {
    let d = dir(store, pipeline_id, node_id)?;
    let entries = match std::fs::read_dir(&d) {
        Ok(e) => e,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(crate::ContextError::Io { path: d, source: e }),
    };
    let mut out = Vec::new();
    for entry in entries {
        let path = entry.at(&d)?.path();
        let Some(seq) = path.file_name().and_then(|n| parse_seq(&n.to_string_lossy())) else { continue };
        let bytes = store.metadata().read(&path)?;
        let e: CommitEntry = serde_json::from_slice(&bytes).map_err(|e| crate::ContextError::Invalid(format!("{}: {e}", path.display())))?;
        out.push((seq, e));
    }
    out.sort_by_key(|(s, _)| *s);
    Ok(out)
}

/// Every entry of the pipeline's log, in sequence order.
pub fn read(store: &Store, pipeline_id: &str, node_id: &str) -> Result<Vec<CommitEntry>> {
    Ok(read_numbered(store, pipeline_id, node_id)?.into_iter().map(|(_, e)| e).collect())
}

/// Append `entry` at the next free sequence number. An entry for the table carrying a
/// higher fence, read before the create or found holding the slot, refuses with
/// `LeaseFenced`; the create is the commit point.
pub fn append(store: &Store, pipeline_id: &str, node_id: &str, entry: &CommitEntry) -> Result<u64> {
    admit_kind(pipeline_id, entry)?;
    let d = dir(store, pipeline_id, node_id)?;
    std::fs::create_dir_all(&d).at(&d)?;
    loop {
        let log = read_numbered(store, pipeline_id, node_id)?;
        let entries: Vec<CommitEntry> = log.iter().map(|(_, e)| e.clone()).collect();
        admit(&entries, &entry.table, entry.fence)?;
        let seq = log.last().map_or(1, |(s, _)| s + 1);
        let bytes = serde_json::to_vec_pretty(entry).map_err(|e| crate::ContextError::Invalid(e.to_string()))?;
        if store.metadata().create_new(&d.join(file_name(seq)), &bytes)? {
            return Ok(seq);
        }
    }
}

/// Record a lease acquisition under `fence` for `table`.
pub fn open_fence(store: &Store, pipeline_id: &str, node_id: &str, table: &str, fence: u64) -> Result<u64> {
    append(store, pipeline_id, node_id, &CommitEntry { kind: Kind::Acquire, table: table.to_string(), run_id: None, cursor: None, cursor_kind: None, fence })
}
