//! `run.bind.host-task` and the host half of `run.emit`: compiled derive tasks an
//! embedding binary registers by name before build, the key a host unit derives under,
//! and the rows it lands across a marker table and its content tables.

use super::config::{DeriveConfig, OrderBy, check_output_table, check_reserved_discriminator};
use super::emit::{
    DERIVATION_KEY, DERIVED_ID, EMPTY_ATTEMPTS, KIND, MARKER_SEQ, UnitStatus, by_unit, derived_id, redact, standing,
};
use crate::connector::infer::{DataItem, Provenance, fence, taint};
use crate::memory::synthesize::{CallRecord, Message};
use crate::run::RunError;
use crate::run::journal::sha256_hex;
use crate::run::ports::Row;
use crate::store::declare::TableDecl;
use crate::time::Instant;
use serde_json::{Map, Value, json};
use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

/// The tasks the engine names; a host task takes any other name.
pub const BUILT_IN_TASKS: [&str; 2] = ["transcribe", "link_preview"];
/// The column every host row carries its task's version in (`run.emit.task-version`).
pub const TASK_VERSION: &str = "task_version";

/// Rows per content table, keyed by the table's name in the pipeline's `tables`.
pub type Derived = BTreeMap<String, Vec<Row>>;

/// One unit of a host task: the parent row's key and the columns the task declares.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct HostUnit {
    pub key: String,
    /// The parent row, holding the task's declared columns.
    pub row: Row,
    /// Per table the task joins, every row whose join column holds the unit's key
    /// (`run.select.unit-parents`).
    pub joined: BTreeMap<String, Vec<Row>>,
    /// Attempts its marker under `derivation_key` records so far.
    pub prior_attempts: i64,
    /// The key its rows land under.
    pub derivation_key: String,
}

/// A table a host unit joins beside its driving row: every row whose `key_column` holds the
/// unit's key, carrying `columns` (`run.select.unit-parents`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Join {
    pub table: String,
    pub key_column: String,
    pub columns: Vec<String>,
}

impl Join {
    /// The columns a read of the joined table names: its key column, then `columns`.
    pub fn read_columns(&self) -> Vec<&str> {
        let mut columns = vec![self.key_column.as_str()];
        columns.extend(self.columns.iter().map(String::as_str).filter(|c| *c != self.key_column));
        columns
    }
}

/// One request a host task sends through the run's metered client.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Request {
    pub method: String,
    pub url: String,
    pub headers: Vec<(String, String)>,
    pub body: Option<Vec<u8>>,
}

/// A metered client's answer to one [`Request`].
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Answer {
    pub status: u16,
    pub body: Vec<u8>,
}

/// The run's metered client, the network reach the engine hands a host task that is not
/// pure (`run.bind.reach-client`). It reaches the task's model endpoint host alone, and a
/// request it refuses answers `DeriveMeteredClient` (`run.bind.model-endpoint`,
/// `run.bind.off-endpoint`).
pub trait MeteredClient: Send + Sync {
    /// Send `request` under the run's meter, answering its status and body.
    fn send(&self, request: &Request) -> Result<Answer, RunError>;
    /// Send one chat completion to the run's model endpoint under the run's meter.
    fn complete(&self, messages: &[Message]) -> Result<CallRecord, RunError>;
}

/// One model call a host task makes: the operator's template, sent in the system role and
/// never fenced, and the values the call reads, each with its provenance, which enter the
/// prompt inside data boundary markers (`run.bind.model-fence`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ModelCall {
    pub template: String,
    pub items: Vec<(Provenance, DataItem)>,
    /// Characters each value keeps (`connector.infer.value-cap`).
    pub cap: usize,
    /// The caller's rules, restated after the blocks.
    pub rules: String,
}

/// A model call's answer and the label its output carries (`connector.infer.output-taint`).
#[derive(Debug, Clone, PartialEq)]
pub struct Inferred {
    pub record: CallRecord,
    pub taint: Provenance,
}

