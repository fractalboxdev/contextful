//! A node's run state: the run history and cursor markers that live outside the store root,
//! summarized into `nodes/<node-id>/run-state.json` so they travel with the store
//! (`store.push.run-state`, `store.pull.run-state-cursor`).

use crate::sync::{Result, SyncError};
use contextful_context::{ContextError, Store};
use contextful_context::encrypt::MetadataFiles;
use contextful_core::coordinate::Catalog;
use contextful_core::run::record::{RunRow, RunStatus};
use contextful_core::surface::control::parse_pointer;
use contextful_core::time::Instant;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

/// The run-state layout this build writes and reads (`store.push.run-state-format`).
pub const RUN_STATE_FORMAT: u32 = 1;
/// The directory under the store root holding one run state per node.
pub const NODES_DIR: &str = "nodes";
/// A run state's file name inside its node's directory.
pub const RUN_STATE_FILE: &str = "run-state.json";

/// One pipeline's newest run on the recording node.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunMark {
    pub run_id: String,
    pub table: String,
    pub status: RunStatus,
    pub started_at: Instant,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ended_at: Option<Instant>,
}

/// One cursor row and the commit marker its position mirrors.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CursorMark {
    pub pipeline_id: String,
    pub table: String,
    #[serde(default)]
    pub position: Option<Value>,
    pub run_id: String,
    pub committed_at: Instant,
}

/// What one node records of its run history and cursors.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunState {
    #[serde(default = "first_format")]
    pub format: u32,
    pub node_id: String,
    /// The newest-started run per pipeline id.
    #[serde(default)]
    pub runs: BTreeMap<String, RunMark>,
    #[serde(default)]
    pub cursors: Vec<CursorMark>,
    /// The version named by the local default control pointer, when one is applied.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub control_version: Option<u64>,
}

fn first_format() -> u32 {
    1
}

fn invalid(what: impl std::fmt::Display) -> SyncError {
    SyncError::Context(ContextError::Invalid(what.to_string()))
}

fn io(path: &Path, e: std::io::Error) -> SyncError {
    SyncError::Context(ContextError::Io { path: path.to_path_buf(), source: e })
}

impl RunState {
    /// The run state of `node`: its catalog's newest run per pipeline, and the cursor row of
    /// every pipeline table its runs name that mirrors a commit marker.
    pub fn read(catalog: &dyn Catalog, node: &str) -> Result<RunState> {
        let rows: Vec<RunRow> = catalog.runs(None).map_err(invalid)?;
        let mut runs: BTreeMap<String, RunMark> = BTreeMap::new();
        let mut scopes: BTreeSet<(String, String)> = BTreeSet::new();
        for row in rows {
            if row.host_scope.is_none() && !row.table.is_empty() {
                scopes.insert((row.pipeline_id.clone(), row.table.clone()));
            }
            let newer = runs.get(&row.pipeline_id).is_none_or(|m| (row.started_at, &row.run_id) > (m.started_at, &m.run_id));
            if newer {
                let mark = RunMark { run_id: row.run_id, table: row.table, status: row.status, started_at: row.started_at, ended_at: row.ended_at };
                runs.insert(row.pipeline_id, mark);
            }
        }
        let mut cursors = Vec::new();
        for (pipeline_id, table) in scopes {
            let row = catalog.cursor(&pipeline_id, &table).map_err(invalid)?;
            if let (Some(run_id), Some(committed_at)) = (row.marker_run_id, row.marker_committed_at) {
                cursors.push(CursorMark { pipeline_id, table, position: row.position, run_id, committed_at });
            }
        }
        Ok(RunState { format: RUN_STATE_FORMAT, node_id: node.to_string(), runs, cursors, control_version: None })
    }
}

/// The default local control directory beside a store root: `.contextful/control/<project>/`.
pub fn control_dir(store: &Store, project: &str) -> PathBuf {
    let depth = project.split('/').count() + 1;
    store.root().ancestors().nth(depth).map_or_else(|| store.root().join("control"), |dotdir| dotdir.join("control")).join(project)
}

