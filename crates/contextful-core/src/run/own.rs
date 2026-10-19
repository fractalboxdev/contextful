//! `run.own`: the scope an execution owner is keyed on, the build or plan reference it
//! pins while pending, and when a closing run releases it.

use super::failure::{Failure, FailureTag};
use super::record::RunStatus;
use super::RunError;
use crate::time::Instant;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// The suffix naming a table's seeding scope.
pub const SEED_SUFFIX: &str = "#seed";
/// The infix naming a table's receive scope for one batch key.
pub const RECEIVE_INFIX: &str = "#receive-";

/// The longest run id the store lands under, in chars.
// mirrors: store.lay-out.run-id-shape
pub const RUN_ID_MAX_CHARS: usize = 128;

/// The run id a run's `n`th commit lands under: the run's own id for its first commit, then
/// `<run id>.<n>` with `n` zero-padded to six digits, so each commit names its own parts
/// (`run.own.checkpoint-run-id`). A later commit's id past [`RUN_ID_MAX_CHARS`] raises
/// `RunCheckpointIdTooLong` (`run.own.checkpoint-id-length`).
pub fn segment_run_id(run_id: &str, n: u32) -> Result<String, RunError> {
    if n == 0 {
        return Ok(run_id.to_string());
    }
    let segment = format!("{run_id}.{n:06}");
    if segment.len() > RUN_ID_MAX_CHARS {
        return Err(RunError::RunCheckpointIdTooLong(format!(
            "run `{run_id}` would land its checkpoint under a {}-char run id, past the store's {RUN_ID_MAX_CHARS}-char bound; it commits nothing and retires its owner",
            segment.len()
        )));
    }
    Ok(segment)
}

/// The 16 hex characters a batch key of `pipeline_id` names its runs and scopes by.
pub fn receive_digest(pipeline_id: &str, key: &str) -> String {
    let mut bytes = pipeline_id.as_bytes().to_vec();
    bytes.push(0);
    bytes.extend_from_slice(key.as_bytes());
    crate::run::journal::sha256_hex(&bytes)[..16].to_string()
}

/// What an execution owner is keyed on (`run.own.execution-owner`, `run.own.host-scope`).
///
/// A table scope serializes as the `pipeline_id` and `table` fields an owner row carries,
/// so a table owner's stored row and key stay byte for byte what a table-keyed catalog wrote.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(untagged)]
pub enum OwnerScope {
    /// One chunk of a table's backfill.
    Chunk { pipeline_id: String, table: String, chunk: String },
    /// A live table.
    Table { pipeline_id: String, table: String },
    /// A scope id the embedding host declares.
    Host { host_scope: String },
}

impl OwnerScope {
    pub fn table(pipeline_id: &str, table: &str) -> OwnerScope {
        OwnerScope::Table { pipeline_id: pipeline_id.to_string(), table: table.to_string() }
    }

    pub fn chunk(pipeline_id: &str, table: &str, chunk: &str) -> OwnerScope {
        OwnerScope::Chunk { pipeline_id: pipeline_id.to_string(), table: table.to_string(), chunk: chunk.to_string() }
    }

    pub fn host(id: &str) -> OwnerScope {
        OwnerScope::Host { host_scope: id.to_string() }
    }

    /// The seeding scope of a table: `<table>#seed`, apart from the live table's, so a
    /// seed's owner pins its own source identity and its cursor stays its own
    /// (`run.seed.scope`, `run.own.scope-independence`). A seed's chunks key on the same
    /// table segment.
    pub fn seed(pipeline_id: &str, table: &str) -> OwnerScope {
        OwnerScope::table(pipeline_id, &format!("{table}{SEED_SUFFIX}"))
    }

    /// Whether the scope is a seeding scope: a table or chunk scope whose table segment is
    /// `<table>#seed` (`run.seed.scope`).
    pub fn seeding(&self) -> bool {
        self.pipeline_table().is_some_and(|(_, table)| table.ends_with(SEED_SUFFIX))
    }