/// The run's metered client as a task holds it: a request the client refuses is recorded
/// for [`Reach::refusal`] (`run.bind.off-endpoint`).
pub struct Metered<'r> {
    client: &'r dyn MeteredClient,
    refused: &'r std::sync::Mutex<Option<RunError>>,
}

impl Metered<'_> {
    /// Send `request` under the run's meter; a refusal fails the tick whether or not the
    /// task returns it.
    pub fn send(&self, request: &Request) -> Result<Answer, RunError> {
        let answer = self.client.send(request);
        if let Err(e @ RunError::DeriveMeteredClient(_)) = &answer {
            record(self.refused, e);
        }
        answer
    }
}

fn record(slot: &std::sync::Mutex<Option<RunError>>, e: &RunError) {
    let mut refused = slot.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    refused.get_or_insert_with(|| e.clone());
}

/// What the engine hands a host task while it derives one unit: the run's metered client
/// when the task is not pure and the run binds one, else nothing. A request for an absent
/// client raises `DeriveMeteredClient` and the engine fails the tick, whether or not the
/// task returns the error (`run.bind.pure-task`, `run.bind.reach-client`).
pub struct Reach<'a> {
    task: &'a str,
    pure: bool,
    client: Option<&'a dyn MeteredClient>,
    refused: std::sync::Mutex<Option<RunError>>,
}

impl<'a> Reach<'a> {
    /// The reach of `task`, registered as `name`, under the run's `client`: a pure task's
    /// reach holds no client whatever the run binds.
    pub fn for_task(name: &'a str, task: &dyn DeriveTask, client: Option<&'a dyn MeteredClient>) -> Reach<'a> {
        let pure = task.pure();
        Reach { task: name, pure, client: if pure { None } else { client }, refused: Default::default() }
    }

    /// The reach of a pure task named `name`: no client.
    pub fn none(name: &'a str) -> Reach<'a> {
        Reach { task: name, pure: true, client: None, refused: Default::default() }
    }

    /// The run's metered client; a pure task, or a run binding none, raises
    /// `DeriveMeteredClient`, recorded for [`Reach::refusal`].
    pub fn client(&self) -> Result<Metered<'_>, RunError> {
        self.bound().map(|client| Metered { client, refused: &self.refused })
    }

    fn bound(&self) -> Result<&'a dyn MeteredClient, RunError> {
        if let Some(client) = self.client {
            return Ok(client);
        }
        let e = RunError::DeriveMeteredClient(if self.pure {
            format!("host task `{}` is pure and receives no network client; a task reaching the network declares `pure` false", self.task)
        } else {
            format!("host task `{}` asks for a metered client and the run binds none", self.task)
        });
        record(&self.refused, &e);
        Err(e)
    }

    /// One model call through the run's metered client: `call.template` in the system role,
    /// then every item inside its data boundary markers under one batch token, answering the
    /// call's record and its least-trusted input's label (`run.bind.model-fence`).
    pub fn infer(&self, call: &ModelCall) -> Result<Inferred, RunError> {
        let client = self.bound()?;
        let items: Vec<DataItem> = call.items.iter().map(|(_, item)| item.clone()).collect();
        let messages = [Message::new("system", call.template.clone()), Message::new("user", fence(&items, call.cap, &call.rules))];
        let answer = client.complete(&messages);
        if let Err(e @ RunError::DeriveMeteredClient(_)) = &answer {
            record(&self.refused, e);
        }
        Ok(Inferred { record: answer?, taint: taint(call.items.iter().map(|(label, _)| *label)) })
    }

    /// The first refused request for a client, which fails the tick.
    pub fn refusal(&self) -> Option<RunError> {
        self.refused.lock().unwrap_or_else(std::sync::PoisonError::into_inner).clone()
    }
}

