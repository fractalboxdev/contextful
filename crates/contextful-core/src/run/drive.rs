//! The store-driven run kind's domain: the plan pins a job's input joins, the input set its
//! first step records, the row key and step labels a row's work records under, the body an
//! embedding binary registers, and the calls a body makes through the journal
//! (`run.journal.store-input`, `run.journal.row-step`, `run.suspend.row-parks`).

use super::journal::sha256_hex;
use super::own::{OwnerPins, PlanPins};
use super::ports::Row;
use super::record::InputBounds;
use super::{Failure, FailureTag, RunError};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::collections::BTreeMap;
use std::sync::Arc;

/// The label of the step recording a store-driven run's input set.
pub const INPUT_STEP: &str = "input";

/// Rows per output table, keyed by the table's name in the job's `tables`.
pub type Emitted = BTreeMap<String, Vec<Row>>;

/// Canonically admitted effect groups, with no raw model result exposed to the body.
pub type PreparedEmitted = BTreeMap<String, Vec<super::effect::PreparedEmission>>;

/// What a store-driven job pins: the body it names, its input statement and the `as_of`
/// it declares, absent when the execution resolves one at open (`run.journal.input-pin`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoreInput {
    pub body: String,
    pub statement: String,
    pub as_of: Option<String>,
}

impl StoreInput {
    /// The content hash of the body name, statement and declared `as_of`.
    pub fn plan_ref(&self) -> String {
        let canonical = json!({ "kind": "store-driven", "body": self.body, "statement": self.statement, "as_of": self.as_of });
        sha256_hex(canonical.to_string().as_bytes())
    }

    /// The pins a host owner holds while the job's execution is pending; a resume under a
    /// changed statement or `as_of` refuses with `ExecutionPinMismatch`.
    pub fn pins(&self) -> OwnerPins {
        let mut identities = BTreeMap::from([("body".to_string(), self.body.clone()), ("statement".to_string(), sha256_hex(self.statement.as_bytes()))]);
        identities.insert("as_of".to_string(), self.as_of.clone().unwrap_or_else(|| "open".to_string()));
        PlanPins { plan_ref: self.plan_ref(), identities }.into()
    }
}

/// The input set a store-driven run's first step records: the resolved `as_of`, the
/// snapshot id per table the statement touched, and the rows in statement order.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct InputSet {
    pub as_of: String,
    pub snapshots: BTreeMap<String, String>,
    pub rows: Vec<Row>,
}

impl InputSet {
    /// The input set a read answered at `as_of`: each response row as a row of its
    /// columns. A response the face truncated at its row ceiling raises
    /// `RunInputTruncated` (`run.journal.input-truncated`).
    pub fn from_response(
        as_of: &str,
        snapshots: BTreeMap<String, String>,
        columns: &[String],
        rows: Vec<Vec<serde_json::Value>>,
        truncated: bool,
    ) -> Result<InputSet, RunError> {
        if truncated {
            return Err(RunError::RunInputTruncated(format!(
                "the input statement answered more rows than the read face delivers; {} rows came back at as_of {as_of}, and a store-driven run reads its whole input or none",
                rows.len()
            )));
        }
        let rows = rows.into_iter().map(|cells| columns.iter().cloned().zip(cells).collect()).collect();
        Ok(InputSet { as_of: as_of.to_string(), snapshots, rows })
    }

    /// The bounds the run row carries (`run.record.input-bounds`).
    pub fn bounds(&self) -> InputBounds {
        InputBounds { as_of: self.as_of.clone(), snapshots: self.snapshots.clone(), rows: self.rows.len() as u64 }
    }

    /// Every input row under its row key, in recorded order.
    pub fn keyed(&self) -> Vec<InputRow> {
        self.rows.iter().enumerate().map(|(i, row)| InputRow { key: row_key(i), row: row.clone() }).collect()
    }

    /// The recorded bytes of the input step.
    pub fn encode(&self) -> Vec<u8> {
        serde_json::to_vec(self).expect("an input set serializes")
    }

    pub fn decode(bytes: &[u8]) -> Result<InputSet, Failure> {
        serde_json::from_slice(bytes).map_err(|e| Failure::deterministic(FailureTag::Permanent, format!("the recorded input step does not decode: {e}")))
    }
}

/// A row's key: its ordinal in the recorded input set.
pub fn row_key(ordinal: usize) -> String {
    ordinal.to_string()
}

/// The label a step of row `key` records under, scoped to the row (`run.journal.row-step`).
pub fn row_label(key: &str, label: &str) -> String {
    format!("row/{key}/{label}")
}

/// One input row as the body receives it.
#[derive(Debug, Clone, PartialEq)]
pub struct InputRow {
    pub key: String,
    pub row: Row,
}