    /// The scope one delivered batch lands a table under: `<table>#receive-<digest>`, the
    /// digest a SHA-256 prefix of the batch key, so a resend under the key resumes its own
    /// pending owner and no other key's (`run.land.receive-batch-key`). The segment carries
    /// no byte of the sender's key.
    pub fn receive(pipeline_id: &str, table: &str, key: &str) -> OwnerScope {
        OwnerScope::table(pipeline_id, &format!("{table}{RECEIVE_INFIX}{}", receive_digest(pipeline_id, key)))
    }


    /// The pipeline and table a table or chunk scope names; `None` for a host scope.
    pub fn pipeline_table(&self) -> Option<(&str, &str)> {
        match self {
            OwnerScope::Table { pipeline_id, table } | OwnerScope::Chunk { pipeline_id, table, .. } => Some((pipeline_id, table)),
            OwnerScope::Host { .. } => None,
        }
    }

    /// The host scope id; `None` for a table or chunk scope.
    pub fn host_id(&self) -> Option<&str> {
        match self {
            OwnerScope::Host { host_scope } => Some(host_scope),
            _ => None,
        }
    }

    /// The name the live projection groups the scope's runs under: the pipeline, or the
    /// host scope id.
    pub fn workflow(&self) -> &str {
        match self {
            OwnerScope::Table { pipeline_id, .. } | OwnerScope::Chunk { pipeline_id, .. } => pipeline_id,
            OwnerScope::Host { host_scope } => host_scope,
        }
    }
}

impl std::fmt::Display for OwnerScope {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            OwnerScope::Table { pipeline_id, table } => write!(f, "pipeline `{pipeline_id}` table `{table}`"),
            OwnerScope::Chunk { pipeline_id, table, chunk } => write!(f, "pipeline `{pipeline_id}` table `{table}` chunk `{chunk}`"),
            OwnerScope::Host { host_scope } => write!(f, "host scope `{host_scope}`"),
        }
    }
}

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

/// Where admitted connector artifacts live, content-addressed by the hash a
/// [`ConnectorPin`] carries.
pub trait Artifacts {
    /// The bytes stored under `hash`; `None` when no admitted artifact holds it.
    fn by_hash(&self, hash: &str) -> Result<Option<Vec<u8>>, Failure>;
    /// The content hash a pin names `bytes` by: their sha256, unless the store's artifacts
    /// fold more into their pin, as a component's forwarded guest table does.
    fn hash_of(&self, bytes: &[u8]) -> String {
        super::journal::sha256_hex(bytes)
    }
}

/// The connector build a run of a scope executes: the pending owner's admitted pin, so a
/// replay resolves the artifact that owner recorded, else the build admitted now. A
/// connector rebuilt later reaches no in-flight or replayed run (`run.own.admission-pin`).
pub fn admission_pin<'a>(pending: Option<&'a ExecutionOwner>, admitted: &'a ConnectorPin) -> &'a ConnectorPin {
    match pending.map(|owner| &owner.pins) {
        Some(OwnerPins::Build(pins)) => &pins.connector,
        _ => admitted,
    }
}

/// The artifact `pin` names, looked up by its content hash alone. An absent artifact, or
/// bytes whose [`Artifacts::hash_of`] differs from the pin, refuses rather than running
/// another build.
pub fn resolve_pinned(pin: &ConnectorPin, artifacts: &dyn Artifacts) -> Result<Vec<u8>, Failure> {
    let bytes = artifacts
        .by_hash(&pin.hash)?
        .ok_or_else(|| Failure::deterministic(FailureTag::UnknownConnector, format!("connector {pin} is pinned and no admitted artifact holds hash {}; restore the recorded build", pin.hash)))?;
    let digest = artifacts.hash_of(&bytes);
    if digest != pin.hash {
        return Err(Failure::deterministic(FailureTag::UnknownConnector, format!("the artifact stored under {} hashes to {digest}; connector {pin} runs only its recorded bytes", pin.hash)));
    }
    Ok(bytes)
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

/// What a host owner pins while pending: a content-hashed plan reference and the
/// identities the host supplies (`run.own.host-scope`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlanPins {
    pub plan_ref: String,
    #[serde(default)]
    pub identities: BTreeMap<String, String>,
}

