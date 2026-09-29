//! `run.bind.host-task` and the host half of `run.emit`: compiled derive tasks an
//! embedding binary registers by name before build, the key a host unit derives under,
//! and the rows it lands across a marker table and its content tables.

use super::config::{DeriveConfig, check_output_table, check_reserved_discriminator};
use super::emit::{
    DERIVATION_KEY, EMPTY_ATTEMPTS, KIND, MARKER_SEQ, UnitStatus, by_unit, redact, standing,
};
use crate::run::RunError;
use crate::run::journal::sha256_hex;
use crate::run::ports::Row;
use crate::store::declare::TableDecl;
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
#[derive(Debug, Clone, PartialEq)]
pub struct HostUnit {
    pub key: String,
    /// The parent row, holding the task's declared columns.
    pub row: Row,
    /// Attempts its marker under `derivation_key` records so far.
    pub prior_attempts: i64,
    /// The key its rows land under.
    pub derivation_key: String,
}

/// A derive task compiled into the embedding binary. Configuration names it; it never
/// names a command (`run.bind.host-task`).
pub trait DeriveTask: Send + Sync {
    /// The version its output derives under; raising it re-selects every settled unit.
    fn version(&self) -> &str;
    /// The parent-table columns a unit carries besides its key column.
    fn columns(&self) -> Vec<String>;
    /// The table its markers land in, the anti-join's side.
    fn marker_table(&self) -> String;
    /// The tables its content rows land in, in landing order.
    fn content_tables(&self) -> Vec<String>;
    /// Derive one unit into rows per content table; an error lands a `failed` marker.
    fn derive(&self, unit: &HostUnit) -> Result<Derived, RunError>;
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
    /// Register `task` under `name`; a built-in or already registered name refuses.
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
}

/// The outstanding host units of one tick: every parent row holding no settled marker under
/// its current key in the marker table, truncated to the run's row budget after the anti-join
/// (`run.select.anti-join`).
pub fn select_host(
    parents: &[Row],
    markers: &[Row],
    config: &DeriveConfig,
    task: &dyn DeriveTask,
) -> HostSelection {
    let units = by_unit(markers);
    let columns = task.columns();
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
        let derivation_key = host_key(config.task.name(), task.version(), &key, r, &columns);
        let prior = match units.get(&key) {
            None => 0,
            Some(rows) => match standing(rows, &derivation_key, config.max_attempts) {
                None => continue,
                Some(attempts) => attempts,
            },
        };
        sel.outstanding.push(HostUnit {
            key,
            row: r.clone(),
            prior_attempts: prior,
            derivation_key,
        });
    }
    sel.outstanding
        .truncate(usize::try_from(config.max_rows_per_run).unwrap_or(usize::MAX));
    sel
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