/// The version named by the default local control pointer, when the pointer exists.
pub fn local_control_version(store: &Store, project: &str) -> Result<Option<u64>> {
    let path = control_dir(store, project).join("manifest@current");
    match std::fs::read_to_string(&path) {
        Ok(body) => parse_pointer(&body).map(Some).map_err(|e| invalid(e.to_string())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(io(&path, e)),
    }
}

/// `nodes/<node-id>/run-state.json` under `root`.
pub fn state_path(root: &Path, node: &str) -> PathBuf {
    root.join(NODES_DIR).join(node).join(RUN_STATE_FILE)
}

/// Run-state files under a store root, using that store's metadata envelope.
pub struct RunStateFiles<'a> {
    root: &'a Path,
    metadata: MetadataFiles<'a>,
}

impl<'a> RunStateFiles<'a> {
    pub fn new(root: &'a Path, metadata: MetadataFiles<'a>) -> Self {
        Self { root, metadata }
    }

    /// Write `state` as its node's run state under the store root (`store.push.run-state`).
    pub fn record(&self, state: &RunState) -> Result<PathBuf> {
        let path = state_path(self.root, &state.node_id);
        let bytes = serde_json::to_vec_pretty(state).map_err(invalid)?;
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).map_err(|e| io(dir, e))?;
        }
        self.metadata.replace(&path, &bytes)?;
        Ok(path)
    }

    /// Every node's state, keyed by node id. Unsupported or malformed canonical
    /// JSON contributes nothing; an unreadable encrypted envelope refuses.
    pub fn run_states(&self) -> Result<BTreeMap<String, RunState>> {
        let dir = self.root.join(NODES_DIR);
        let entries = match std::fs::read_dir(&dir) {
            Ok(e) => e,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(BTreeMap::new()),
            Err(e) => return Err(io(&dir, e)),
        };
        let mut out = BTreeMap::new();
        for entry in entries {
            let node = entry.map_err(|e| io(&dir, e))?.file_name().to_string_lossy().into_owned();
            let path = state_path(self.root, &node);
            let Some(bytes) = self.metadata.read_optional(&path)? else { continue };
            let Ok(value) = serde_json::from_slice::<Value>(&bytes) else { continue };
            if value.get("format").and_then(Value::as_u64).unwrap_or(1) > u64::from(RUN_STATE_FORMAT) {
                continue;
            }
            if let Ok(state) = serde_json::from_value::<RunState>(value) {
                out.insert(node, state);
            }
        }
        Ok(out)
    }
}

/// Write `state` under the store root using its metadata envelope.
pub fn record(store: &Store, state: &RunState) -> Result<PathBuf> {
    RunStateFiles::new(store.root(), store.metadata_files()).record(state)
}

/// Every node's run state the store holds, keyed by node id (`store.push.run-state-format`).
pub fn run_states(store: &Store) -> Result<BTreeMap<String, RunState>> {
    RunStateFiles::new(store.root(), store.metadata_files()).run_states()
}

/// Each pipeline's latest run start across every node's run state, the history a schedule
/// counts its next fire from (`surface.arm.pulled-history`).
pub fn newest_starts(store: &Store) -> Result<BTreeMap<String, Instant>> {
    let mut out: BTreeMap<String, Instant> = BTreeMap::new();
    for (pipeline, mark) in run_states(store)?.into_values().flat_map(|s| s.runs) {
        let start = out.entry(pipeline).or_insert(mark.started_at);
        *start = (*start).max(mark.started_at);
    }
    Ok(out)
}

/// The newest commit marker any node's run state records for `pipeline_id`'s `table`
/// (`store.pull.run-state-cursor`).
pub fn newest_cursor(store: &Store, pipeline_id: &str, table: &str) -> Result<Option<CursorMark>> {
    Ok(run_states(store)?
        .into_values()
        .flat_map(|s| s.cursors)
        .filter(|c| c.pipeline_id == pipeline_id && c.table == table)
        .max_by(|a, b| a.committed_at.cmp(&b.committed_at).then_with(|| a.run_id.cmp(&b.run_id))))
}
