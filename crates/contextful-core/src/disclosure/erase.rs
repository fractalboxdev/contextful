//! Refusals owned by the erasure contract; no row or subject material enters them.

use crate::store::declare::{DeclarationSet, ErasureSurvival, TableDecl};
use crate::store::reconcile::ColumnType;
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

/// Reconciled column types per table, as each table's stored schema records them.
pub type TableTypes = BTreeMap<String, BTreeMap<String, ColumnType>>;

/// The bare outermost partition column of each declaration that names one, keyed by
/// table. A declared type, else the reconciled type, other than text refuses; a table
/// with neither has landed no row and holds no tenant.
pub fn tenant_columns(declarations: &[TableDecl], types: &TableTypes) -> Result<BTreeMap<String, String>, ErasureError> {
    let mut columns = BTreeMap::new();
    for decl in declarations {
        let Some(column) = decl.partition_by().first().filter(|column| is_bare(column)) else { continue };
        let ty = decl.column_types().get(column).cloned().or_else(|| types.get(&decl.name).and_then(|table| table.get(column)).cloned());
        if ty.is_some_and(|ty| !matches!(ty, ColumnType::Utf8 | ColumnType::Null)) {
            return Err(ErasureError::PurgeTenantColumnType(decl.name.clone()));
        }
        columns.insert(decl.name.clone(), column.clone());
    }
    if columns.is_empty() { return Err(ErasureError::PurgeTenantUndeclared); }
    Ok(columns)
}

fn is_bare(column: &str) -> bool {
    !column.is_empty() && column.chars().all(|ch| ch.is_alphanumeric() || ch == '_')
}

/// Selects every row whose tenant column holds the identifier's exact bytes, with the
/// exclusively referenced descendants the ordinary cascade reaches.
pub fn select_tenant(
    declarations: &[TableDecl],
    rows: &RetainedRows,
    columns: &BTreeMap<String, String>,
    tenant: &str,
) -> Result<ErasureSelection, ErasureError> {
    if tenant.is_empty() { return Err(ErasureError::ErasureScopeUnsupported("a purge names no tenant identifier".into())); }
    let requested: Vec<String> = columns.keys().cloned().collect();
    select(declarations, rows, &requested, &|decl, row| {
        let column = columns.get(&decl.name).ok_or(ErasureError::PurgeTenantUndeclared)?;
        Ok(row.get(column).and_then(Value::as_str) == Some(tenant))
    })
}

/// Identifies a column named by canonical table declarations.
pub fn declares_erasure_column(declaration: &TableDecl, column: &str) -> bool {
    declaration.declared_column_names().contains(column)
}

/// Resolves scalar column predicates across every canonical declaring table.
pub fn column_key_set(declarations: &[TableDecl], column: &str, values: &[Value]) -> Result<RetainedRows, ErasureError> {
    column_key_set_declared(&DeclarationSet::from_tables(declarations), column, values)
}

/// Resolves a column across the complete canonical declaration pack, including name-only memory columns.
pub fn column_key_set_declared(declarations: &DeclarationSet, column: &str, values: &[Value]) -> Result<RetainedRows, ErasureError> {
    let unsupported = || ErasureError::ErasureScopeUnsupported("invalid declaration-scoped column key set".into());
    if column.is_empty() || values.is_empty() || values.iter().any(|value| !matches!(value, Value::String(_) | Value::Number(_) | Value::Bool(_))) {
        return Err(unsupported());
    }
    let keys: RetainedRows = declarations.declared_columns().iter().filter(|(_, columns)| columns.contains(column))
        .map(|(name, _)| (name.to_string(), values.iter().map(|value| Map::from_iter([(column.to_string(), value.clone())])).collect())).collect();
    if keys.is_empty() { return Err(unsupported()); }
    Ok(keys)
}

/// Selects column-scoped roots through the ordinary cascade and reference owner.
pub fn select_column_keys(declarations: &[TableDecl], rows: &RetainedRows, column: &str, values: &[Value]) -> Result<ErasureSelection, ErasureError> {
    select_column_keys_declared(&DeclarationSet::from_tables(declarations), rows, column, values)
}

/// Uses canonical column identities through the ordinary cascade and reference owner.
pub fn select_column_keys_declared(declarations: &DeclarationSet, rows: &RetainedRows, column: &str, values: &[Value]) -> Result<ErasureSelection, ErasureError> {
    select_key_rows(declarations.tables(), rows, &column_key_set_declared(declarations, column, values)?)
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
    #[error("PurgeRequiresOwner: {0}")]
    PurgeRequiresOwner(String),
    #[error("PurgeTenantColumnType: table `{0}` partitions outermost by a column that is not text")]
    PurgeTenantColumnType(String),
    #[error("PurgeTenantUndeclared: no table declares an outermost partition key")]
    PurgeTenantUndeclared,
    #[error("ReceiptIdentifierLeak: the receipt carries the tenant identifier in cleartext")]
    ReceiptIdentifierLeak,
    #[error("ReceiptClaimWidened: {0}")]
    ReceiptClaimWidened(String),
    #[error("ReceiptReplayed: the artifact repeats an earlier receipt byte for byte")]
    ReceiptReplayed,
}
