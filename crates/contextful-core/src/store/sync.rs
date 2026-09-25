//! `store.push`, `store.pull`, `store.probe` and `store.merge`: the sync configuration,
//! prefix confinement, the bucket manifest, key ownership and the scoped-union merge.

use super::StoreError;
use crate::time::Instant;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// Default re-commit rounds on a lost manifest compare-and-set: 5 attempts (`store.merge.retries`).
pub const PUSH_RETRIES: u32 = 5;
/// Manifest re-fetches a pull performs against keys moving beneath it: 3 attempts (`store.pull.convergence`).
pub const PULL_CONVERGENCE: u32 = 3;
/// Age at which a tombstone leaves the bucket manifest: 30 d (`store.merge.tombstone-ttl`).
pub const TOMBSTONE_TTL_SECS: u64 = 30 * 24 * 60 * 60;

/// The bucket manifest's key under the prefix.
pub const MANIFEST_KEY: &str = "manifest.json";
/// Where a probe writes its sentinel under the prefix.
pub const PROBE_PREFIX: &str = "_contextful/cas-probe/";

/// `[sync]` in a store's `config.toml`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SyncConfig {
    pub endpoint: String,
    pub bucket: String,
    #[serde(default)]
    pub prefix: Option<String>,
    /// `env:<NAME>`: the variable holding the prefix.
    #[serde(default)]
    pub prefix_from: Option<String>,
    /// `cas`, or `single-writer`.
    #[serde(default)]
    pub coordination: Option<String>,
    #[serde(default)]
    pub push_retries: Option<u32>,
    #[serde(default)]
    pub pull_before_run: Option<bool>,
}

impl SyncConfig {
    /// Resolve the prefix: `prefix` or the variable `prefix_from` names, never both and
    /// never the bucket root by default (`store.push.prefix-overspecified`, `store.push.prefix-unbound`).
    pub fn resolve_prefix(&self, env: impl Fn(&str) -> Option<String>) -> Result<String, StoreError> {
        let raw = match (&self.prefix, &self.prefix_from) {
            (Some(_), Some(_)) => {
                return Err(StoreError::SyncPrefixOverspecified("`[sync]` declares `prefix` and `prefix_from`; declare one".into()))
            }
            (Some(p), None) => p.clone(),
            (None, Some(from)) => {
                let var = from.strip_prefix("env:").unwrap_or(from);
                env(var).filter(|v| !v.is_empty()).ok_or_else(|| {
                    StoreError::SyncPrefixUnbound(format!("`prefix_from` names `{var}`, which is unset; the bucket root is no fallback"))
                })?
            }
            (None, None) => String::new(),
        };
        Ok(raw.trim_matches('/').to_string())
    }

    pub fn push_retries(&self) -> u32 {
        self.push_retries.unwrap_or(PUSH_RETRIES)
    }

    /// Whether the push is declared to coordinate by compare-and-set.
    pub fn declares_cas(&self) -> bool {
        self.coordination.as_deref().is_none_or(|c| c == "cas")
    }
}

/// `prefix` joined with `rel`, refusing a relative key that climbs or starts absolute
/// out of the prefix (`store.push.prefix-escape`).
pub fn confine(prefix: &str, rel: &str) -> Result<String, StoreError> {
    let escapes = rel.starts_with('/') || rel.split('/').any(|seg| seg == ".." || seg == ".") || rel.contains('\\') || rel.is_empty();
    if escapes {
        return Err(StoreError::SyncPrefixEscape(format!("key `{rel}` resolves outside the prefix `{prefix}`")));
    }
    Ok(if prefix.is_empty() { rel.to_string() } else { format!("{prefix}/{rel}") })
}

/// One object the bucket manifest lists.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Entry {
    pub sha256: String,
    pub size: u64,
    /// The node that owns the key, or empty for an unowned key.
    #[serde(default)]
    pub owner: String,
}

/// A deletion a writer records for an entry it owns.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Tombstone {
    pub owner: String,
    pub deleted_at: Instant,
}

/// The bucket manifest: key (relative to the prefix) to entry, and tombstones.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct BucketManifest {
    #[serde(default)]
    pub entries: BTreeMap<String, Entry>,
    #[serde(default)]
    pub tombstones: BTreeMap<String, Tombstone>,
}