/// A derive task compiled into the embedding binary. Configuration names it; it never
/// names a command (`run.bind.host-task`).
pub trait DeriveTask: Send + Sync {
    /// The version its output derives under; raising it re-selects every settled unit.
    fn version(&self) -> &str;
    /// Whether the task reaches no network. A pure task receives no metered client; one
    /// declaring `false` receives the run's (`run.bind.pure-task`).
    fn pure(&self) -> bool {
        true
    }
    /// The parent-table columns a unit carries besides its key column.
    fn columns(&self) -> Vec<String>;
    /// The tables a unit joins beside its driving row; none by default.
    fn joins(&self) -> Vec<Join> {
        Vec::new()
    }
    /// The table its markers land in, the anti-join's side.
    fn marker_table(&self) -> String;
    /// The tables its content rows land in, in landing order.
    fn content_tables(&self) -> Vec<String>;
    /// Derive one unit into rows per content table, reaching the network only through
    /// `reach`; an error lands a `failed` marker.
    fn derive(&self, unit: &HostUnit, reach: &Reach<'_>) -> Result<Derived, RunError>;
}

/// The host tasks an embedding binary registers before build.
#[derive(Clone, Default)]
pub struct Tasks {
    tasks: BTreeMap<String, Arc<dyn DeriveTask>>,
}

impl std::fmt::Debug for Tasks {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_list().entries(self.tasks.keys()).finish()
    }
}

impl Tasks {
    /// Register `task` under `name`; a built-in or already registered name refuses, and so
    /// does a task whose joins name one table twice (`run.bind.join-repeated`).
    pub fn register(&mut self, name: &str, task: Arc<dyn DeriveTask>) -> Result<(), RunError> {
        if name.trim().is_empty() || BUILT_IN_TASKS.contains(&name) || self.tasks.contains_key(name)
        {
            return Err(RunError::DeriveTaskNameTaken(format!(
                "a derive task is already named `{name}`; taken names: {}",
                BUILT_IN_TASKS
                    .iter()
                    .copied()
                    .chain(self.names())
                    .collect::<Vec<_>>()
                    .join(", ")
            )));
        }
        let joins = task.joins();
        for (i, join) in joins.iter().enumerate() {
            if let Some(again) = joins[..i].iter().find(|j| j.table == join.table) {
                return Err(RunError::DeriveJoinRepeated(format!(
                    "task `{name}` joins `{}` on `{}` and again on `{}`; a unit holds one joined list per table",
                    join.table, again.key_column, join.key_column
                )));
            }
        }
        self.tasks.insert(name.to_string(), task);
        Ok(())
    }

    pub fn get(&self, name: &str) -> Option<Arc<dyn DeriveTask>> {
        self.tasks.get(name).cloned()
    }

    /// Every registered name, sorted.
    pub fn names(&self) -> Vec<&str> {
        self.tasks.keys().map(String::as_str).collect()
    }
}

/// The key a host unit derives under: lowercase hex SHA-256 over the task's name and
/// version and the parent row's id, declared columns and own key (`run.emit.task-version`).
pub fn host_key(
    task: &str,
    version: &str,
    parent_id: &str,
    row: &Row,
    columns: &[String],
) -> String {
    let declared: Map<String, Value> = columns
        .iter()
        .map(|c| (c.clone(), row.get(c).cloned().unwrap_or(Value::Null)))
        .collect();
    let doc = json!({
        "task": task,
        "version": version,
        "parent": { "id": parent_id, "columns": declared, "key": row.get(DERIVATION_KEY) },
    });
    sha256_hex(&serde_json::to_vec(&doc).expect("a JSON value serializes"))
}

fn text(v: Option<&Value>) -> Option<String> {
    match v? {
        Value::String(s) if !s.trim().is_empty() => Some(s.clone()),
        Value::Number(n) => Some(n.to_string()),
        _ => None,
    }
}

