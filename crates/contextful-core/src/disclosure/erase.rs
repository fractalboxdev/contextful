//! Refusals owned by the erasure contract; no row or subject material enters them.

use crate::store::declare::{ErasureSurvival, TableDecl};
use serde_json::{Map, Value};
use std::collections::BTreeMap;

// mirrors: disclosure.erase.cascade
const CASCADE_HOPS: usize = 16;

/// Every retained row version in the declared reference universe.
pub type RetainedRows = BTreeMap<String, Vec<Map<String, Value>>>;

/// Row positions selected for removal; selectors and row values remain absent.
#[derive(Debug)]
pub struct ErasureSelection {
    removed: BTreeMap<String, Vec<bool>>,
}

impl ErasureSelection {
    pub fn removes(&self, table: &str) -> &[bool] {
        self.removed.get(table).map(Vec::as_slice).unwrap_or(&[])
    }

    pub fn affected_tables(&self) -> impl Iterator<Item = &str> {
        self.removed.iter().filter(|(_, mask)| mask.iter().any(|v| *v)).map(|(name, _)| name.as_str())
    }
}

/// Selects subject rows and exclusively referenced descendants across retained versions.
pub fn select_subject(
    declarations: &[TableDecl],
    rows: &RetainedRows,
    requested: &[String],
    subject: &str,
) -> Result<ErasureSelection, ErasureError> {
    for table in requested {
        if declarations.iter().find(|decl| decl.name == *table).is_some_and(|decl| decl.subject_id.is_none()) {
            return Err(ErasureError::ErasureSubjectUndeclared(table.clone()));
        }
    }
    select(declarations, rows, requested, &|decl, row| {
        let column = decl.subject_id.as_ref().ok_or_else(|| ErasureError::ErasureSubjectUndeclared(decl.name.clone()))?;
        let value = row.get(column).ok_or_else(|| ErasureError::ErasureScopeUnsupported("incomplete erasure subject column".into()))?;
        Ok(value.as_str() == Some(subject))
    })
}

/// Identifies a column named by canonical table declarations.
pub fn declares_erasure_column(declaration: &TableDecl, column: &str) -> bool {
    declaration.columns.as_ref().is_some_and(|columns| columns.contains_key(column))
        || declaration.erasure_key.as_deref() == Some(column)
        || declaration.primary_key.as_ref().is_some_and(|columns| columns.iter().any(|key| key == column))
}

/// Resolves scalar column predicates across every canonical declaring table.
pub fn column_key_set(declarations: &[TableDecl], column: &str, values: &[Value]) -> Result<RetainedRows, ErasureError> {
    let unsupported = || ErasureError::ErasureScopeUnsupported("invalid declaration-scoped column key set".into());
    if column.is_empty() || values.is_empty() || values.iter().any(|value| !matches!(value, Value::String(_) | Value::Number(_) | Value::Bool(_))) {
        return Err(unsupported());
    }
    let keys: RetainedRows = declarations.iter().filter(|decl| declares_erasure_column(decl, column))
        .map(|decl| (decl.name.clone(), values.iter().map(|value| Map::from_iter([(column.to_string(), value.clone())])).collect())).collect();
    if keys.is_empty() { return Err(unsupported()); }
    Ok(keys)
}

/// Selects column-scoped roots through the ordinary cascade and reference owner.
pub fn select_column_keys(declarations: &[TableDecl], rows: &RetainedRows, column: &str, values: &[Value]) -> Result<ErasureSelection, ErasureError> {
    select_key_rows(declarations, rows, &column_key_set(declarations, column, values)?)
}

/// Selects complete declared primary keys or an explicit scalar erasure key.
pub fn select_keys(
    declarations: &[TableDecl],
    rows: &RetainedRows,
    keys: &BTreeMap<String, Vec<Map<String, Value>>>,
) -> Result<ErasureSelection, ErasureError> {
    let unsupported = || ErasureError::ErasureScopeUnsupported("incomplete declared erasure key set".into());
    for (table, selectors) in keys {
        let decl = declarations.iter().find(|decl| decl.name == *table).ok_or_else(unsupported)?;
        if selectors.is_empty() { return Err(unsupported()); }
        for selector in selectors {
            let primary = decl.primary_key.as_ref().is_some_and(|keys| !keys.is_empty() && selector.len() == keys.len() && keys.iter().all(|key| selector.contains_key(key)));
            let erasure = decl.erasure_key.as_ref().is_some_and(|key| selector.len() == 1 && selector.contains_key(key));
            if !(primary || erasure) || selector.values().any(Value::is_null) { return Err(unsupported()); }
        }
    }
    select_key_rows(declarations, rows, keys)
}

