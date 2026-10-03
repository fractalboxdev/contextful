//! `run.normalize`: the declared mode and depth, and the nested types `native` infers over
//! a batch of deferred-typing JSON for the store sink.

use crate::run::ports::{Row, Types};
use crate::run::RunError;
use crate::store::reconcile::{supertype, ColumnType, StructField};
use serde_json::Value;

/// Levels of nesting `native` keeps before a deeper subtree lands as one `Json` value
/// (`run.normalize.nesting-depth`).
pub const DEFAULT_NESTING_DEPTH: u32 = 5;

/// How a batch's nested values reach the sink (`run.normalize.mode`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Native,
    Relational,
}

impl Mode {
    pub const SPELLINGS: [&'static str; 2] = ["native", "relational"];

    fn parse(s: &str) -> Result<Mode, RunError> {
        match s {
            "native" => Ok(Mode::Native),
            "relational" => Ok(Mode::Relational),
            other => Err(RunError::PipelineNormalizeModeUnknown(format!(
                "normalize mode `{other}` is neither `{}` nor `{}`",
                Mode::SPELLINGS[0],
                Mode::SPELLINGS[1]
            ))),
        }
    }
}

/// A pipeline's `normalize` declaration: `"native"`, `"relational"`, or a table carrying
/// `mode` and `depth`. An absent declaration resolves to `native` at the default depth
/// (`run.normalize.mode-resolution`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Normalize {
    pub mode: Mode,
    pub depth: u32,
}

impl Default for Normalize {
    fn default() -> Normalize {
        Normalize {
            mode: Mode::Native,
            depth: DEFAULT_NESTING_DEPTH,
        }
    }
}

impl Normalize {
    pub fn parse(declared: Option<&Value>) -> Result<Normalize, RunError> {
        let malformed = |why: String| RunError::Invalid(format!("normalize: {why}"));
        Ok(match declared {
            None => Normalize::default(),
            Some(Value::String(mode)) => Normalize {
                mode: Mode::parse(mode)?,
                ..Normalize::default()
            },
            Some(Value::Object(o)) => {
                if let Some(key) = o.keys().find(|k| !["mode", "depth"].contains(&k.as_str())) {
                    return Err(malformed(format!("`{key}` is neither `mode` nor `depth`")));
                }
                let mode = match o.get("mode") {
                    None => Mode::Native,
                    Some(Value::String(m)) => Mode::parse(m)?,
                    Some(other) => return Err(malformed(format!("`mode` is {other}, not text"))),
                };
                let depth = match o.get("depth") {
                    None => DEFAULT_NESTING_DEPTH,
                    Some(d) => d
                        .as_u64()
                        .and_then(|d| u32::try_from(d).ok())
                        .ok_or_else(|| malformed(format!("`depth` is {d}, not a level count")))?,
                };
                Normalize { mode, depth }
            }
            Some(other) => return Err(malformed(format!("{other} is neither a mode nor a table"))),
        })
    }

    /// The nested types this declaration lands on the store sink for `rows`: under
    /// `native`, [`native_types`] at its depth; under `relational`, none.
    pub fn store_types(&self, rows: &[Row]) -> Types {
        match self.mode {
            Mode::Native => native_types(rows, self.depth),
            Mode::Relational => Types::new(),
        }
    }
}

/// The struct or list type of each column whose values include an object or an array,
/// inferred over every row (`run.normalize.native-store`). A container nested deeper than
/// `depth` levels types as `Json`; a column whose values meet no supertype, or a scalar
/// beside a container, is left out and lands as `Json` text.
pub fn native_types(rows: &[Row], depth: u32) -> Types {
    let mut seen: std::collections::BTreeMap<&str, Option<ColumnType>> =
        std::collections::BTreeMap::new();
    for row in rows {
        for (name, v) in row {
            let ty = infer(v, 1, depth);
            let slot = seen.entry(name.as_str()).or_insert(Some(ColumnType::Null));
            *slot = slot
                .as_ref()
                .and_then(|held| ty.and_then(|t| supertype(held, &t)));
        }
    }
    seen.into_iter()
        .filter_map(|(name, ty)| {
            ty.filter(ColumnType::is_nested)
                .map(|t| (name.to_string(), t))
        })
        .collect()
}

/// The type one JSON value carries at nesting `level`; `None` where its parts meet no
/// supertype. An empty object adds no field.
fn infer(v: &Value, level: u32, depth: u32) -> Option<ColumnType> {
    Some(match v {
        Value::Null => ColumnType::Null,
        Value::Bool(_) => ColumnType::Boolean,
        Value::Number(n) if n.is_i64() => ColumnType::Int64,
        Value::Number(_) => ColumnType::Float64,
        Value::String(_) => ColumnType::Utf8,
        Value::Array(_) | Value::Object(_) if level > depth => ColumnType::Json,
        Value::Array(items) => {
            let mut item = ColumnType::Null;
            for x in items {
                item = supertype(&item, &infer(x, level + 1, depth)?)?;
            }
            ColumnType::list(item)
        }
        Value::Object(o) if o.is_empty() => ColumnType::Null,
        Value::Object(o) => ColumnType::Struct(
            o.iter()
                .map(|(k, x)| Some(StructField::new(k, infer(x, level + 1, depth)?)))
                .collect::<Option<_>>()?,
        ),
    })
}
