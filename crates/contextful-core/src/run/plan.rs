//! The plan a run starts against: serialized, content-hashed, and resolved for the run's
//! whole life (`run.journal.plan-pin`).
//!
//! The plan file is TOML naming the pipeline, the table it lands, the connector with the
//! command that serves its pulls, the cursor, an optional retry schedule, and the
//! journaling and redaction declarations the journal is held to.

use super::advance::CursorKind;
use super::journal::sha256_hex;
use super::own::ConnectorPin;
use super::retry::{Schedule, ScheduleSpec};
use super::RunError;
use crate::store::reserve::check_table_name;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConnectorSpec {
    pub id: String,
    pub version: String,
    /// The component world the connector implements, or `native`.
    #[serde(default = "native")]
    pub world: String,
    /// The argv serving one pull.
    pub command: Vec<String>,
}

fn native() -> String {
    NATIVE_WORLD.to_string()
}

/// The world a native connector declares.
pub const NATIVE_WORLD: &str = "native";

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CursorSpec {
    #[serde(default)]
    pub kind: Option<String>,
    /// The `incremental` clock field a monotonic position is measured against.
    #[serde(default)]
    pub field: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PlanSpec {
    pub pipeline: String,
    pub table: String,
    pub connector: ConnectorSpec,
    #[serde(default)]
    pub cursor: CursorSpec,
    #[serde(default)]
    pub retry: Option<ScheduleSpec>,
    /// Whether the source journals its pulls. Pull journaling defaults on (`run.journal.opt-out`).
    #[serde(default = "journal_default")]
    pub journal: bool,
    /// Columns redacted on the write path.
    #[serde(default)]
    pub redact: Vec<String>,
}

fn journal_default() -> bool {
    true
}

/// A compiled plan and the hash that names it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Plan {
    pub spec: PlanSpec,
    /// `sha256` of the plan's bytes: the reference a run pins.
    pub content_hash: String,
    pub cursor_kind: CursorKind,
    pub schedule: Schedule,
}

impl Plan {
    /// Compile a plan from its serialized bytes.
    pub fn compile(bytes: &[u8]) -> Result<Plan, RunError> {
        let text = std::str::from_utf8(bytes).map_err(|e| RunError::Invalid(format!("a plan is UTF-8 TOML: {e}")))?;
        let spec: PlanSpec = toml::from_str(text).map_err(|e| RunError::Invalid(format!("plan: {e}")))?;
        let cursor_kind = CursorKind::resolve(spec.cursor.kind.as_deref())?;
        if cursor_kind == CursorKind::Monotonic && spec.cursor.field.is_none() {
            return Err(RunError::Invalid("a `monotonic` cursor names its `field`".into()));
        }
        if spec.connector.command.is_empty() {
            return Err(RunError::Invalid(format!("connector `{}` names no command", spec.connector.id)));
        }
        check_table_name(&spec.table).map_err(|e| RunError::Invalid(e.to_string()))?;
        let schedule = spec.retry.clone().unwrap_or_default().compile()?;
        let plan = Plan { spec, content_hash: sha256_hex(bytes), cursor_kind, schedule };
        plan.validate()?;
        Ok(plan)
    }

    /// The declarations the journal is held to, checked at manifest validation and again
    /// at run open (`run.journal.redacting-source`).
    pub fn validate(&self) -> Result<(), RunError> {
        if self.spec.journal && !self.spec.redact.is_empty() {
            return Err(RunError::JournalRedactionConflict(format!(
                "pipeline `{}` redacts {} on the write path over a source that journals its pulls; the journal would hold the pre-redaction values. Declare `journal = false`",
                self.spec.pipeline,
                self.spec.redact.iter().map(|c| format!("`{c}`")).collect::<Vec<_>>().join(", ")
            )));
        }
        if !self.spec.redact.is_empty() {
            return Err(RunError::Invalid(format!(
                "pipeline `{}` declares write-path redaction of {}; this build links no write-path redaction and refuses rather than landing the values unredacted",
                self.spec.pipeline,
                self.spec.redact.iter().map(|c| format!("`{c}`")).collect::<Vec<_>>().join(", ")
            )));
        }
        Ok(())
    }

    /// The connector build this plan admits, given the artifact's content hash.
    pub fn connector_pin(&self, artifact_hash: &str) -> ConnectorPin {
        let c = &self.spec.connector;
        ConnectorPin { id: c.id.clone(), version: c.version.clone(), world: c.world.clone(), hash: artifact_hash.to_string() }
    }
}