fn select_key_rows(declarations: &[TableDecl], rows: &RetainedRows, keys: &RetainedRows) -> Result<ErasureSelection, ErasureError> {
    let unsupported = || ErasureError::ErasureScopeUnsupported("incomplete declared erasure key set".into());
    select(declarations, rows, &keys.keys().cloned().collect::<Vec<_>>(), &|decl, row| {
        let selectors = keys.get(&decl.name).ok_or_else(unsupported)?;
        for selector in selectors {
            if selector.keys().any(|key| !row.contains_key(key)) { return Err(unsupported()); }
        }
        Ok(selectors.iter().any(|selector| selector.iter().all(|(key, value)| row.get(key) == Some(value))))
    })
}

fn select(
    declarations: &[TableDecl],
    rows: &RetainedRows,
    requested: &[String],
    choose: &impl Fn(&TableDecl, &Map<String, Value>) -> Result<bool, ErasureError>,
) -> Result<ErasureSelection, ErasureError> {
    let unsupported = || ErasureError::ErasureScopeUnsupported("incomplete erasure reference universe".into());
    let declared: BTreeMap<_, _> = declarations.iter().map(|decl| (decl.name.as_str(), decl)).collect();
    if declared.len() != declarations.len() || requested.is_empty() {
        return Err(unsupported());
    }
    let mut removed = BTreeMap::new();
    for decl in declarations {
        let retained = rows.get(&decl.name).ok_or_else(unsupported)?;
        removed.insert(decl.name.clone(), vec![false; retained.len()]);
        if let Some(references) = &decl.referenced_by {
            let key = decl.erasure_key.as_ref().ok_or_else(unsupported)?;
            if retained.iter().any(|row| row.get(key).is_none_or(Value::is_null)) {
                return Err(unsupported());
            }
            for reference in references {
                if !declared.contains_key(reference.table.as_str()) {
                    return Err(unsupported());
                }
                let sources = rows.get(&reference.table).ok_or_else(unsupported)?;
                if sources.iter().any(|row| !row.contains_key(&reference.column)) {
                    return Err(unsupported());
                }
            }
        }
    }
    for table in requested {
        let decl = declared.get(table.as_str()).ok_or_else(unsupported)?;
        if decl.on_erase == Some(ErasureSurvival::Survive) {
            continue;
        }
        for (mask, row) in removed.get_mut(table).ok_or_else(unsupported)?.iter_mut().zip(&rows[table]) {
            *mask = choose(decl, row)?;
        }
    }
    let mut hops = 0;
    loop {
        let mut additions = Vec::new();
        for decl in declarations {
            if decl.on_erase == Some(ErasureSurvival::Survive) {
                continue;
            }
            let Some(references) = &decl.referenced_by else { continue };
            let key = decl.erasure_key.as_ref().ok_or_else(unsupported)?;
            for (index, row) in rows[&decl.name].iter().enumerate() {
                if removed[&decl.name][index] {
                    continue;
                }
                let mut erased_reference = false;
                let mut surviving_reference = false;
                for reference in references {
                    for (position, source) in rows[&reference.table].iter().enumerate() {
                        if source.get(&reference.column) == row.get(key) {
                            if removed[&reference.table][position] {
                                erased_reference = true;
                            } else {
                                surviving_reference = true;
                            }
                        }
                    }
                }
                if erased_reference && !surviving_reference {
                    additions.push((decl.name.clone(), index));
                }
            }
        }
        if additions.is_empty() {
            break;
        }
        if hops == CASCADE_HOPS {
            return Err(ErasureError::ErasureCascadeUnbounded);
        }
        hops += 1;
        for (table, index) in additions {
            removed.get_mut(&table).ok_or_else(unsupported)?[index] = true;
        }
    }
    Ok(ErasureSelection { removed })
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ErasureError {
    #[error("ErasureUngranted: Forget authority does not cover requested table `{0}`")]
    ErasureUngranted(String),
    #[error("ErasureScopeUnsupported: {0}")]
    ErasureScopeUnsupported(String),
    #[error("ErasureTransactionIncomplete: {0}")]
    ErasureTransactionIncomplete(String),
    #[error("ErasureSubjectUndeclared: table `{0}` declares no subject column")]
    ErasureSubjectUndeclared(String),
    #[error("ErasureCascadeUnbounded: the reference cascade exceeds its declared bound")]
    ErasureCascadeUnbounded,
}
