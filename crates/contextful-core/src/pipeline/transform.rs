//! `run.transform`: the declarative chain rewriting a batch in place.

use crate::run::ports::Row;
use crate::run::RunError;
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// One chain operation (`run.transform.chain`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "lowercase", deny_unknown_fields)]
pub enum TransformOp {
    /// Fix the outgoing column set by name.
    Select { columns: Vec<String> },
    /// Map an incoming name onto an outgoing one.
    Rename { from: String, to: String },
    /// Rewrite one column's type: `string`, `int64`, `float64` or `boolean`.
    Cast { column: String, to: String },
    /// Keep the rows whose single column equals a value.
    Filter { column: String, equals: Value },
}

/// The types a cast reaches.
pub const CAST_TYPES: [&str; 4] = ["string", "int64", "float64", "boolean"];

impl TransformOp {
    fn name(&self) -> &'static str {
        match self {
            TransformOp::Select { .. } => "select",
            TransformOp::Rename { .. } => "rename",
            TransformOp::Cast { .. } => "cast",
            TransformOp::Filter { .. } => "filter",
        }
    }

    /// Hold the operation's own declaration: a cast names a type it reaches.
    pub fn validate(&self) -> Result<(), RunError> {
        match self {
            TransformOp::Cast { to, column } if !CAST_TYPES.contains(&to.as_str()) => Err(RunError::Invalid(format!(
                "cast of `{column}` names type `{to}`; a cast reaches {}",
                CAST_TYPES.join(", ")
            ))),
            _ => Ok(()),
        }
    }
}

/// A value converted to `to`; a value with no reading in the type becomes null.
fn cast_value(v: &Value, to: &str) -> Value {
    match (to, v) {
        (_, Value::Null) => Value::Null,
        ("string", Value::String(_)) => v.clone(),
        ("string", other) => Value::String(other.to_string()),
        ("int64", Value::Number(n)) => n.as_i64().map(Value::from).or_else(|| n.as_f64().filter(|f| f.fract() == 0.0).map(|f| Value::from(f as i64))).unwrap_or(Value::Null),
        ("int64", Value::String(s)) => s.trim().parse::<i64>().map(Value::from).unwrap_or(Value::Null),
        ("int64", Value::Bool(b)) => Value::from(i64::from(*b)),
        ("float64", Value::Number(n)) => n.as_f64().map(Value::from).unwrap_or(Value::Null),
        ("float64", Value::String(s)) => s.trim().parse::<f64>().ok().filter(|f| f.is_finite()).map(Value::from).unwrap_or(Value::Null),
        ("boolean", Value::Bool(_)) => v.clone(),
        ("boolean", Value::String(s)) => match s.trim() {
            "true" => Value::Bool(true),
            "false" => Value::Bool(false),
            _ => Value::Null,
        },
        _ => Value::Null,
    }
}

fn require(rows: &[Row], column: &str, op: &TransformOp, table: &str) -> Result<(), RunError> {
    if !rows.is_empty() && !rows.iter().any(|r| r.contains_key(column)) {
        return Err(RunError::PipelineTransformColumnMissing(format!(
            "`{}` names column `{column}`, which table `{table}` does not carry in this batch",
            op.name()
        )));
    }
    Ok(())
}

/// Apply one operation to a batch.
pub fn apply_op(op: &TransformOp, rows: Vec<Row>, table: &str) -> Result<Vec<Row>, RunError> {
    Ok(match op {
        TransformOp::Select { columns } => rows.into_iter().map(|r| r.into_iter().filter(|(k, _)| columns.contains(k)).collect()).collect(),
        TransformOp::Rename { from, to } => rows
            .into_iter()
            .map(|mut r| {
                if let Some(v) = r.remove(from) {
                    r.insert(to.clone(), v);
                }
                r
            })
            .collect(),
        TransformOp::Cast { column, to } => {
            require(&rows, column, op, table)?;
            rows.into_iter()
                .map(|mut r| {
                    if let Some(v) = r.get_mut(column) {
                        *v = cast_value(v, to);
                    }
                    r
                })
                .collect()
        }
        TransformOp::Filter { column, equals } => {
            require(&rows, column, op, table)?;
            rows.into_iter().filter(|r| r.get(column) == Some(equals)).collect()
        }
    })
}

/// Run the chain in order, holding each operation to emit no more rows than it
/// consumed (`run.transform.arity`).
pub fn apply(chain: &[TransformOp], mut rows: Vec<Row>, table: &str) -> Result<Vec<Row>, RunError> {
    for op in chain {
        rows = checked(op, rows, table, apply_op)?;
    }
    Ok(rows)
}

/// Apply `f` for `op` and refuse an output larger than its input.
pub fn checked(
    op: &TransformOp,
    rows: Vec<Row>,
    table: &str,
    f: impl FnOnce(&TransformOp, Vec<Row>, &str) -> Result<Vec<Row>, RunError>,
) -> Result<Vec<Row>, RunError> {
    let consumed = rows.len();
    let out = f(op, rows, table)?;
    if out.len() > consumed {
        return Err(RunError::PipelineTransformArity(format!(
            "`{}` over table `{table}` emitted {} rows from {consumed}",
            op.name(),
            out.len()
        )));
    }
    Ok(out)
}

/// A pipeline's chain bound to one table, as the stage between the journal and the land path.
pub struct Chain {
    pub ops: Vec<TransformOp>,
    pub table: String,
}

impl crate::run::ports::Shape for Chain {
    fn shape(&self, rows: Vec<Row>) -> Result<Vec<Row>, RunError> {
        apply(&self.ops, rows, &self.table)
    }
}
