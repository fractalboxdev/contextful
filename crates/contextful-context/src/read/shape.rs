//! Aggregate shape follows the embedded engine's parsed SELECT and function catalog.

use super::engine::SqlEngine;
use super::fault::ReadFault;
use serde_json::{Map, Value};
use std::collections::{BTreeMap, BTreeSet};

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

/// The first cohort table a SELECT of `tree` groups by a strict subset of its group key,
/// beside a key column the grouping omits (`disclosure.bound-cohort.cohort-widening`).
///
/// `keys` maps each cohort table to its released group key. A SELECT groups each cohort
/// table its `FROM` reaches, through subqueries and common table expressions, once per
/// grouping: each `GROUP BY` set, the non-aggregate select columns under `GROUP BY ALL`,
/// the empty set when it aggregates under neither, and each aggregate window's
/// `PARTITION BY` list. A grouping expression covers a key column only as a bare column
/// reference to it, directly or through a select alias or position.
pub(crate) fn cohort_widening(tree: &Value, keys: &BTreeMap<String, BTreeSet<String>>, aggregates: &BTreeSet<String>) -> Option<(String, String)> {
    let mut ctes: BTreeMap<String, Vec<&Value>> = BTreeMap::new();
    collect_ctes(tree, &mut ctes);
    let mut nodes = Vec::new();
    select_nodes(tree, &mut nodes);
    for node in nodes {
        let mut reached = BTreeSet::new();
        if let Some(from) = node.get("from_table") {
            reach(from, &ctes, &mut BTreeSet::new(), &mut reached);
        }
        let cohort: Vec<(&String, &BTreeSet<String>)> = keys.iter().filter(|(t, _)| reached.contains(*t)).collect();
        if cohort.is_empty() {
            continue;
        }
        for grouping in groupings(node, aggregates) {
            for (table, key) in &cohort {
                if let Some(missing) = key.iter().find(|c| grouping.as_ref().is_some_and(|g| !g.contains(*c))) {
                    return Some(((*table).clone(), missing.clone()));
                }
            }
        }
    }
    None
}

/// Every SELECT node of a statement, its subqueries and common table expressions included.
fn select_nodes<'a>(value: &'a Value, out: &mut Vec<&'a Map<String, Value>>) {
    match value {
        Value::Object(map) => {
            if map.get("type").and_then(Value::as_str) == Some("SELECT_NODE") {
                out.push(map);
            }
            map.values().for_each(|child| select_nodes(child, out));
        }
        Value::Array(items) => items.iter().for_each(|child| select_nodes(child, out)),
        _ => {}
    }
}

/// Each common table expression's name and bodies, from every `cte_map` of the statement.
fn collect_ctes<'a>(value: &'a Value, out: &mut BTreeMap<String, Vec<&'a Value>>) {
    match value {
        Value::Object(map) => {
            if let Some(entries) = map.get("cte_map").and_then(|m| m.get("map")).and_then(Value::as_array) {
                for entry in entries {
                    if let (Some(name), Some(body)) = (entry.get("key").and_then(Value::as_str), entry.get("value")) {
                        out.entry(name.to_string()).or_default().push(body);
                    }
                }
            }
            map.values().for_each(|child| collect_ctes(child, out));
        }
        Value::Array(items) => items.iter().for_each(|child| collect_ctes(child, out)),
        _ => {}
    }
}

/// Every base table a subtree names, the tables of each common table expression it names included.
fn reach(value: &Value, ctes: &BTreeMap<String, Vec<&Value>>, expanded: &mut BTreeSet<String>, out: &mut BTreeSet<String>) {
    match value {
        Value::Object(map) => {
            if map.get("type").and_then(Value::as_str) == Some("BASE_TABLE") {
                if let Some(name) = map.get("table_name").and_then(Value::as_str) {
                    out.insert(name.to_string());
                    if expanded.insert(name.to_string()) {
                        for body in ctes.get(name).into_iter().flatten() {
                            reach(body, ctes, expanded, out);
                        }
                    }
                }
            }
            map.values().for_each(|child| reach(child, ctes, expanded, out));
        }
        Value::Array(items) => items.iter().for_each(|child| reach(child, ctes, expanded, out)),
        _ => {}
    }
}