impl std::fmt::Display for PlanPins {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "plan reference {}", self.plan_ref)?;
        for (name, value) in &self.identities {
            write!(f, ", {name} {value}")?;
        }
        Ok(())
    }
}

/// What an owner pins: a connector build and pipeline plan for a table or chunk, a plan
/// reference and host identities for a host scope.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum OwnerPins {
    Build(Pins),
    Plan(PlanPins),
}

impl From<Pins> for OwnerPins {
    fn from(p: Pins) -> OwnerPins {
        OwnerPins::Build(p)
    }
}

impl From<PlanPins> for OwnerPins {
    fn from(p: PlanPins) -> OwnerPins {
        OwnerPins::Plan(p)
    }
}

impl OwnerPins {
    /// The content-hashed plan the pins name: the pipeline `content_hash` or the plan
    /// reference (`run.journal.plan-pin`).
    pub fn plan_ref(&self) -> &str {
        match self {
            OwnerPins::Build(p) => &p.content_hash,
            OwnerPins::Plan(p) => &p.plan_ref,
        }
    }

    /// The reference the declaration alone determines: a verified body owner's `input`
    /// identity, else the plan reference (`run.own.apply-retires-superseded`).
    pub fn declared_ref(&self) -> &str {
        match self {
            OwnerPins::Plan(p) => p.identities.get("input").unwrap_or(&p.plan_ref),
            OwnerPins::Build(p) => &p.content_hash,
        }
    }
}

impl std::fmt::Display for OwnerPins {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            OwnerPins::Build(p) => write!(f, "connector {} and plan {}", p.connector, p.content_hash),
            OwnerPins::Plan(p) => write!(f, "{p}"),
        }
    }
}

/// The durable execution owner of one scope (`run.own.execution-owner`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExecutionOwner {
    pub execution_id: String,
    #[serde(flatten)]
    pub scope: OwnerScope,
    pub pins: OwnerPins,
    /// Catalog run ids of the attempts made under this owner, oldest first.
    pub attempts: Vec<String>,
    pub opened_at: Instant,
    /// The instant a waiting attempt resumes at: the scheduler wakes the owner's unit then,
    /// and the next attempt's claim clears it (`run.retry.wake`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_attempt_at: Option<Instant>,
}

impl ExecutionOwner {
    /// Hold a pending owner to the build or plan reference now asked for. A moved
    /// connector identity, component world, plan hash, plan reference or host identity
    /// refuses before any replay (`run.own.pinned-plan-changed`); the input hash is no pin.
    pub fn check_pins(&self, now: &OwnerPins) -> Result<(), RunError> {
        let held = match (&self.pins, now) {
            (OwnerPins::Build(p), OwnerPins::Build(n)) => p.connector == n.connector && p.content_hash == n.content_hash,
            (OwnerPins::Plan(p), OwnerPins::Plan(n)) => p == n,
            _ => false,
        };
        if held {
            return Ok(());
        }
        let (restore, asks) = match self.pins {
            OwnerPins::Build(_) => ("build", "run"),
            OwnerPins::Plan(_) => ("plan", "execution"),
        };
        // A chunk owner is no rewind target, so its refusal names the restore alone
        // (`run.own.pin-recovery`).
        let rewind = match self.scope {
            OwnerScope::Chunk { .. } => String::new(),
            _ => format!(", or retire it with `contextful run rewind {} --reason <text>`", self.execution_id),
        };
        Err(RunError::ExecutionPinMismatch(format!(
            "{} has a pending execution `{}` pinned to {}; this {asks} asks for {now}. Restore the recorded {restore} and resume{rewind}",
            self.scope, self.execution_id, self.pins
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