/// The outstanding host units of one tick.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct HostSelection {
    pub outstanding: Vec<HostUnit>,
    /// Parent rows skipped for a missing key.
    pub incomplete: Vec<RunError>,
    /// The counts a dry run prints (`run.select.dry-run`).
    pub counts: super::emit::Counts,
}

/// The landing column a host task's `since` compares (`run.select.since`).
pub const INGESTED_AT: &str = "_ingested_at";

/// Whether `row` landed at or after `since`; a row with no readable `_ingested_at` did not
/// (`run.select.since`).
pub fn ingested_since(row: &Row, since: Instant) -> bool {
    row.get(INGESTED_AT)
        .and_then(Value::as_str)
        .and_then(|s| Instant::parse(s).ok())
        .is_some_and(|at| at >= since)
}

/// Keep the first `budget` of `units` in `order`, ties broken by unit key (`run.select.order`).
pub fn keep_ordered(units: &mut Vec<HostUnit>, order: &OrderBy, budget: usize) {
    units.sort_by(|a, b| order.compare((&a.key, a.row.get(&order.column)), (&b.key, b.row.get(&order.column))));
    units.truncate(budget);
}

/// The outstanding host units of one tick: every parent row the task's `filter` matches and
/// its `sample_rate` keeps, holding no settled marker under its current key in the marker
/// table, ordered by `order_by` and truncated to the run's row budget after the anti-join.
/// An ordered selection holds at most twice the budget in units, cutting back to the budget
/// each time it fills (`run.select.filter`, `run.select.sample`, `run.select.anti-join`,
/// `run.select.order`).
pub fn select_host(
    parents: &[Row],
    markers: &[Row],
    config: &DeriveConfig,
    task: &dyn DeriveTask,
) -> HostSelection {
    let units = by_unit(markers);
    let columns = task.columns();
    let budget = usize::try_from(config.max_rows_per_run).unwrap_or(usize::MAX);
    let mut sel = HostSelection::default();
    let mut seen = BTreeSet::new();
    for r in parents {
        let Some(key) = text(r.get(&config.parent_id_column)) else {
            sel.incomplete.push(RunError::DeriveUnitIncomplete(format!(
                "a `{}` row lacks `{}`; it is skipped",
                config.source_table, config.parent_id_column
            )));
            continue;
        };
        if !seen.insert(key.clone()) {
            continue;
        }
        if config.filter.as_ref().is_some_and(|f| !f.0.matches(r)) {
            continue;
        }
        if config
            .sample_rate
            .is_some_and(|rate| !rate.keeps(&key, config.task.name()))
        {
            continue;
        }
        sel.counts.eligible += 1;
        let derivation_key = host_key(config.task.name(), task.version(), &key, r, &columns);
        let prior = match units.get(&key) {
            None => 0,
            Some(rows) => match standing(rows, &derivation_key, config.max_attempts) {
                None => {
                    sel.counts.derived += 1;
                    continue;
                }
                Some(attempts) => attempts,
            },
        };
        sel.counts.outstanding += 1;
        sel.outstanding.push(HostUnit {
            key,
            row: r.clone(),
            joined: BTreeMap::new(),
            prior_attempts: prior,
            derivation_key,
        });
        if let Some(order) = &config.order_by {
            if sel.outstanding.len() >= budget.saturating_mul(2).max(1) {
                keep_ordered(&mut sel.outstanding, order, budget);
            }
        }
    }
    match &config.order_by {
        Some(order) => keep_ordered(&mut sel.outstanding, order, budget),
        None => sel.outstanding.truncate(budget),
    }
    sel
}

