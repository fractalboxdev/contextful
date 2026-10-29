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

/// Most ancestors a snapshot manifest records (`store.lay-out.ancestors`).
pub const SNAPSHOT_ANCESTORS_MAX: usize = 256;

/// The node id a machine with no writable state directory takes (`store.lay-out.node-id-local`).
pub const LOCAL_NODE_ID: &str = "local";

pub const MANIFEST_FILE: &str = "_manifest.json";
/// The marker a collection writes beside a snapshot's `_manifest.json` once it removed the
/// snapshot's own parts and sidecars (`store.fold.manifest-kept`, `store.collect.superseded`).
pub const COLLECTED_FILE: &str = "_collected.json";
pub const POINTER_FILE: &str = "_pointer.json";
pub const SCHEMA_FILE: &str = "schema.json";
pub const STAGING_SUFFIX: &str = ".staging";

pub const CONFIG_FILE: &str = "config.toml";
pub const STORE_ID_FILE: &str = "store-id";
pub const TABLES_DIR: &str = "tables";
pub const CURSORS_DIR: &str = "cursors";
pub const RUNS_DIR: &str = "data/runs";
pub const SNAPSHOTS_DIR: &str = "data/snapshots";
pub const REQUESTS_DIR: &str = "requests";

/// The store-root file naming each folded run manifest a lazy pull left in the bucket
/// (`store.pull.lazy-folded-runs`). The leading `.` keeps it node-local: a push uploads no dot file.
pub const SKIPPED_RUNS_FILE: &str = ".skipped-runs.json";

/// The contents of [`SKIPPED_RUNS_FILE`]: each skipped `_manifest.json`, a run's or a
/// collected snapshot's, relative to the store root, sorted.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SkippedRuns {
    pub manifests: Vec<String>,
}

impl SkippedRuns {
    /// The skipped manifests of `table`: those under its `data/runs/` directory.
    pub fn of_table<'a>(&'a self, table: &str) -> impl Iterator<Item = &'a String> {
        let prefix = format!("{TABLES_DIR}/{table}/{RUNS_DIR}/");
        self.manifests.iter().filter(move |m| m.strip_prefix(&prefix).is_some_and(|rest| rest.split('/').count() == 3))
    }

    /// Whether `rel` names a collected snapshot's `_manifest.json` a pull skipped
    /// (`store.pull.husks-skipped`).
    pub fn skips_snapshot(&self, rel: &str) -> bool {
        rel.contains(&format!("/{SNAPSHOTS_DIR}/")) && self.manifests.iter().any(|m| m == rel)
    }
}

/// A project's store root, relative to the project directory (`store.lay-out.store-root`).
pub fn store_root(project: &str) -> String {
    format!(".contextful/context/{project}")
}

/// A leased pipeline's commit-log directory for one node, relative to the store root.
pub fn commit_log_dir(pipeline_id: &str, node_id: &str) -> String {
    format!("{CURSORS_DIR}/{pipeline_id}/{node_id}")
}

/// The paths of one table's directory, relative to the store root
/// (`store.lay-out.table-directory`). It renders names as given; a caller holds each to
/// [`is_path_segment`] before it reaches a filesystem.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TableLayout<'a> {
    table: &'a str,
}