/// A SELECT's groupings as the key columns each covers; `None` covers every column.
fn groupings(node: &Map<String, Value>, aggregates: &BTreeSet<String>) -> Vec<Option<BTreeSet<String>>> {
    let list = |key: &str| -> &[Value] { node.get(key).and_then(Value::as_array).map_or(&[], Vec::as_slice) };
    let select = list("select_list");
    let expressions = list("group_expressions");
    let clauses: Vec<&Value> = ["select_list", "having", "qualify", "modifiers"].iter().filter_map(|k| node.get(*k)).collect();
    let indexes = |set: &Value| -> Vec<usize> { set.as_array().map_or(Vec::new(), |s| s.iter().filter_map(Value::as_u64).filter_map(|i| usize::try_from(i).ok()).collect()) };
    let sets: Vec<Vec<usize>> = list("group_sets").iter().map(indexes).collect();
    let mut out = Vec::new();
    if node.get("aggregate_handling").and_then(Value::as_str) == Some("FORCE_AGGREGATES") {
        if select.iter().any(|item| item.get("class").and_then(Value::as_str) == Some("STAR")) {
            out.push(None);
        } else {
            out.push(Some(select.iter().filter(|item| !calls_aggregate(item, aggregates)).filter_map(|item| covered(item, select)).collect()));
        }
    } else if !sets.is_empty() {
        for set in sets {
            out.push(Some(set.iter().filter_map(|i| expressions.get(*i)).filter_map(|e| covered(e, select)).collect()));
        }
    } else if !expressions.is_empty() {
        out.push(Some(expressions.iter().filter_map(|e| covered(e, select)).collect()));
    } else if node.get("having").is_some_and(|h| !h.is_null()) || clauses.iter().any(|c| calls_aggregate(c, aggregates)) {
        out.push(Some(BTreeSet::new()));
    }
    let mut windows = Vec::new();
    for clause in clauses {
        aggregate_windows(clause, &mut windows);
    }
    for window in windows {
        let partitions: &[Value] = window.get("partitions").and_then(Value::as_array).map_or(&[], Vec::as_slice);
        out.push(Some(partitions.iter().filter_map(|e| covered(e, select)).collect()));
    }
    out
}

/// The key column a grouping expression covers: a bare column reference, or a select alias
/// or 1-based select position naming one.
fn covered(expression: &Value, select: &[Value]) -> Option<String> {
    let column = |e: &Value| -> Option<String> {
        if e.get("class").and_then(Value::as_str) != Some("COLUMN_REF") {
            return None;
        }
        e.get("column_names")?.as_array()?.last()?.as_str().map(str::to_string)
    };
    match expression.get("class").and_then(Value::as_str) {
        Some("COLUMN_REF") => {
            let name = column(expression)?;
            let bare = expression.get("column_names").and_then(Value::as_array).is_some_and(|n| n.len() == 1);
            match select.iter().find(|item| bare && item.get("alias").and_then(Value::as_str) == Some(name.as_str())) {
                Some(aliased) => column(aliased),
                None => Some(name),
            }
        }
        Some("CONSTANT") => {
            let position = expression.get("value").and_then(|v| v.get("value")).and_then(Value::as_u64)?;
            column(select.get(usize::try_from(position).ok()?.checked_sub(1)?)?)
        }
        _ => None,
    }
}

/// Whether an expression calls an aggregate function outside a nested SELECT.
fn calls_aggregate(value: &Value, aggregates: &BTreeSet<String>) -> bool {
    match value {
        Value::Object(map) => {
            if map.get("type").and_then(Value::as_str) == Some("SELECT_NODE") {
                return false;
            }
            if map.get("class").and_then(Value::as_str) == Some("FUNCTION")
                && map.get("function_name").and_then(Value::as_str).is_some_and(|name| aggregates.contains(&name.to_ascii_lowercase()))
            {
                return true;
            }
            map.values().any(|child| calls_aggregate(child, aggregates))
        }
        Value::Array(items) => items.iter().any(|child| calls_aggregate(child, aggregates)),
        _ => false,
    }
}

/// Every aggregate window an expression holds outside a nested SELECT.
fn aggregate_windows<'a>(value: &'a Value, out: &mut Vec<&'a Map<String, Value>>) {
    match value {
        Value::Object(map) => {
            if map.get("type").and_then(Value::as_str) == Some("SELECT_NODE") {
                return;
            }
            if map.get("type").and_then(Value::as_str) == Some("WINDOW_AGGREGATE") {
                out.push(map);
            }
            map.values().for_each(|child| aggregate_windows(child, out));
        }
        Value::Array(items) => items.iter().for_each(|child| aggregate_windows(child, out)),
        _ => {}
    }
}
