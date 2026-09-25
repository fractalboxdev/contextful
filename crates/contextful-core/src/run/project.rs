//! `run.project`: the wire snapshot of a live run, the deltas that move it, the total
//! reducer folding them, and the coalescing decision over an injected clock.
//!
//! The projection is best-effort and execution never reads it; the in-process hub that
//! carries it off the machine is `contextful-engine::project`.

use super::failure::Failure;
use super::record::RunStatus;
use super::RunError;
use crate::time::Instant;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;

/// Serialized bound on a snapshot's application metadata: 16384 B (`run.project.metadata-too-large`).
pub const METADATA_MAX_BYTES: usize = 16384;
/// Window snapshot broadcasts collapse into: 100 ms (`run.project.coalescing`).
pub const COALESCE_WINDOW_MS: u64 = 100;

/// A snapshot version: the hub's start instant, then a counter within that hub. The
/// derived order compares the epoch first, so a restarted hub's versions sort above
/// every version its predecessor issued (`run.project.version`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct Version {
    pub epoch: Instant,
    pub counter: u64,
}

/// A step's status in the projection.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StepStatus {
    Pending,
    Running,
    Completed,
    Failed,
    Retrying,
}

impl StepStatus {
    /// `completed` and `failed` end a step.
    pub fn is_terminal(self) -> bool {
        matches!(self, StepStatus::Completed | StepStatus::Failed)
    }

    /// `running` and `retrying` mark the step a stop interrupts.
    pub fn is_executing(self) -> bool {
        matches!(self, StepStatus::Running | StepStatus::Retrying)
    }
}

/// A step output as the projection carries it: a post-redaction reference and a byte
/// count. The type holds no payload field, so no snapshot can carry one inline
/// (`run.project.outputs-by-reference`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OutputRef {
    pub reference: String,
    pub bytes: u64,
}

/// One step of a run (`run.project.step-view`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StepView {
    pub label: String,
    pub status: StepStatus,
    pub attempts: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub started_at: Option<Instant>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ended_at: Option<Instant>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output: Option<OutputRef>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failure: Option<Failure>,
}

/// The wire snapshot of one run (`run.project.wire-snapshot`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Snapshot {
    pub run_id: String,
    pub workflow_id: String,
    pub status: RunStatus,
    pub steps: Vec<StepView>,
    pub metadata: BTreeMap<String, Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub started_at: Option<Instant>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ended_at: Option<Instant>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<Failure>,
    pub version: Version,
}

impl Snapshot {
    /// An empty `pending` snapshot at `version`.
    pub fn new(run_id: &str, workflow_id: &str, version: Version) -> Snapshot {
        Snapshot {
            run_id: run_id.to_string(),
            workflow_id: workflow_id.to_string(),
            status: RunStatus::Pending,
            steps: Vec::new(),
            metadata: BTreeMap::new(),
            started_at: None,
            ended_at: None,
            error: None,
            version,
        }
    }

    pub fn is_terminal(&self) -> bool {
        self.status.is_terminal()
    }

    pub fn step(&self, label: &str) -> Option<&StepView> {
        self.steps.iter().find(|s| s.label == label)
    }

    /// The snapshot's wire form. Every deploy target serializes through this one call.
    pub fn to_wire(&self) -> String {
        serde_json::to_string(self).unwrap_or_default()
    }
}

/// A field-wise patch to one step; absent fields keep their value.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct StepPatch {
    pub label: String,
    #[serde(default)]
    pub status: Option<StepStatus>,
    #[serde(default)]
    pub attempts: Option<u32>,
    #[serde(default)]
    pub started_at: Option<Instant>,
    #[serde(default)]
    pub ended_at: Option<Instant>,
    #[serde(default)]
    pub output: Option<OutputRef>,
    #[serde(default)]
    pub failure: Option<Failure>,
}

impl StepPatch {
    pub fn new(label: &str) -> StepPatch {
        StepPatch { label: label.to_string(), ..StepPatch::default() }
    }

    pub fn status(mut self, status: StepStatus) -> StepPatch {
        self.status = Some(status);
        self
    }
}

/// What one delta changes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Change {
    /// The run's status, the instant it took effect, and the run-level error.
    Status {
        status: RunStatus,
        #[serde(default)]
        at: Option<Instant>,
        #[serde(default)]
        error: Option<Failure>,
    },
    Step(StepPatch),
    /// Keys set in the application metadata map.
    Metadata { entries: BTreeMap<String, Value> },
}

/// One versioned change to a run's snapshot.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Delta {
    pub run_id: String,
    pub workflow_id: String,
    pub version: Version,
    pub change: Change,
}

impl Delta {
    /// Whether this delta moves its run to a terminal status.
    pub fn is_terminal(&self) -> bool {
        matches!(&self.change, Change::Status { status, .. } if status.is_terminal())
    }
}

/// What folding a delta did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Applied {
    /// The snapshot took the delta and its version.
    Folded,
    /// The delta's version is at or below the snapshot's; nothing moved.
    Stale,
}