impl<'a> TableLayout<'a> {
    pub fn new(table: &'a str) -> TableLayout<'a> {
        TableLayout { table }
    }

    /// `tables/<t>`.
    pub fn dir(&self) -> String {
        format!("{TABLES_DIR}/{}", self.table)
    }

    pub fn schema(&self) -> String {
        format!("{}/{SCHEMA_FILE}", self.dir())
    }

    pub fn pointer(&self) -> String {
        format!("{}/{POINTER_FILE}", self.dir())
    }

    /// `data/runs`, under which each run commits per node.
    pub fn runs(&self) -> String {
        format!("{}/{RUNS_DIR}", self.dir())
    }

    /// A run's node directory, `data/runs/<run-id>/<node-id>` (`store.lay-out.node-segment`).
    pub fn run(&self, run_id: &str, node_id: &str) -> String {
        format!("{}/{run_id}/{node_id}", self.runs())
    }

    pub fn run_manifest(&self, run_id: &str, node_id: &str) -> String {
        format!("{}/{MANIFEST_FILE}", self.run(run_id, node_id))
    }

    pub fn snapshots(&self) -> String {
        format!("{}/{SNAPSHOTS_DIR}", self.dir())
    }

    pub fn snapshot(&self, id: impl std::fmt::Display) -> String {
        format!("{}/{id}", self.snapshots())
    }

    pub fn snapshot_manifest(&self, id: impl std::fmt::Display) -> String {
        format!("{}/{MANIFEST_FILE}", self.snapshot(id))
    }

    /// Where a fold in flight writes, beside the id it publishes (`store.lay-out.staging`).
    pub fn staging(&self, id: impl std::fmt::Display) -> String {
        format!("{}/{id}{STAGING_SUFFIX}", self.snapshots())
    }

    /// The request-ledger directory (`store.reserve.ledger-path`).
    pub fn requests(&self) -> String {
        format!("{}/{REQUESTS_DIR}", self.dir())
    }
}

/// Percent-escape every byte outside `[A-Za-z0-9._-]`, rendering a value as one path segment.
pub fn escape(v: &str) -> String {
    let mut out = String::with_capacity(v.len());
    for b in v.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-') {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    if out == "." || out == ".." {
        out = out.replace('.', "%2E");
    }
    out
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

/// A run or snapshot manifest's `format_version`, `<major>.<minor>`
/// (`store.lay-out.format-version`). A minor adds defaulted fields an older build ignores;
/// a major changes meaning, and a build reading a newer major refuses the table
/// (`store.lay-out.format-newer`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct FormatVersion {
    pub major: u32,
    pub minor: u32,
}

impl FormatVersion {
    /// The newest layout this build writes: `2.1`, a snapshot manifest carrying a part by
    /// reference and a folded-runs index (`store.lay-out.format-version`).
    pub const CURRENT: FormatVersion = FormatVersion { major: 2, minor: 1 };

    /// The layout of a snapshot manifest carrying a part or a sidecar segment by reference.
    pub const BY_REFERENCE: FormatVersion = FormatVersion { major: 2, minor: 0 };

    /// The layout of a run manifest, and of a snapshot manifest holding every part in its
    /// own directory: `1.0`, which a build of either major reads.
    pub const SELF_CONTAINED: FormatVersion = FormatVersion { major: 1, minor: 0 };

    /// The version a snapshot manifest naming `parts` and `indexes` declares: `2.0` once a
    /// part or a sidecar segment sits in another snapshot's directory, `1.0` otherwise
    /// (`store.lay-out.format-version`).
    pub fn of_manifest(parts: &[PartEntry], indexes: &[super::index::IndexEntry]) -> FormatVersion {
        if parts.iter().any(|p| p.origin.is_some()) || indexes.iter().any(|i| !i.segments().is_empty() || i.extensions() > 0) {
            Self::BY_REFERENCE
        } else {
            Self::SELF_CONTAINED
        }
    }

    /// The version snapshot manifest `m` declares: the major [`FormatVersion::of_manifest`]
    /// gives its parts and sidecars, and minor `1` where it carries a folded-runs index, which
    /// a reader of an older minor ignores (`store.lay-out.format-version`).
    pub fn of_snapshot(m: &SnapshotManifest) -> FormatVersion {
        let base = Self::of_manifest(&m.parts, &m.indexes);
        match m.folded_runs {
            Some(_) => FormatVersion { major: base.major, minor: 1 },
            None => base,
        }
    }

    /// The version a manifest's bytes declare, read before the rest of the manifest so a
    /// newer major refuses whatever else it reshaped. A manifest without the field reads as
    /// `1.0`; `None` when the bytes hold no JSON object or the version is malformed, which
    /// the full parse then reports.
    pub fn declared(bytes: &[u8]) -> Option<FormatVersion> {
        #[derive(Deserialize)]
        struct Declared {
            #[serde(default)]
            format_version: FormatVersion,
        }
        serde_json::from_slice::<Declared>(bytes).ok().map(|d| d.format_version)
    }

    /// Refuse a version whose major this build does not read, naming `file` and both versions.
    pub fn admit(self, file: &str) -> Result<(), StoreError> {
        if self.major > Self::CURRENT.major {
            return Err(StoreError::StoreManifestFormatNewer(format!(
                "`{file}` declares format_version {self}, and this build reads {} at most within major {}",
                Self::CURRENT,
                Self::CURRENT.major
            )));
        }
        Ok(())
    }
}

/// A manifest without the field reads as `1.0`.
impl Default for FormatVersion {
    fn default() -> Self {
        Self::SELF_CONTAINED
    }
}

impl std::fmt::Display for FormatVersion {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}.{}", self.major, self.minor)
    }
}