/// Hand each unit the rows of `join`'s table whose join column holds its key, an empty list
/// where none does; a row whose join column is null or blank joins no unit
/// (`run.select.unit-parents`).
pub fn join_units(units: &mut [HostUnit], join: &Join, rows: Vec<Row>) {
    let mut by_key: BTreeMap<String, Vec<Row>> = BTreeMap::new();
    for r in rows {
        if let Some(key) = text(r.get(&join.key_column)) {
            by_key.entry(key).or_default().push(r);
        }
    }
    for unit in units {
        let rows = by_key.get(&unit.key).cloned().unwrap_or_default();
        unit.joined.insert(join.table.clone(), rows);
    }
}

/// Whether the marker table already holds `unit` settled under its key, as a concurrent
/// tick lands it (`run.emit.settled-revived`).
pub fn host_revived(unit: &HostUnit, markers: &[Row], max_attempts: i64) -> Option<RunError> {
    let units = by_unit(markers);
    let rows = units.get(&unit.key)?;
    standing(rows, &unit.derivation_key, max_attempts).is_none().then(|| {
        RunError::DeriveSettledUnitRevived(format!(
            "unit `{}` settled under key `{}` while this tick derived it; its rows are not landed",
            unit.key,
            &unit.derivation_key[..unit.derivation_key.len().min(12)]
        ))
    })
}

/// The engine id a host task's rows carry: `host:<name>@<version>`.
pub fn host_engine_id(task: &str, version: &str) -> String {
    format!("host:{task}@{version}")
}

fn stamp(row: &mut Row, unit: &HostUnit, version: &str) {
    row.insert("unit_ref".into(), json!(unit.key));
    row.insert(DERIVATION_KEY.into(), json!(unit.derivation_key));
    row.insert(TASK_VERSION.into(), json!(version));
}

/// A unit's one marker row in the marker table.
pub fn host_marker(
    unit: &HostUnit,
    task: &str,
    version: &str,
    status: UnitStatus,
    error: Option<&str>,
    retryable: bool,
) -> Row {
    let attempts = if status == UnitStatus::Empty {
        EMPTY_ATTEMPTS
    } else {
        unit.prior_attempts + 1
    };
    let mut row: Row = json!({
        "cue_seq": MARKER_SEQ, KIND: "marker", "unit_status": status.name(), "attempts": attempts,
        "last_error": error.map(redact), "retryable": retryable, "engine_id": host_engine_id(task, version),
    })
    .as_object()
    .cloned()
    .unwrap_or_default();
    stamp(&mut row, unit, version);
    row.insert(DERIVED_ID.into(), json!(derived_id(&unit.key, &unit.derivation_key, MARKER_SEQ)));
    row
}

/// The one `empty` marker a content table takes for a unit landing no row in it, so the
/// table's own rows carry the unit's latest key (`run.emit.content-empty`).
fn content_marker(unit: &HostUnit, version: &str) -> Row {
    let mut row: Row = json!({ KIND: "marker", "unit_status": UnitStatus::Empty.name() })
        .as_object()
        .cloned()
        .unwrap_or_default();
    stamp(&mut row, unit, version);
    row
}