/// Hold a metadata write to its bound: the merged map's serialized size above
/// 16384 B refuses, naming size and bound, and leaves the snapshot unchanged.
pub fn write_metadata(snapshot: &mut Snapshot, entries: &BTreeMap<String, Value>) -> Result<(), RunError> {
    let mut merged = snapshot.metadata.clone();
    merged.extend(entries.iter().map(|(k, v)| (k.clone(), v.clone())));
    let size = serde_json::to_vec(&merged).map(|b| b.len()).unwrap_or(usize::MAX);
    if size > METADATA_MAX_BYTES {
        return Err(RunError::RunMetadataTooLarge(format!(
            "run `{}` metadata serializes to {size} B, above the {METADATA_MAX_BYTES} B bound",
            snapshot.run_id
        )));
    }
    snapshot.metadata = merged;
    Ok(())
}

fn merge_step(step: &mut StepView, patch: &StepPatch) {
    if let Some(s) = patch.status {
        step.status = s;
    }
    if let Some(a) = patch.attempts {
        step.attempts = a;
    }
    if patch.started_at.is_some() {
        step.started_at = patch.started_at;
    }
    if patch.ended_at.is_some() {
        step.ended_at = patch.ended_at;
    }
    if patch.output.is_some() {
        step.output = patch.output.clone();
    }
    if patch.failure.is_some() {
        step.failure = patch.failure.clone();
    }
}

/// Fold `delta` into `snapshot`. The reducer is total: a delta at or below the held
/// version is a no-op, nothing overwrites a terminal run or step status, a new step
/// label appends, and an existing one merges field-wise (`run.project.reducer-is-total`).
///
/// A stop closes the run `canceled`: its executing step reads `failed` under the
/// `Canceled` tag and the run-level error stays unset (`run.project.stopped-step`).
/// The one refusal is a metadata write past its bound, which leaves the snapshot as it was.
pub fn reduce(snapshot: &mut Snapshot, delta: &Delta) -> Result<Applied, RunError> {
    if delta.version <= snapshot.version {
        return Ok(Applied::Stale);
    }
    match &delta.change {
        Change::Status { status, at, error } => {
            if !snapshot.is_terminal() {
                snapshot.status = *status;
                match status {
                    RunStatus::Running if snapshot.started_at.is_none() => snapshot.started_at = *at,
                    s if s.is_terminal() => snapshot.ended_at = *at,
                    _ => {}
                }
                if *status == RunStatus::Canceled {
                    snapshot.error = None;
                    for step in snapshot.steps.iter_mut().filter(|s| s.status.is_executing()) {
                        step.status = StepStatus::Failed;
                        step.failure = Some(Failure::canceled(format!("run `{}` was stopped", snapshot.run_id)));
                        step.ended_at = *at;
                    }
                } else if status.is_terminal() {
                    snapshot.error = error.clone();
                }
            }
        }
        Change::Step(patch) => match snapshot.steps.iter_mut().find(|s| s.label == patch.label) {
            Some(step) if step.status.is_terminal() => {}
            Some(step) => merge_step(step, patch),
            None => {
                let mut step = StepView {
                    label: patch.label.clone(),
                    status: StepStatus::Pending,
                    attempts: 0,
                    started_at: None,
                    ended_at: None,
                    output: None,
                    failure: None,
                };
                merge_step(&mut step, patch);
                snapshot.steps.push(step);
            }
        },
        Change::Metadata { entries } => write_metadata(snapshot, entries)?,
    }
    snapshot.version = delta.version;
    Ok(Applied::Folded)
}

/// The broadcast decision for one run: at most one broadcast per 100 ms window, keeping
/// only the latest snapshot offered inside it; a terminal snapshot flushes at once. The
/// caller supplies every instant (`run.project.coalescing`).
#[derive(Debug, Clone, Default)]
pub struct Coalescer {
    last_flush: Option<Instant>,
    pending: Option<Snapshot>,
}

fn elapsed_ms(from: Instant, to: Instant) -> u128 {
    u128::try_from(to.unix_nanos() - from.unix_nanos()).unwrap_or(0) / 1_000_000
}

impl Coalescer {
    /// Offer a snapshot at `now`; the snapshot to broadcast now, if any.
    pub fn offer(&mut self, snapshot: Snapshot, now: Instant) -> Option<Snapshot> {
        let open = self.last_flush.is_none_or(|t| elapsed_ms(t, now) >= u128::from(COALESCE_WINDOW_MS));
        if snapshot.is_terminal() || open {
            self.pending = None;
            self.last_flush = Some(now);
            Some(snapshot)
        } else {
            self.pending = Some(snapshot);
            None
        }
    }

    /// The held snapshot, once its window has closed at `now`.
    pub fn poll(&mut self, now: Instant) -> Option<Snapshot> {
        let closed = self.last_flush.is_none_or(|t| elapsed_ms(t, now) >= u128::from(COALESCE_WINDOW_MS));
        if closed {
            let out = self.pending.take();
            if out.is_some() {
                self.last_flush = Some(now);
            }
            out
        } else {
            None
        }
    }
}