impl TryFrom<String> for FormatVersion {
    type Error = String;
    fn try_from(s: String) -> Result<Self, String> {
        let (major, minor) = s.split_once('.').ok_or_else(|| format!("format_version `{s}` is not `<major>.<minor>`"))?;
        let parse = |v: &str| v.parse::<u32>().map_err(|e| format!("format_version `{s}`: {e}"));
        Ok(FormatVersion { major: parse(major)?, minor: parse(minor)? })
    }
}

impl From<FormatVersion> for String {
    fn from(v: FormatVersion) -> String {
        v.to_string()
    }
}

/// One data file a manifest names.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PartEntry {
    /// Relative to the directory holding the file: the manifest's own, or `origin`'s.
    pub name: String,
    #[serde(default)]
    pub key_version: u32,
    /// The earlier snapshot whose directory holds the file, on a part a snapshot carries by
    /// reference; absent when the file sits in the manifest's own directory
    /// (`store.fold.carry-by-reference`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub origin: Option<SnapshotId>,
    /// Rows the file holds, recorded by the pass that writes or carries it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rows: Option<u64>,
}

impl PartEntry {
    /// A part in its manifest's own directory.
    pub fn new(name: impl Into<String>, key_version: u32) -> PartEntry {
        PartEntry { name: name.into(), key_version, ..PartEntry::default() }
    }

    /// The snapshot whose directory holds the file: `origin`, else `own`, the snapshot
    /// whose manifest names it.
    pub fn holder<'a>(&'a self, own: &'a SnapshotId) -> &'a SnapshotId {
        self.origin.as_ref().unwrap_or(own)
    }

    /// The file's path relative to its table directory, named by the manifest of `own`:
    /// `data/snapshots/<holder>/<name>`.
    pub fn location(&self, own: &SnapshotId) -> String {
        format!("{SNAPSHOTS_DIR}/{}/{}", self.holder(own), self.name)
    }

    /// The file's path relative to the directory of the snapshot naming it: `name`, or
    /// `../<origin>/<name>` for a part carried by reference.
    pub fn locator(&self) -> String {
        match &self.origin {
            Some(origin) => format!("../{origin}/{}", self.name),
            None => self.name.clone(),
        }
    }

    /// The partition directory the part sits in, its `col=value` segments joined by `/`;
    /// empty on an unpartitioned table.
    pub fn partition(&self) -> &str {
        self.name.rsplit_once('/').map_or("", |(dir, _)| dir)
    }
}

/// A run's commit marker (`store.lay-out.run-manifest`). Every field added after the
/// first carries a default (`store.lay-out.manifest-default`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RunManifest {
    /// The layout this manifest follows (`store.lay-out.format-version`).
    #[serde(default)]
    pub format_version: FormatVersion,
    pub run_id: String,
    pub table: String,
    pub node_id: String,
    #[serde(default)]
    pub parts: Vec<PartEntry>,
    pub committed_at: Instant,
    /// A complete empty replacement frontier, preserved with a no-part manifest.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub replace_frontier: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pipeline_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cursor: Option<Value>,
    #[serde(default)]
    pub fence: Option<u64>,
    /// Written under the commit-log protocol: readable once the writing node's commit log
    /// records the run under `fence`. A manifest without it reads as committed.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub logged: bool,
    /// The `_commit_seq` value the run's rows carry (`store.reserve.commit-seq-seed`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub commit_seq: Option<i64>,
    /// A relational landing reads only once the root's group marker exists.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub group_root: Option<String>,
    /// Nested columns downgraded while this run staged its committed parts.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub schema_diffs: Vec<SchemaDiff>,
    /// The SHA-256 of the landing's producer rows, column types and position, which a
    /// re-landing of the run after its manifest is gone compares (`store.lay-out.run-digest`).
    /// A staged commit records none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub batch_digest: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SchemaDiff {
    pub table: String,
    pub column_path: String,
    pub source_type: String,
    pub landed_type: String,
    pub reason: String,
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
    /// The layout this manifest follows (`store.lay-out.format-version`).
    #[serde(default)]
    pub format_version: FormatVersion,
    pub snapshot_id: SnapshotId,
    #[serde(default)]
    pub parent: Option<SnapshotId>,
    /// The parent, then the parent's `ancestors`, at most [`SNAPSHOT_ANCESTORS_MAX`];
    /// absent when the parent records none (`store.lay-out.ancestors`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ancestors: Option<Vec<SnapshotId>>,
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
    pub indexes: Vec<super::index::IndexEntry>,
    #[serde(default)]
    pub fence: Option<u64>,
    /// The highest `_commit_seq` its included runs and its parent record
    /// (`store.reserve.commit-seq-seed`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub commit_seq: Option<i64>,

    /// A published build's contract identity, freshness and build, committed with the
    /// data it publishes (`run.publish.manifest-commit`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub publish: Option<crate::pipeline::model::PublishSection>,

    /// Every run the snapshot's chain folds, in segments (`store.lay-out.folded-index`). A
    /// reader trusts it only where `through` names this snapshot; a field it cannot decode
    /// reads as absent, and the reader walks.
    #[serde(default, deserialize_with = "lenient_folded", skip_serializing_if = "Option::is_none")]
    pub folded_runs: Option<FoldedIndex>,
}