/// The rows one unit's derivation lands as, per table name: its content rows stamped with
/// the unit's key, derivation key and task version, an `empty` marker in each content table
/// it returns no row for, and one marker — `ok` over content rows, `empty` over none,
/// `failed` on an error or a row for an undeclared table (`run.emit.host-rows`,
/// `run.emit.content-empty`, `run.emit.output-tables`).
pub fn host_rows(
    unit: &HostUnit,
    task_name: &str,
    task: &dyn DeriveTask,
    outcome: Result<Derived, RunError>,
) -> Derived {
    let version = task.version();
    let content: BTreeSet<String> = task.content_tables().into_iter().collect();
    let marker = |status, error: Option<&str>, retryable| {
        host_marker(unit, task_name, version, status, error, retryable)
    };
    let outcome = outcome.and_then(|derived| match derived.keys().find(|t| !content.contains(*t)) {
        Some(t) => Err(RunError::DeriveOutputTablesMismatch(format!(
            "task `{task_name}` returned rows for `{t}`, which is none of its content tables {}",
            content.iter().cloned().collect::<Vec<_>>().join(", ")
        ))),
        None => Ok(derived),
    });
    let mut out = Derived::new();
    match outcome {
        Err(e) => {
            out.insert(
                task.marker_table(),
                vec![marker(UnitStatus::Failed, Some(&e.to_string()), true)],
            );
        }
        Ok(derived) => {
            let mut any = false;
            for (table, rows) in derived {
                let stamped: Vec<Row> = rows
                    .into_iter()
                    .map(|mut r| {
                        stamp(&mut r, unit, version);
                        r.insert(KIND.into(), json!("passage"));
                        r.insert("unit_status".into(), json!(UnitStatus::Ok.name()));
                        r
                    })
                    .collect();
                any |= !stamped.is_empty();
                if !stamped.is_empty() {
                    out.insert(table, stamped);
                }
            }
            for table in &content {
                out.entry(table.clone())
                    .or_insert_with(|| vec![content_marker(unit, version)]);
            }
            let status = if any {
                UnitStatus::Ok
            } else {
                UnitStatus::Empty
            };
            out.insert(task.marker_table(), vec![marker(status, None, false)]);
        }
    }
    out
}

/// The order a host task's tables land in: every content table, then the marker table
/// (`run.emit.marker-last`).
pub fn landing_order(task: &dyn DeriveTask) -> Vec<String> {
    let marker = task.marker_table();
    let mut order: Vec<String> = task
        .content_tables()
        .into_iter()
        .filter(|t| *t != marker)
        .collect();
    order.push(marker);
    order
}

/// Hold a host-task pipeline's declared tables to its task's: exactly the marker table and
/// the content tables, the marker keyed as every derive output table, and each content
/// table keyed opening with `unit_ref` and `derivation_key` (`run.emit.output-tables`,
/// `run.emit.primary-key`).
pub fn check_host_tables(
    pipeline_id: &str,
    task_name: &str,
    task: &dyn DeriveTask,
    tables: &[TableDecl],
) -> Result<(), RunError> {
    let declared: BTreeSet<&str> = tables.iter().map(|t| t.name.as_str()).collect();
    let wanted = landing_order(task);
    let wanted_set: BTreeSet<&str> = wanted.iter().map(String::as_str).collect();
    if declared != wanted_set || declared.len() != tables.len() {
        return Err(RunError::DeriveOutputTablesMismatch(format!(
            "pipeline `{pipeline_id}` declares tables {}; task `{task_name}` lands in {}",
            tables
                .iter()
                .map(|t| t.name.as_str())
                .collect::<Vec<_>>()
                .join(", "),
            wanted.join(", ")
        )));
    }
    let marker = task.marker_table();
    for t in tables {
        if t.name == marker {
            check_output_table(t)?;
        } else {
            check_content_table(t)?;
        }
    }
    Ok(())
}

/// The columns a host content table's key opens with.
pub const CONTENT_KEY_PREFIX: [&str; 2] = ["unit_ref", DERIVATION_KEY];

/// Refuse a host content table that names the reserved `kind` column, or whose key does not
/// open with [`CONTENT_KEY_PREFIX`].
pub fn check_content_table(table: &TableDecl) -> Result<(), RunError> {
    check_reserved_discriminator(table)?;
    let pk = table.primary_key();
    if !is_derive_key(pk) {
        return Err(RunError::DerivePrimaryKeyMissing(format!(
            "host content table `{}` declares `primary_key = {pk:?}`; a content table's key opens with [\"unit_ref\", \"derivation_key\"]",
            table.name
        )));
    }
    Ok(())
}

/// Whether a table's primary key marks it as a derive table: opening with `unit_ref` and
/// `derivation_key`.
pub fn is_derive_key(pk: &[String]) -> bool {
    pk.len() >= 2 && pk[..2].iter().zip(CONTENT_KEY_PREFIX).all(|(a, b)| a == b)
}
