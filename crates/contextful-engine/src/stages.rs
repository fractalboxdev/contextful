//! The stages a run reports while it runs: each pull, landing, commit and journaled step,
//! which the binary turns into the run's child spans (`disclosure.record.stage-span`).
//! With no sink wired the runner times no stage (`disclosure.record.stage-unconfigured`).

use contextful_core::run::{Failure, RunError};
use std::time::Duration;

/// Which stage of a run a report names.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StageKind {
    /// One call to the source.
    Pull,
    /// One shaped batch staged as a part.
    Land,
    /// One commit of the staged parts, a checkpoint's included.
    Commit,
    /// One step resolved through the journal.
    Step,
}

impl StageKind {
    /// The stage's name, as its span's `contextful.stage` carries it.
    pub fn name(self) -> &'static str {
        match self {
            StageKind::Pull => "pull",
            StageKind::Land => "land",
            StageKind::Commit => "commit",
            StageKind::Step => "step",
        }
    }
}

/// A pull, landing or commit that succeeded (`disclosure.record.stage-outcome`).
pub const OK: &str = "ok";
/// A stage that failed; its report carries the error identifier.
pub const FAILED: &str = "failed";
/// A journaled step whose value the journal held, so the effect did not run.
pub const REPLAYED: &str = "replayed";
/// A journaled step whose effect ran and whose value the journal now holds.
pub const RECORDED: &str = "recorded";
/// A journaled step whose effect ran and whose value the journal did not keep.
pub const UNRECORDED: &str = "unrecorded";
/// A journaled step parked until its next-attempt instant.
pub const WAITING: &str = "waiting";

/// One stage a run passed through: names, counts and identifiers only, never a row value
/// or a statement (`disclosure.record.stage-content`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StageReport {
    pub kind: StageKind,
    /// The run's table; `None` for a step under a scope naming none.
    pub table: Option<String>,
    /// The step label a pull or a journaled step resolves under.
    pub step: Option<String>,
    pub outcome: &'static str,
    /// A failed stage's error identifier.
    pub error: Option<String>,
    pub rows: Option<u64>,
    pub bytes: Option<u64>,
    /// How long the stage took, on the monotonic clock.
    pub elapsed: Duration,
}

/// Where a run's stage reports go.
pub trait StageSink: Send + Sync {
    fn stage(&self, report: StageReport);
}

/// The error identifier a failure carries: its tag's wire spelling.
pub fn failure_identifier(failure: &Failure) -> String {
    failure.tag.name().to_string()
}

/// The error identifier a refusal carries: the identifier its text begins with.
pub fn refusal_identifier(error: &RunError) -> String {
    let text = error.to_string();
    text.split_once(':').map_or(text.as_str(), |(id, _)| id).to_string()
}
