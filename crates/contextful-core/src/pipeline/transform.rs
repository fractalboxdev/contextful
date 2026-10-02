//! `run.transform`: the declarative chain rewriting a batch in place.

use crate::run::ports::{Row, Types};
use crate::store::reconcile::{decode_binary, ColumnType};
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
    /// Rewrite one column's type: `string`, `int64`, `float64` or `boolean`, or a binary
    /// or vector type (`run.transform.typed-cast`).
    Cast { column: String, to: String },
    /// Keep the rows whose single column equals a value, or by whether a column holds a
    /// value at all (`run.transform.presence-filter`). A filter takes exactly one form.
    Filter {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        column: Option<String>,
        /// Present when declared, `null` included: an explicit `null` is a value to match.
        #[serde(default, deserialize_with = "declared", skip_serializing_if = "Option::is_none")]
        equals: Option<Value>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        absent: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        present: Option<String>,
    },
    /// Copy the value at an RFC 6901 pointer into a named column (`run.transform.extract`).
    Extract { pointer: String, to: String },
}

/// A declared key's value, `null` included.
fn declared<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Option<Value>, D::Error> {
    Value::deserialize(d).map(Some)
}

/// The value at RFC 6901 `pointer` inside `row`; `None` when it names nothing or is not a
/// pointer. The first token names a column, the rest descend into its value.
pub fn row_pointer<'a>(row: &'a Row, pointer: &str) -> Option<&'a Value> {
    let rest = pointer.strip_prefix('/')?;
    let (first, tail) = match rest.find('/') {
        Some(i) => (&rest[..i], &rest[i..]),
        None => (rest, ""),
    };
    let column = first.replace("~1", "/").replace("~0", "~");
    row.get(&column)?.pointer(tail)
}

/// The scalar types a cast reaches.
pub const CAST_TYPES: [&str; 4] = ["string", "int64", "float64", "boolean"];

/// The binary or vector type a cast names, spelled as a declaration spells it.
fn typed_cast(to: &str) -> Option<ColumnType> {
    ColumnType::parse(to).filter(|t| t.is_binary() || t.is_vector())
}

/// Whether `v` is the JSON form of type `ty`: padded base64 of the width, or a number
/// array of the dimension (`store.reconcile.typed-landing`).
fn reads_as(v: &Value, ty: ColumnType) -> bool {
    match ty {
        ColumnType::Binary => v.as_str().and_then(decode_binary).is_some(),
        ColumnType::FixedSizeBinary(n) => v.as_str().and_then(decode_binary).is_some_and(|b| b.len() == n as usize),
        ColumnType::FixedSizeList(_, n) => v.as_array().is_some_and(|a| a.len() == n as usize && a.iter().all(Value::is_number)),
        _ => false,
    }
}

impl TransformOp {
    fn name(&self) -> &'static str {
        match self {
            TransformOp::Select { .. } => "select",
            TransformOp::Rename { .. } => "rename",
            TransformOp::Cast { .. } => "cast",
            TransformOp::Filter { .. } => "filter",
            TransformOp::Extract { .. } => "extract",
        }
    }

    /// Hold the operation's own declaration: a cast names a type it reaches, a filter takes
    /// one form, and an extract names a pointer into the row.
    pub fn validate(&self) -> Result<(), RunError> {
        match self {
            TransformOp::Cast { to, column } if !CAST_TYPES.contains(&to.as_str()) && typed_cast(to).is_none() => Err(RunError::Invalid(format!(
                "cast of `{column}` names type `{to}`; a cast reaches {}, `binary`, `binary(n)`, `float32[n]` or `float16[n]`",
                CAST_TYPES.join(", ")
            ))),
            TransformOp::Filter { column, equals, absent, present } => {
                let forms = [column.is_some() && equals.is_some(), absent.is_some(), present.is_some()];
                let stray = column.is_some() != equals.is_some();
                if stray || forms.iter().filter(|f| **f).count() != 1 {
                    return Err(RunError::Invalid(
                        "a filter declares exactly one of `column` with `equals`, `absent` or `present`".to_string(),
                    ));
                }
                Ok(())
            }
            TransformOp::Extract { pointer, to } if !pointer.starts_with('/') => Err(RunError::Invalid(format!(
                "extract into `{to}` names pointer `{pointer}`; a pointer opens with `/` and its first token names a column"
            ))),
            _ => Ok(()),
        }
    }
}

/// A value converted to `to`; a value with no reading in the type becomes null.
fn cast_value(v: &Value, to: &str) -> Value {
    if let Some(ty) = typed_cast(to) {
        return if reads_as(v, ty) { v.clone() } else { Value::Null };
    }
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

/// Whether `row` holds a value in `column`: the key is present and not `null`.
fn holds(row: &Row, column: &str) -> bool {
    row.get(column).is_some_and(|v| !v.is_null())
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
    // A filter's form and an extract's pointer decide which rows and values it reads.
    if matches!(op, TransformOp::Filter { .. } | TransformOp::Extract { .. }) {
        op.validate()?;
    }
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
        TransformOp::Filter { column: Some(column), equals: Some(equals), .. } => {
            require(&rows, column, op, table)?;
            rows.into_iter().filter(|r| r.get(column) == Some(equals)).collect()
        }
        TransformOp::Filter { absent: Some(column), .. } => rows.into_iter().filter(|r| !holds(r, column)).collect(),
        TransformOp::Filter { present: Some(column), .. } => rows.into_iter().filter(|r| holds(r, column)).collect(),
        // Unreachable past the form check above, which admits exactly one form.
        TransformOp::Filter { .. } => rows,
        TransformOp::Extract { pointer, to } => rows
            .into_iter()
            .map(|mut r| {
                    let v = row_pointer(&r, pointer).cloned().unwrap_or(Value::Null);
                    r.insert(to.clone(), v);
                    r
                })
                .collect(),
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

/// The column types after `chain`: a rename moves a type to the new name, a select drops
/// it with its column, and a cast sets it or, to a scalar type, drops it
/// (`run.transform.type-carry`).
pub fn carry_types(chain: &[TransformOp], mut types: Types) -> Types {
    for op in chain {
        match op {
            TransformOp::Select { columns } => types.retain(|c, _| columns.contains(c)),
            TransformOp::Rename { from, to } => {
                if let Some(t) = types.remove(from) {
                    types.insert(to.clone(), t);
                }
            }
            TransformOp::Cast { column, to } => match typed_cast(to) {
                Some(t) => {
                    types.insert(column.clone(), t);
                }
                None => {
                    types.remove(column);
                }
            },
            TransformOp::Filter { .. } => {}
            TransformOp::Extract { to, .. } => {
                types.remove(to);
            }
        }
    }
    types
}

impl crate::run::ports::Shape for Chain {
    fn shape(&self, rows: Vec<Row>) -> Result<Vec<Row>, RunError> {
        apply(&self.ops, rows, &self.table)
    }

    fn shape_types(&self, types: Types) -> Types {
        carry_types(&self.ops, types)
    }
}
