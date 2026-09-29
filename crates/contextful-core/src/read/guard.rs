//! `read.guard`: admission of caller-written SQL over the syntax tree the engine itself
//! serializes for the statement.
//!
//! The input is the engine's serialization of the text — `{"error": bool, "statements":
//! [...]}` — so the guard and the executor read one parse (`read.guard.engine-own-parse`).
//! The engine serializes only `SELECT` statements; anything else arrives as an error.

use super::error::{ReadError, Refusal};
use crate::enforce::EnforceError;
use serde_json::Value;
use std::collections::BTreeSet;

/// Schemas and catalogs whose relations are the engine's own catalog.
const SYSTEM_SCHEMAS: [&str; 4] = ["information_schema", "pg_catalog", "system", "temp"];

/// What an admitted statement reads.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Admitted {
    /// The registered relations the statement's base relations name.
    pub relations: BTreeSet<String>,
    /// Common table expressions the statement declares.
    pub ctes: BTreeSet<String>,
    /// Identifiers of the distinct placeholders the statement carries: `1`, `2`, … for
    /// positional ones, the name for `$name`.
    pub placeholders: BTreeSet<String>,
}

/// The one statement of a serialization, or `StatementNotReadOnly`
/// (`read.guard.single-read-only-statement`).
pub fn single_statement(serialized: &Value) -> Result<&Value, ReadError> {
    if serialized["error"].as_bool() != Some(false) {
        let why = serialized["error_message"].as_str().unwrap_or("the text is not a SELECT statement");
        return Err(ReadError::StatementNotReadOnly(why.to_string()));
    }
    match serialized["statements"].as_array().map(Vec::as_slice) {
        Some([one]) => Ok(one),
        Some(many) => Err(ReadError::StatementNotReadOnly(format!("the text holds {} statements; one is admitted", many.len()))),
        None => Err(ReadError::StatementNotReadOnly("the serialization carries no statement".into())),
    }
}

/// The CTE names an object declares for itself and every node beneath it.
fn declared(o: &serde_json::Map<String, Value>) -> impl Iterator<Item = &str> {
    o.get("cte_map").and_then(|m| m["map"].as_array()).into_iter().flatten().filter_map(|e| e["key"].as_str())
}

/// Unqualified names of the engine's own catalog relations.
fn catalog_name(name: &str) -> bool {
    let n = name.to_ascii_lowercase();
    ["sqlite_master", "sqlite_schema", "sqlite_temp_master", "sqlite_temp_schema", "information_schema"].contains(&n.as_str())
        || n.starts_with("duckdb_")
        || n.starts_with("pg_")
}

fn qualifier(o: &serde_json::Map<String, Value>) -> Option<String> {
    let parts: Vec<&str> =
        ["catalog_name", "schema_name"].iter().filter_map(|k| o.get(*k).and_then(Value::as_str)).filter(|s| !s.is_empty()).collect();
    (!parts.is_empty()).then(|| parts.join("."))
}

/// Admit one caller statement: exactly one read-only `SELECT`, no table function or
/// system-catalog reach anywhere in the tree, and every base relation a view registered
/// for this caller or a common table expression declared in a scope enclosing it. The
/// walk covers the whole tree, and a CTE name resolves only within the scope that
/// declares it (`read.guard.whole-tree-walk`). An unregistered relation is refused as
/// ungranted, echoing the statement's own spelling (`read.guard.unregistered-relation`).
pub fn admit(serialized: &Value, registered: impl Fn(&str) -> bool) -> Result<Admitted, Refusal> {
    let statement = single_statement(serialized)?;
    let mut admitted = Admitted::default();
    let mut placeholders = BTreeSet::new();
    walk(statement, &BTreeSet::new(), &registered, &mut admitted, &mut placeholders)?;
    admitted.placeholders = placeholders;
    Ok(admitted)
}

fn walk(
    v: &Value,
    scope: &BTreeSet<String>,
    registered: &impl Fn(&str) -> bool,
    admitted: &mut Admitted,
    parameters: &mut BTreeSet<String>,
) -> Result<(), Refusal> {
    match v {
        Value::Array(a) => a.iter().try_for_each(|c| walk(c, scope, registered, admitted, parameters)),
        Value::Object(o) => {
            let mut inner = scope.clone();
            for name in declared(o) {
                inner.insert(name.to_string());
                admitted.ctes.insert(name.to_string());
            }
            match o.get("type").and_then(Value::as_str) {
                Some("TABLE_FUNCTION") => {
                    let name = o.get("function").and_then(|f| f["function_name"].as_str()).unwrap_or("a table function");
                    return Err(ReadError::TableFunctionRefused(format!("`{name}` is a table function")).into());
                }
                Some("BASE_TABLE") => {
                    let name = o.get("table_name").and_then(Value::as_str).unwrap_or_default();
                    if let Some(q) = qualifier(o) {
                        if q.split('.').any(|part| SYSTEM_SCHEMAS.contains(&part.to_ascii_lowercase().as_str())) {
                            return Err(ReadError::TableFunctionRefused(format!("`{q}.{name}` reaches the engine's catalog")).into());
                        }
                        return Err(EnforceError::UnknownRelation(format!("`{q}.{name}`")).into());
                    }
                    if !scope.contains(name) {
                        if catalog_name(name) {
                            return Err(ReadError::TableFunctionRefused(format!("`{name}` is the engine's catalog")).into());
                        }
                        if !registered(name) {
                            return Err(EnforceError::UnknownRelation(format!("`{name}`")).into());
                        }
                        admitted.relations.insert(name.to_string());
                    }
                }
                _ => {}
            }
            if o.get("class").and_then(Value::as_str) == Some("PARAMETER") {
                parameters.insert(o.get("identifier").and_then(Value::as_str).unwrap_or_default().to_string());
            }
            o.values().try_for_each(|c| walk(c, &inner, registered, admitted, parameters))
        }
        _ => Ok(()),
    }
}