/// The node owning a key, read off the key itself: a run directory's node segment, or a
/// request-ledger file's node. Every other key is unowned.
pub fn owner_of(key: &str) -> Option<String> {
    let segs: Vec<&str> = key.split('/').collect();
    if let Some(i) = segs.iter().position(|s| *s == "runs") {
        if segs.get(i - 1) == Some(&"data") && segs.len() > i + 3 {
            return Some(segs[i + 2].to_string());
        }
    }
    if let Some(i) = segs.iter().position(|s| *s == "requests") {
        let file = segs.get(i + 1)?;
        let stem = file.strip_suffix(".parquet")?;
        return stem.split_once('.').map(|(_, node)| node.to_string());
    }
    None
}

/// Whether `key` is a table pointer, which commits by a fenced compare-and-set and never
/// through the manifest.
pub fn is_pointer(key: &str) -> bool {
    key.ends_with("/_pointer.json")
}

/// Whether `key` is a commit-log entry, which resolves through its commit, never by recency.
pub fn is_commit_log(key: &str) -> bool {
    key.split('/').nth(1) == Some("cursors")
}

/// What a merge produced: the manifest to commit, and the refusals it answered by keeping an entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Merged {
    pub manifest: BucketManifest,
    pub refused: Vec<StoreError>,
}

/// Merge this writer's local entries into the remote manifest by a scoped union
/// (`store.merge`): the remote contributes an entry only where `me` has never written,
/// so an entry `me` owns and no longer holds leaves with a tombstone. A tombstone naming
/// another owner's entry refuses and keeps the entry; a tombstone past its TTL leaves. A
/// commit-log entry whose copies differ refuses outright: a cursor resolves through its
/// commit, never through whichever copy was written last.
pub fn merge(remote: &BucketManifest, local: &BTreeMap<String, Entry>, me: &str, now: Instant) -> Result<Merged, StoreError> {
    let mut out = BucketManifest::default();
    let mut refused = Vec::new();
    for (key, entry) in local {
        if let Some(theirs) = remote.entries.get(key) {
            if is_commit_log(key) && theirs.sha256 != entry.sha256 {
                return Err(StoreError::SyncCursorConflict(format!(
                    "commit-log entry `{key}` differs between this node and the bucket; a cursor resolves through its commit, not by recency"
                )));
            }
        }
        out.entries.insert(key.clone(), entry.clone());
    }
    for (key, entry) in &remote.entries {
        if out.entries.contains_key(key) {
            continue;
        }
        if entry.owner == me {
            // Owned here and no longer held here: its deletion propagates.
            out.tombstones.insert(key.clone(), Tombstone { owner: me.to_string(), deleted_at: now });
            continue;
        }
        out.entries.insert(key.clone(), entry.clone());
    }
    for (key, t) in &remote.tombstones {
        if out.tombstones.contains_key(key) || t.deleted_at.secs_until(now) >= TOMBSTONE_TTL_SECS {
            continue;
        }
        match out.entries.get(key) {
            Some(e) if e.owner != t.owner => {
                refused.push(StoreError::SyncTombstoneForeign(format!(
                    "a tombstone by `{}` names `{key}`, which `{}` owns; the entry stays",
                    t.owner, e.owner
                )));
                continue;
            }
            Some(e) if e.owner == me => continue,
            Some(_) => {
                out.entries.remove(key);
            }
            None => {}
        }
        out.tombstones.insert(key.clone(), t.clone());
    }
    Ok(Merged { manifest: out, refused })
}

/// The coordination a probe outcome resolves (`store.probe`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Coordination {
    /// Conditional writes were demonstrated.
    Cas,
    /// They were not; one writer at a time.
    SingleWriter,
}

impl Coordination {
    /// Hold a declared `cas` to what the probe demonstrated (`store.probe.unproven`).
    pub fn admit(self, config: &SyncConfig) -> Result<(), StoreError> {
        if config.declares_cas() && self != Coordination::Cas {
            return Err(StoreError::SyncCoordinationUnproven(format!(
                "`coordination = \"cas\"` and the probe did not demonstrate conditional writes against `{}`",
                config.endpoint
            )));
        }
        Ok(())
    }
}
