//! `run.select.available-select`: a derive run bounded by `available_as_of` selects the
//! parent rows the bound admits, and each derived row lands its parent's availability instant.

use super::super::ports::Row;
use crate::store::bound_time::Bound;
use crate::time::Instant;
use serde_json::Value;
use std::collections::BTreeMap;

/// A derive run over a parent declaring `available_time`, and the bound it carries.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AvailableSelect {
    /// The run's `available_as_of`; absent, every parent is selected and still stamps its rows.
    pub bound: Option<Bound>,
    /// The parent table's declared availability column.
    pub parent_column: String,
    /// Each output table declaring an availability column, by store table name.
    pub outputs: BTreeMap<String, String>,
}

/// Each admitted parent's availability instant, by unit key.
pub type Instants = BTreeMap<String, Value>;

impl AvailableSelect {
    /// The parents whose availability instant the bound admits, as
    /// `store.available-time.available-as-of` admits them; under a bound, a parent with no
    /// readable instant is admitted by none. Each admitted parent's instant joins `instants`
    /// under its id.
    pub fn admit(&self, parents: Vec<Row>, id_column: &str, instants: &mut Instants) -> Vec<Row> {
        parents
            .into_iter()
            .filter(|parent| {
                let value = parent.get(&self.parent_column).filter(|v| !v.is_null());
                let at = value.and_then(Value::as_str).and_then(|s| Instant::parse(s).ok());
                let admitted = match (self.bound, at) {
                    (None, _) => true,
                    (Some(bound), Some(at)) => bound.admits(at),
                    (Some(_), None) => false,
                };
                if let (true, Some(value), Some(id)) = (admitted, value, unit_key(parent.get(id_column))) {
                    instants.insert(id, value.clone());
                }
                admitted
            })
            .collect()
    }

    /// Land each row of `table` with its parent's availability instant, joined on `unit_ref`.
    pub fn stamp(&self, table: &str, rows: &mut [Row], instants: &Instants) {
        let Some(column) = self.outputs.get(table) else { return };
        for row in rows {
            if let Some(at) = unit_key(row.get("unit_ref")).and_then(|unit| instants.get(&unit)) {
                row.insert(column.clone(), at.clone());
            }
        }
    }
}

/// A parent id or `unit_ref` as the selection keys units.
fn unit_key(v: Option<&Value>) -> Option<String> {
    match v? {
        Value::String(s) if !s.trim().is_empty() => Some(s.clone()),
        Value::Number(n) => Some(n.to_string()),
        _ => None,
    }
}
