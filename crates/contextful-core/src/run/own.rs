//! `run.own`: the execution owner a table holds, the build it pins while pending, and
//! when a closing run releases it.

use super::record::RunStatus;
use super::RunError;
use crate::time::Instant;
use serde::{Deserialize, Serialize};

/// The connector build a run is admitted against (`run.own.admission-pin`).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ConnectorPin {
    pub id: String,
    pub version: String,
    /// The component world, or `native`.
    pub world: String,
    /// The content hash of the connector artifact.
    pub hash: String,
}

impl std::fmt::Display for ConnectorPin {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}@{} ({}, {})", self.id, self.version, self.world, self.hash)
    }
}

/// What an owner pins while pending.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Pins {
    pub connector: ConnectorPin,
    /// The pipeline plan's `content_hash`.
    pub content_hash: String,
    /// The hash of the input the execution started from: its opening position.
    pub input_hash: String,
}

/// The durable execution owner of one live table (`run.own.execution-owner`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExecutionOwner {
    pub execution_id: String,
    pub pipeline_id: String,
    pub table: String,
    pub pins: Pins,
    /// Catalog run ids of the attempts made under this owner, oldest first.
    pub attempts: Vec<String>,
    pub opened_at: Instant,
}

impl ExecutionOwner {
    /// Hold a pending owner to the build now asked for. A moved connector identity,
    /// component world or plan hash refuses before any replay (`run.own.pinned-plan-changed`).
    pub fn check_pins(&self, now: &Pins) -> Result<(), RunError> {
        let p = &self.pins;
        if p.connector == now.connector && p.content_hash == now.content_hash {
            return Ok(());
        }
        Err(RunError::ExecutionPinMismatch(format!(
            "pipeline `{}` table `{}` has a pending execution `{}` pinned to connector {} and plan {}; this run asks for connector {} and plan {}. Restore the recorded build and resume, or rewind",
            self.pipeline_id, self.table, self.execution_id, p.connector, p.content_hash, now.connector, now.content_hash
        )))
    }

    /// Whether the newest commit marker for the table was produced by one of this
    /// owner's attempts and postdates the catalog's cached position, in which case the
    /// owner retires before any replay (`run.own.marker-reconciles`).
    pub fn produced(&self, marker_run_id: &str) -> bool {
        self.attempts.iter().any(|a| a == marker_run_id)
    }
}

/// Whether a run closing on `status`, having recorded `recorded_steps` journal entries,
/// releases its owner: `success` does, and so does a failure that wrote nothing; every
/// other status holds it (`run.own.pin-release`, `run.cancel.resumable-remains`).
pub fn releases(status: RunStatus, recorded_steps: usize) -> bool {
    match status {
        RunStatus::Success => true,
        RunStatus::Failed | RunStatus::Canceled => recorded_steps == 0,
        RunStatus::PartialFailure | RunStatus::Pending | RunStatus::Running | RunStatus::Waiting => false,
    }
}
