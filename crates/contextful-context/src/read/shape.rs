//! Aggregate shape follows the embedded engine's parsed SELECT and function catalog.

use super::engine::SqlEngine;
use super::fault::ReadFault;
use serde_json::Value;
use std::collections::BTreeSet;

/// A model statement is aggregate-shaped when a SELECT groups, deduplicates, has a
/// HAVING clause, or calls an aggregate function outside a window expression.
pub fn aggregate_shape(sql: &str) -> Result<bool, ReadFault> {
    let engine = SqlEngine::bare()?;
    let tree = engine.serialize(sql)?;
    if tree.get("error").and_then(Value::as_bool) == Some(true) {
        if SqlEngine::statement_count(sql).is_ok() {
            return Err(contextful_core::read::ReadError::StatementNotReadOnly("the model statement is not a read-only SELECT".into()).into());
        }
        return Err(ReadFault::Engine("the statement does not parse".into()));
    }
    contextful_core::read::guard::admit(&tree, |_| true)?;
    let names = engine.aggregate_functions()?;
    Ok(aggregate_node(&tree, &names))
}

fn aggregate_node(value: &Value, names: &BTreeSet<String>) -> bool {
    match value {
        Value::Object(map) => {
            if map.get("type").and_then(Value::as_str) == Some("SELECT_NODE") {
                for key in ["group_expressions", "group_sets"] {
                    if map.get(key).and_then(Value::as_array).is_some_and(|items| !items.is_empty()) {
                        return true;
                    }
                }
                if map.get("having").is_some_and(|having| !having.is_null()) {
                    return true;
                }
                if map.get("modifiers").and_then(Value::as_array).is_some_and(|items| items.iter().any(|item| item.get("type").and_then(Value::as_str) == Some("DISTINCT_MODIFIER"))) {
                    return true;
                }
            }
            if map.get("class").and_then(Value::as_str) == Some("FUNCTION") {
                if map.get("function_name").and_then(Value::as_str).is_some_and(|name| names.contains(&name.to_ascii_lowercase())) {
                    return true;
                }
            }
            map.values().any(|child| aggregate_node(child, names))
        }
        Value::Array(items) => items.iter().any(|child| aggregate_node(child, names)),
        _ => false,
    }
}
