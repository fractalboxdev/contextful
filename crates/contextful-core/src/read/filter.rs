//! A ranked read's caller filter: equality and membership conditions over named
//! columns, `kinds` joined as one membership on `kind`, and the budget checked over the
//! whole filter before any arm is built (`read.retrieve.filter-budget`,
//! `read.retrieve.filter-budget-refusal`).

use super::ReadError;
use serde_json::{Map, Value};

/// Most conditions one filter holds, `kinds` counting as one (`read.retrieve.filter-budget`).
pub const FILTER_CONDITIONS: usize = 32;

/// Most values one membership list holds (`read.retrieve.filter-budget`).
pub const FILTER_VALUES: usize = 256;

/// Most bytes of serialized JSON the filter and `kinds` hold together (`read.retrieve.filter-budget`).
pub const FILTER_BYTES: usize = 16 * 1024;

/// The column `kinds` binds (`read.retrieve.kinds`).
pub const KIND_COLUMN: &str = "kind";

/// One filter value (`read.retrieve.filter-values`).
#[derive(Debug, Clone, PartialEq)]
pub enum Scalar {
    Text(String),
    Integer(i64),
    Float(f64),
    Boolean(bool),
}

/// One condition: the column equals the one value, or is a member of the values.
#[derive(Debug, Clone, PartialEq)]
pub struct Condition {
    pub column: String,
    pub values: Vec<Scalar>,
}

/// Every condition a ranked read's rows meet, in column order, `kinds` last.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Filter {
    pub conditions: Vec<Condition>,
}

fn refuse(message: impl Into<String>) -> ReadError {
    ReadError::FilterBudgetExceeded(message.into())
}

fn scalar(column: &str, v: &Value) -> Result<Scalar, ReadError> {
    match v {
        Value::String(s) => Ok(Scalar::Text(s.clone())),
        Value::Bool(b) => Ok(Scalar::Boolean(*b)),
        Value::Number(n) => Ok(n.as_i64().map_or_else(|| Scalar::Float(n.as_f64().unwrap_or(f64::NAN)), Scalar::Integer)),
        other => Err(refuse(format!("`{column}` holds {other}, not a string, number or boolean"))),
    }
}

fn membership(column: &str, values: &[Value]) -> Result<Vec<Scalar>, ReadError> {
    if values.is_empty() {
        return Err(refuse(format!("`{column}` holds an empty list")));
    }
    if values.len() > FILTER_VALUES {
        return Err(refuse(format!("`{column}` lists {} values, over {FILTER_VALUES}", values.len())));
    }
    values.iter().map(|v| scalar(column, v)).collect()
}

impl Filter {
    /// Parse a caller's `filter` object and `kinds` list, refusing the whole filter when
    /// it breaks its budget or holds a malformed condition.
    pub fn parse(filter: Option<&Value>, kinds: Option<&[String]>) -> Result<Filter, ReadError> {
        let empty = Map::new();
        let object = match filter {
            None | Some(Value::Null) => &empty,
            Some(Value::Object(m)) => m,
            Some(other) => return Err(refuse(format!("the filter is {other}, not an object"))),
        };
        let bytes = filter.map_or(0, |f| f.to_string().len())
            + kinds.map_or(0, |k| serde_json::to_string(k).map_or(usize::MAX, |s| s.len()));
        if bytes > FILTER_BYTES {
            return Err(refuse(format!("the filter serializes to {bytes} bytes, over {FILTER_BYTES}")));
        }
        let count = object.len() + usize::from(kinds.is_some());
        if count > FILTER_CONDITIONS {
            return Err(refuse(format!("the filter holds {count} conditions, over {FILTER_CONDITIONS}")));
        }
        let mut conditions = Vec::with_capacity(count);
        for (column, v) in object {
            if column.is_empty() {
                return Err(refuse("a condition names an empty column"));
            }
            let values = match v {
                Value::Array(list) => membership(column, list)?,
                one => vec![scalar(column, one)?],
            };
            conditions.push(Condition { column: column.clone(), values });
        }
        if let Some(kinds) = kinds {
            let list: Vec<Value> = kinds.iter().cloned().map(Value::String).collect();
            conditions.push(Condition { column: KIND_COLUMN.to_string(), values: membership("kinds", &list)? });
        }
        Ok(Filter { conditions })
    }

    /// The columns the filter names, each a column a table's arm must carry
    /// (`read.retrieve.unsatisfiable-arm-drops`).
    pub fn columns(&self) -> impl Iterator<Item = &str> {
        self.conditions.iter().map(|c| c.column.as_str())
    }

    pub fn is_empty(&self) -> bool {
        self.conditions.is_empty()
    }
}
