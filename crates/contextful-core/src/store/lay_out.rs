//! `store.lay-out`: names and shapes of the store tree — snapshot ids, part names, the
//! run and snapshot manifests, the table pointer and node identity.

use super::declare::ValidTime;
use super::StoreError;
use crate::time::Instant;
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Width of the zero-padded nanosecond field of a snapshot id (`store.lay-out.snapshot-id`).
pub const SNAPSHOT_ID_WIDTH: usize = 20;

/// Longest node id (`store.lay-out.node-id-shape`).
pub const NODE_ID_MAX_LEN: usize = 64;

/// The node id a machine with no writable state directory takes (`store.lay-out.node-id-local`).
pub const LOCAL_NODE_ID: &str = "local";

pub const MANIFEST_FILE: &str = "_manifest.json";
pub const POINTER_FILE: &str = "_pointer.json";
pub const SCHEMA_FILE: &str = "schema.json";
pub const STAGING_SUFFIX: &str = ".staging";

/// A project's store root, relative to the project directory (`store.lay-out.store-root`).
pub fn store_root(project: &str) -> String {
    format!(".contextful/context/{project}")
}

/// A data file name (`store.lay-out.part-name`).
pub fn part_name(ordinal: u32) -> String {
    format!("part-{ordinal:05}.parquet")
}

/// A snapshot id: `snapshot-` and a nanosecond value zero-padded to 20 chars.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct SnapshotId(u128);

impl SnapshotId {
    /// The id a commit at `commit` takes: the greater of the commit instant and the
    /// previous id plus one, so lexical order equals commit order under a clock that
    /// steps backwards.
    pub fn next(commit: Instant, previous: Option<&SnapshotId>) -> SnapshotId {
        let at = u128::try_from(commit.unix_nanos()).unwrap_or(0);
        SnapshotId(match previous {
            Some(p) => at.max(p.0 + 1),
            None => at,
        })
    }

    pub fn nanos(&self) -> u128 {
        self.0
    }
}

impl std::fmt::Display for SnapshotId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "snapshot-{:0width$}", self.0, width = SNAPSHOT_ID_WIDTH)
    }
}

impl TryFrom<String> for SnapshotId {
    type Error = String;
    fn try_from(s: String) -> Result<Self, String> {
        let digits = s.strip_prefix("snapshot-").ok_or_else(|| format!("`{s}` is not a snapshot id"))?;
        if digits.len() != SNAPSHOT_ID_WIDTH || !digits.bytes().all(|b| b.is_ascii_digit()) {
            return Err(format!("`{s}` is not a snapshot id: the value is {SNAPSHOT_ID_WIDTH} digits"));
        }
        digits.parse().map(SnapshotId).map_err(|e| format!("`{s}`: {e}"))
    }
}

impl From<SnapshotId> for String {
    fn from(id: SnapshotId) -> String {
        id.to_string()
    }
}

/// One data file a manifest names.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PartEntry {
    /// Relative to the manifest's directory.
    pub name: String,
    #[serde(default)]
    pub key_version: u32,
}

/// A run's commit marker (`store.lay-out.run-manifest`). Every field added after the
/// first carries a default (`store.lay-out.manifest-default`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RunManifest {
    pub run_id: String,
    pub table: String,
    pub node_id: String,
    #[serde(default)]
    pub parts: Vec<PartEntry>,
    pub committed_at: Instant,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pipeline_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cursor: Option<Value>,
    #[serde(default)]
    pub fence: Option<u64>,
}

impl RunManifest {
    /// The run's identity in a snapshot's `includes_runs`: `<run-id>/<node-id>`, its path
    /// under `data/runs/`, since two nodes commit one logical run id in disjoint directories.
    pub fn key(&self) -> String {
        format!("{}/{}", self.run_id, self.node_id)
    }
}

/// A snapshot's manifest (`store.lay-out.snapshot-manifest`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SnapshotManifest {
    pub snapshot_id: SnapshotId,
    #[serde(default)]
    pub parent: Option<SnapshotId>,
    pub table: String,
    pub created_at: Instant,
    #[serde(default)]
    pub includes_runs: Vec<String>,
    #[serde(default)]
    pub primary_key: Vec<String>,
    #[serde(default)]
    pub order_by: Option<String>,
    #[serde(default)]
    pub row_count: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub valid_time: Option<ValidTime>,
    #[serde(default)]
    pub parts: Vec<PartEntry>,
    #[serde(default)]
    pub indexes: Vec<Value>,
    #[serde(default)]
    pub fence: Option<u64>,
}

/// `tables/<t>/_pointer.json`: the current snapshot and the fence that published it
/// (`store.lay-out.table-pointer`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Pointer {
    pub snapshot_id: SnapshotId,
    #[serde(default)]
    pub fence: Option<u64>,
}

/// Whether `s` is one path segment the store interpolates into a path or key: non-empty,
/// of `[A-Za-z0-9._-]`, and neither `.` nor `..`. Run ids, node ids and each segment of a
/// table or project name hold to it.
pub fn is_path_segment(s: &str) -> bool {
    !s.is_empty() && s != "." && s != ".." && s.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
}

/// A validated node id.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct NodeId(String);

impl NodeId {
    /// Hold a resolved node id to its shape before any path, key or lease carries it
    /// (`store.lay-out.node-id-shape`).
    pub fn parse(s: &str) -> Result<NodeId, StoreError> {
        if s.len() > NODE_ID_MAX_LEN || !is_path_segment(s) {
            return Err(StoreError::StoreNodeIdInvalid(format!(
                "node id `{s}` is not 1 to {NODE_ID_MAX_LEN} chars of ^[A-Za-z0-9._-]+$"
            )));
        }
        Ok(NodeId(s.to_string()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for NodeId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// Where a process's node id came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NodeIdSource {
    Environment,
    Configuration,
    StateDirectory,
    Local,
}

/// Resolve the node id once, in order: `CONTEXTFUL_NODE_ID`, then `[node] id`, then the
/// id persisted in the state directory — generated there on first use — and `local`
/// where no state directory is writable (`store.lay-out.node-id-order`).
pub fn resolve_node_id(
    environment: Option<String>,
    configuration: Option<String>,
    state_directory: impl FnOnce() -> Option<String>,
) -> Result<(NodeId, NodeIdSource), StoreError> {
    if let Some(v) = environment {
        return Ok((NodeId::parse(&v)?, NodeIdSource::Environment));
    }
    if let Some(v) = configuration {
        return Ok((NodeId::parse(&v)?, NodeIdSource::Configuration));
    }
    match state_directory() {
        Some(v) => Ok((NodeId::parse(&v)?, NodeIdSource::StateDirectory)),
        None => Ok((NodeId(LOCAL_NODE_ID.to_string()), NodeIdSource::Local)),
    }
}