/// A wait's outcome as the body reads it; a timeout is a recorded value like a payload
/// (`run.suspend.recorded-timeout`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Woken {
    Resumed(Vec<u8>),
    TimedOut,
}

impl Woken {
    /// The bytes a wait's step records.
    pub fn encode(&self) -> Vec<u8> {
        match self {
            Woken::TimedOut => vec![0],
            Woken::Resumed(p) => std::iter::once(1).chain(p.iter().copied()).collect(),
        }
    }

    pub fn decode(bytes: &[u8]) -> Result<Woken, Failure> {
        match bytes.split_first() {
            Some((0, [])) => Ok(Woken::TimedOut),
            Some((1, payload)) => Ok(Woken::Resumed(payload.to_vec())),
            _ => Err(Failure::deterministic(FailureTag::Permanent, "a recorded wait does not decode")),
        }
    }
}

/// Why a row's body stopped short of emitting.
#[derive(Debug, Clone, PartialEq)]
pub enum RowStop {
    /// The row awaits an unresolved awakeable; it re-enters once that resolves or times out.
    Parked,
    Failed(Failure),
}

impl From<Failure> for RowStop {
    fn from(f: Failure) -> RowStop {
        RowStop::Failed(f)
    }
}

/// A paid call's effect: it receives the idempotency key of its entry and answers the
/// bytes to record.
pub type CallEffect<'a> = dyn FnMut(&str) -> Result<Vec<u8>, Failure> + 'a;

/// The calls a body makes; each resolves through the journal under a label scoped to the row.
pub trait RowCalls {
    /// Record one paid call under `label` and `input`: a recorded entry answers without the
    /// effect, and every entry of the effect carries the entry's idempotency key.
    fn call(&self, label: &str, input: &[u8], effect: &mut CallEffect<'_>) -> Result<Vec<u8>, RowStop>;
    /// Mint this row's awakeable under `label` with a time-to-live of `ttl_secs`, once: the
    /// token is a recorded step, so every re-entry answers the same one.
    fn suspend(&self, label: &str, ttl_secs: u64) -> Result<String, RowStop>;
    /// The outcome of the awakeable `token` minted under `label`: `Parked` while it is
    /// unresolved, else the value recorded the first time it resolved or timed out.
    fn awaited(&self, label: &str, token: &str) -> Result<Woken, RowStop>;
}

/// Compiled per-row work an embedding binary registers by name. Configuration names it and
/// never supplies a command (`surface.fire.store-driven-body`).
pub trait RowBody: Send + Sync {
    /// Run one input row, every effect through `calls`, answering its rows per output table.
    fn run(&self, row: &InputRow, calls: &dyn RowCalls) -> Result<Emitted, RowStop>;
    /// A compiled closed projection enables prepared effect recording; ordinary bodies
    /// retain their existing calls and emission behavior.
    fn recorded(&self) -> Option<&dyn RecordedRowBody> { None }
}

/// Paid calls whose results pass canonical removal before the existing journal records them.
pub trait RecordedRowCalls {
    fn call(&self, label: &str, input: &[u8], effect: &mut CallEffect<'_>) -> Result<super::effect::PreparedEmission, RowStop>;
}

/// A registered projection supplies opaque emitted groups, not arbitrary source bytes.
/// Protected awakeables have no admitted result projection in this interface.
pub trait RecordedRowBody: Send + Sync {
    fn effects(&self) -> Vec<super::effect::RecordedEffect>;
    fn run_recorded(&self, row: &InputRow, calls: &dyn RecordedRowCalls) -> Result<PreparedEmitted, RowStop>;
}

/// The row bodies an embedding binary registers before build.
#[derive(Clone, Default)]
pub struct Bodies {
    bodies: BTreeMap<String, Arc<dyn RowBody>>,
}

impl std::fmt::Debug for Bodies {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_list().entries(self.bodies.keys()).finish()
    }
}

impl Bodies {
    /// Register `body` under `name`; a blank or taken name refuses.
    pub fn register(&mut self, name: &str, body: Arc<dyn RowBody>) -> Result<(), RunError> {
        if name.trim().is_empty() || self.bodies.contains_key(name) {
            return Err(RunError::Invalid(format!("a row body is already named `{name}`, or the name is blank")));
        }
        self.bodies.insert(name.to_string(), body);
        Ok(())
    }

    pub fn get(&self, name: &str) -> Option<Arc<dyn RowBody>> {
        self.bodies.get(name).cloned()
    }

    /// Every registered name, sorted.
    pub fn names(&self) -> Vec<String> {
        self.bodies.keys().cloned().collect()
    }
}
