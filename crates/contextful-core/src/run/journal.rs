//! `run.journal`: entry keys, the idempotency key derived from them, where a recorded
//! value lives, and the rules the journal's storage adapter applies.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// Largest journaled value held inline in its row: 1 MiB (`run.journal.inline-cutoff`).
pub const INLINE_CUTOFF_BYTES: usize = 1024 * 1024;
/// Cadence of the blob mark-and-sweep pass: 24 h (`run.journal.blob-sweep`).
pub const BLOB_SWEEP_INTERVAL_SECS: u64 = 24 * 60 * 60;
/// Age below which an unreferenced blob survives a sweep: 1 h (`run.journal.blob-sweep`).
pub const BLOB_SWEEP_GRACE_SECS: u64 = 60 * 60;

/// Lowercase hex of a SHA-256 digest.
pub fn sha256_hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes).iter().map(|b| format!("{b:02x}")).collect()
}

/// A journal entry's key (`run.journal.entry-key`).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct EntryKey {
    pub execution_id: String,
    pub step_label: String,
    pub input_hash: String,
}

impl EntryKey {
    /// The key of step `step_label` under `execution_id`, hashing the step's input.
    pub fn new(execution_id: &str, step_label: &str, input: &[u8]) -> EntryKey {
        EntryKey { execution_id: execution_id.into(), step_label: step_label.into(), input_hash: sha256_hex(input) }
    }

    /// A stable, path-safe digest of the whole key, naming the entry's row.
    pub fn digest(&self) -> String {
        sha256_hex(format!("{}\0{}\0{}", self.execution_id, self.step_label, self.input_hash).as_bytes())
    }

    /// The idempotency key every outbound request of this step carries: derived from the
    /// entry key alone, so every re-entry of the effect sends the identical value
    /// (`run.journal.idempotency-key`).
    pub fn idempotency_key(&self) -> String {
        sha256_hex(format!("idempotency\0{}", self.digest()).as_bytes())[..32].to_string()
    }
}

/// The key a resume payload records under: `sha256("awakeable:" + token)`
/// (`run.suspend.resume-is-a-step-output`).
pub fn awakeable_key(token: &str) -> String {
    sha256_hex(format!("awakeable:{token}").as_bytes())
}

/// Where a recorded value lives (`run.journal.inline-cutoff`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "at", rename_all = "snake_case")]
pub enum Stored {
    /// The bytes in the row, as a UTF-8 string when they are one, else hex.
    Inline {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        text: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        hex: Option<String>,
    },
    /// A content-addressed blob named by the value's sha256.
    Blob { sha256: String, bytes: u64 },
}

impl Stored {
    /// Place `value`: inline at or below the cutoff, else a blob the caller writes first.
    pub fn place(value: &[u8]) -> Stored {
        if value.len() <= INLINE_CUTOFF_BYTES {
            match std::str::from_utf8(value) {
                Ok(s) => Stored::Inline { text: Some(s.to_string()), hex: None },
                Err(_) => Stored::Inline { text: None, hex: Some(value.iter().map(|b| format!("{b:02x}")).collect()) },
            }
        } else {
            Stored::Blob { sha256: sha256_hex(value), bytes: value.len() as u64 }
        }
    }

    /// The inline bytes; `None` for a blob reference.
    pub fn inline_bytes(&self) -> Option<Vec<u8>> {
        match self {
            Stored::Inline { text: Some(t), .. } => Some(t.as_bytes().to_vec()),
            Stored::Inline { hex: Some(h), .. } => {
                (0..h.len()).step_by(2).map(|i| u8::from_str_radix(h.get(i..i + 2)?, 16).ok()).collect()
            }
            Stored::Inline { .. } => Some(Vec::new()),
            Stored::Blob { .. } => None,
        }
    }

    /// The blob this value references.
    pub fn blob(&self) -> Option<&str> {
        match self {
            Stored::Blob { sha256, .. } => Some(sha256),
            Stored::Inline { .. } => None,
        }
    }
}

/// A journal row: a claim held while the effect runs, or the recorded value.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum Row {
    /// The run holding the claim; its owner lease decides a takeover.
    Pending { key: EntryKey, run_id: String },
    Recorded { key: EntryKey, value: Stored },
}

/// Whether a sweep deletes a blob: unreferenced by every journal row and pending
/// awakeable, and at least the grace window old.
pub fn sweepable(referenced: bool, age_secs: u64) -> bool {
    !referenced && age_secs >= BLOB_SWEEP_GRACE_SECS
}

/// Whether a sweep pass is due, the previous one having run `since_last_secs` ago.
pub fn sweep_due(since_last_secs: Option<u64>) -> bool {
    since_last_secs.is_none_or(|s| s >= BLOB_SWEEP_INTERVAL_SECS)
}