/// `folded_runs` as decoded, `None` where its value does not decode (`store.lay-out.folded-index-valid`).
fn lenient_folded<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Option<FoldedIndex>, D::Error> {
    let value = serde_json::Value::deserialize(d)?;
    Ok(serde_json::from_value(value).ok())
}

/// A snapshot's folded-runs index: the segments listing every run its chain folds, and the
/// snapshot they describe (`store.lay-out.folded-index`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FoldedIndex {
    /// The snapshot the segments describe; any other snapshot carrying the field ignores it.
    pub through: SnapshotId,
    pub segments: Vec<FoldedSegment>,
    /// Rows no segment holds yet, fewer than [`FoldedIndex::INLINE_MAX`]: a pass folding a
    /// few runs writes no file for them.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub inline: Vec<FoldedRow>,
}

impl FoldedIndex {
    /// The directory, under a snapshot directory, holding the segments it writes.
    pub const DIR: &'static str = "folded";
    /// Inline rows a pass flushes into a segment once they reach this many.
    pub const INLINE_MAX: usize = 64;
}

/// One Parquet file of folded-run rows, sorted by run key.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FoldedSegment {
    /// Relative to the directory holding the file: the manifest's own, or `origin`'s.
    pub name: String,
    /// The earlier snapshot whose directory holds the file, on a segment carried by reference.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub origin: Option<SnapshotId>,
    /// The SHA-256 of the file's bytes as written; a segment reading otherwise is unread.
    pub sha256: String,
    pub rows: u64,
}

impl FoldedSegment {
    /// The file's path relative to its table directory, named by the manifest of `own`.
    pub fn location(&self, own: &SnapshotId) -> String {
        format!("{SNAPSHOTS_DIR}/{}/{}", self.origin.as_ref().unwrap_or(own), self.name)
    }
}

/// One run a chain folds, as a folded-runs index records it (`store.lay-out.folded-index`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FoldedRow {
    /// `<run-id>/<node-id>`, the key `includes_runs` names.
    #[serde(rename = "run")]
    pub run_key: String,
    /// The snapshot whose `includes_runs` names the run.
    #[serde(rename = "snapshot")]
    pub snapshot_id: SnapshotId,
    /// The run's commit instant, where its manifest was on disk when the row was written.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub committed_at: Option<Instant>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub commit_seq: Option<i64>,
    /// The run manifest's `batch_digest`; absent on a row for a run that recorded none.
    #[serde(default, rename = "digest", skip_serializing_if = "Option::is_none")]
    pub batch_digest: Option<String>,
    /// The rows the run's parts held, where its manifest was on disk when the row was written.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rows: Option<u64>,
}

impl SnapshotManifest {
    /// Every data file the snapshot names, relative to its table directory; a part carried
    /// by reference sits under its origin's directory (`store.fold.carry-by-reference`).
    pub fn part_files(&self) -> Vec<String> {
        self.parts.iter().map(|p| p.location(&self.snapshot_id)).collect()
    }

    /// The `ancestors` a snapshot whose parent is `parent` records (`store.lay-out.ancestors`):
    /// none for a root, absent under a parent recording none, else the parent first.
    pub fn ancestors_after(parent: Option<&SnapshotManifest>) -> Option<Vec<SnapshotId>> {
        let Some(parent) = parent else { return Some(Vec::new()) };
        let inherited = parent.ancestors.as_ref()?;
        Some(std::iter::once(parent.snapshot_id.clone()).chain(inherited.iter().cloned()).take(SNAPSHOT_ANCESTORS_MAX).collect())
    }
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
