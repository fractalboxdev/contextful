//! `authority.refuse`: the scope guard over the engine's parse of a statement, deciding
//! literal tenant equalities and membership lists against the granted scope.

use super::session::Session;
use contextful_core::enforce::EnforceError;
use contextful_core::read::template::{Bindings, Bound};
use serde_json::Value;

/// Parse-tree nodes the scope guard visits (`authority.refuse.guard-walk`).
pub const SCOPE_GUARD_WALK: usize = 4096;

/// A value the guard settled for the tenant column.
fn constant(v: &Value) -> Option<String> {
    let value = &v["value"];
    if value["is_null"].as_bool() == Some(true) {
        return None;
    }
    match &value["value"] {
        Value::String(s) => Some(s.clone()),
        Value::Null => None,
        other => Some(other.to_string()),
    }
}

fn bound(v: &Value, parameters: &Bindings) -> Option<String> {
    match parameters.get(v["identifier"].as_str()?)? {
        Bound::Text(s) => Some(s.clone()),
        Bound::Integer(n) => Some(n.to_string()),
        _ => None,
    }
}

/// The value one operand settles: a literal, or a value bound to a template or query
/// placeholder destined for the tenant column.
fn settled(v: &Value, parameters: &Bindings) -> Option<String> {
    match v["class"].as_str() {
        Some("CONSTANT") => constant(v),
        Some("PARAMETER") => bound(v, parameters),
        _ => None,
    }
}

/// Whether a column reference names `column`, bare or qualified by the table's own name
/// or alias.
fn names_column(v: &Value, column: &str, qualifiers: &[&str]) -> bool {
    if v["class"].as_str() != Some("COLUMN_REF") {
        return false;
    }
    match v["column_names"].as_array().map(Vec::as_slice) {
        Some([c]) => c.as_str() == Some(column),
        Some([q, c]) => c.as_str() == Some(column) && q.as_str().is_some_and(|q| qualifiers.contains(&q)),
        _ => false,
    }
}

/// The requested values one conjunct settles for the tenant column, or `None` where the
/// guard cannot settle it.
fn requested(conjunct: &Value, column: &str, qualifiers: &[&str], parameters: &Bindings) -> Option<Vec<String>> {
    match conjunct["type"].as_str()? {
        "COMPARE_EQUAL" => {
            let (l, r) = (&conjunct["left"], &conjunct["right"]);
            if names_column(l, column, qualifiers) {
                settled(r, parameters).map(|v| vec![v])
            } else if names_column(r, column, qualifiers) {
                settled(l, parameters).map(|v| vec![v])
            } else {
                None
            }
        }
        "COMPARE_IN" => {
            let children = conjunct["children"].as_array()?;
            let (head, list) = children.split_first()?;
            if !names_column(head, column, qualifiers) {
                return None;
            }
            list.iter().map(|v| settled(v, parameters)).collect()
        }
        _ => None,
    }
}

fn nodes(v: &Value) -> usize {
    match v {
        Value::Object(o) => 1 + o.values().map(nodes).sum::<usize>(),
        Value::Array(a) => a.iter().map(nodes).sum(),
        _ => 0,
    }
}

/// Refuse a statement whose own `FROM` names a tenant-scoped table and whose top-level
/// conjuncts ask for a tenant outside the grant (`authority.refuse.scope-guard`,
/// `authority.refuse.top-level`). The refusal names the table, the granted scope and the
/// statement's own requested value, never a stored one (`authority.refuse.echo`). A
/// constraint the guard cannot settle, or a tree past the walk bound, passes as an
/// ordinary conjunct and the engine-applied equality isolates
/// (`authority.refuse.undecidable`).
pub fn guard(statement: &Value, session: &Session, parameters: &Bindings) -> Result<(), EnforceError> {
    let Some(node) = statement["statements"].as_array().and_then(|s| s.first()).map(|s| &s["node"]) else {
        return Ok(());
    };
    if nodes(node) > SCOPE_GUARD_WALK {
        return Ok(());
    }
    let from = &node["from_table"];
    if from["type"].as_str() != Some("BASE_TABLE") {
        return Ok(());
    }
    let table = from["table_name"].as_str().unwrap_or_default();
    let Some((column, granted)) = session.tenant_scopes().get(table) else { return Ok(()) };
    let alias = from["alias"].as_str().unwrap_or_default();
    let qualifiers: Vec<&str> = [table, alias].into_iter().filter(|q| !q.is_empty()).collect();
    let mut conjuncts = vec![&node["where_clause"]];
    let mut top = Vec::new();
    while let Some(c) = conjuncts.pop() {
        if c["type"].as_str() == Some("CONJUNCTION_AND") {
            conjuncts.extend(c["children"].as_array().into_iter().flatten());
        } else if !c.is_null() {
            top.push(c);
        }
    }
    for c in top {
        if let Some(values) = requested(c, column, &qualifiers, parameters) {
            if let Some(outside) = values.iter().find(|v| !granted.contains(v)) {
                return Err(EnforceError::ScopeDenied(format!(
                    "table `{table}`: granted scope `{}`, requested scope `{outside}`",
                    granted.join("`, `")
                )));
            }
        }
    }
    Ok(())
}
