//! A leased pipeline's commit log: `cursors/<pipeline-id>/<seq>.json`, each entry created
//! only where none holds its sequence number. An acquisition writes an entry carrying its
//! fence, so a holder the next acquisition fenced out finds its own commit's slot taken
//! and the higher fence in the log (`store.lease.stale-fence`).

use super::StoreError;
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// What an entry records.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    /// A lease was taken under `fence`.
    Acquire,
    /// A run committed its rows and position under `fence`.
    Commit,
}

/// One commit-log entry.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CommitEntry {
    pub kind: Kind,
    pub table: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cursor: Option<Value>,
    pub fence: u64,
}

/// The file name of sequence number `seq`: zero-padded, so a sorted listing is log order.
pub fn file_name(seq: u64) -> String {
    format!("{seq:020}.json")
}

/// Parse a sequence number from its file name.
pub fn parse_seq(name: &str) -> Option<u64> {
    name.strip_suffix(".json")?.parse().ok()
}

/// Hold a commit under `fence` for `table` to the log as read: an entry for the table
/// carrying a higher fence refuses.
pub fn admit(log: &[CommitEntry], table: &str, fence: u64) -> Result<(), StoreError> {
    if let Some(higher) = log.iter().filter(|e| e.table == table).map(|e| e.fence).filter(|f| *f > fence).max() {
        return Err(StoreError::LeaseFenced(format!(
            "table `{table}`'s commit log carries fence {higher} past this commit's {fence}; the run stays unreadable"
        )));
    }
    Ok(())
}

/// Whether the log records `run_id`'s commit to `table` under `fence`: the one condition
/// under which a fenced run manifest is readable.
pub fn committed(log: &[CommitEntry], table: &str, run_id: &str, fence: u64) -> bool {
    log.iter().any(|e| e.kind == Kind::Commit && e.table == table && e.run_id.as_deref() == Some(run_id) && e.fence == fence)
}
