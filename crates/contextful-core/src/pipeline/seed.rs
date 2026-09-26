//! `run.seed`: the bulk-load block, the seeded-table declaration and the ordering-stamp
//! ceiling a seeded batch passes before it lands.
//!
//! The ceiling refuses rather than repairs. A stamp at or past `below` fails the whole
//! batch naming the value: clamping it fabricates event time, and dropping the row loses
//! history. A stamp that cannot be ordered against the ceiling fails too, because a
//! ceiling the gate cannot read is a ceiling it does not enforce.

use super::declare::{PipelineSpec, SourceBlock};
use crate::run::ports::Row;
use crate::run::RunError;
use crate::store::reserve::INGESTED_AT;
use crate::time::Instant;
use serde_json::{Number, Value};
use std::cmp::Ordering;

/// The ordering-stamp ceiling, on the seeded table's `order_by` scale.
#[derive(Debug, Clone, PartialEq)]
pub enum Ceiling {
    /// An RFC 3339 instant, for a timestamp column.
    Instant(Instant),
    /// A number, for a numeric event clock.
    Number(Number),
}

impl Ceiling {
    /// Read a declared `below`: an RFC 3339 string or a number. Anything else is on no
    /// ordering scale and raises `PipelineSpecInvalid`.
    pub fn from_value(v: &Value) -> Result<Ceiling, RunError> {
        match v {
            Value::Number(n) => Ok(Ceiling::Number(n.clone())),
            Value::String(s) => Instant::parse(s).map(Ceiling::Instant).map_err(|e| {
                RunError::PipelineSpecInvalid(format!("seed.below `{s}` is neither an RFC 3339 instant nor a number: {e}"))
            }),
            other => Err(RunError::PipelineSpecInvalid(format!(
                "seed.below `{other}` is neither an RFC 3339 instant nor a number"
            ))),
        }
    }

    fn render(&self) -> String {
        match self {
            Ceiling::Instant(i) => i.to_rfc3339_nanos(),
            Ceiling::Number(n) => n.to_string(),
        }
    }
}

/// A `[pipeline.seed]` block (`run.seed.block`).
#[derive(Debug, Clone, PartialEq)]
pub struct SeedBlock {
    /// The bulk-load source, a connector name beside its configuration.
    pub source: SourceBlock,
    /// The ceiling every seeded stamp sorts strictly below.
    pub below: Ceiling,
    /// Chunk settings for the seed pull, in the `[pipeline.backfill]` shape.
    pub backfill: Option<Value>,
}

const SEED_KEYS: [&str; 3] = ["source", "below", "backfill"];

impl SeedBlock {
    /// Read a seed block, refusing a missing or unknown key with `PipelineSpecInvalid`.
    pub fn from_value(pipeline: &str, v: &Value) -> Result<SeedBlock, RunError> {
        let invalid = |what: String| RunError::PipelineSpecInvalid(format!("pipeline `{pipeline}` seed: {what}"));
        let obj = v.as_object().ok_or_else(|| invalid(format!("`{v}` is not a table")))?;
        if let Some(k) = obj.keys().find(|k| !SEED_KEYS.contains(&k.as_str())) {
            return Err(invalid(format!("unknown key `seed.{k}`; a seed block carries {}", SEED_KEYS.join(", "))));
        }
        let source = obj.get("source").ok_or_else(|| invalid("`seed.source` is required".into()))?;
        let source: SourceBlock =
            serde_json::from_value(source.clone()).map_err(|e| invalid(format!("`seed.source` is not a source block: {e}")))?;
        let below = obj.get("below").ok_or_else(|| invalid("`seed.below` is required".into()))?;
        let below = Ceiling::from_value(below).map_err(|e| match e {
            RunError::PipelineSpecInvalid(m) => invalid(m),
            other => other,
        })?;
        Ok(SeedBlock { source, below, backfill: obj.get("backfill").cloned() })
    }
}

impl PipelineSpec {
    /// The declared seed block, when `[pipeline.seed]` is present.
    pub fn seed_block(&self) -> Result<Option<SeedBlock>, RunError> {
        self.seed.as_ref().map(|v| SeedBlock::from_value(&self.id, v)).transpose()
    }
}

/// Hold every table of a seeded pipeline to a primary key and an event-time `order_by`
/// (`run.seed.declaration-missing`). The ingest stamp is transaction time: a seed loaded
/// after the live connector starts would outrank every live row on load order alone.
pub fn check_declaration(spec: &PipelineSpec) -> Result<(), RunError> {
    if spec.seed_block()?.is_none() {
        return Ok(());
    }
    for t in &spec.tables {
        let d = t.decl();
        let missing = if d.primary_key().is_empty() {
            Some("no `primary_key`")
        } else if d.order_by() == INGESTED_AT {
            Some("`order_by` at the ingest stamp `_ingested_at`")
        } else {
            None
        };
        if let Some(missing) = missing {
            return Err(RunError::PipelineSeedDeclarationMissing(format!(
                "pipeline `{}` seeds table `{}` with {missing}; a seeded table declares a key and an event-time ordering",
                spec.id, d.name
            )));
        }
    }
    Ok(())
}

/// The load-time gate on a seeded table: every row's `order_by` stamp sorts strictly
/// below the ceiling.
#[derive(Debug, Clone, PartialEq)]
pub struct SeedCeiling {
    pub order_by: String,
    pub below: Ceiling,
}

impl SeedCeiling {
    pub fn new(order_by: impl Into<String>, below: Ceiling) -> SeedCeiling {
        SeedCeiling { order_by: order_by.into(), below }
    }

    /// Hold every row of one batch to the ceiling, refusing the whole batch at the first
    /// row that breaches it (`run.seed.ceiling-breached`) or cannot be ordered against it
    /// (`run.seed.ceiling-unevaluable`). The rows are read, never rewritten.
    pub fn check(&self, table: &str, rows: &[Row]) -> Result<(), RunError> {
        for row in rows {
            let Some(stamp) = row.get(&self.order_by) else {
                return Err(RunError::PipelineSeedCeilingUnevaluable(format!(
                    "table `{table}`: a seeded row has no `{}` column to hold below `{}`",
                    self.order_by,
                    self.below.render()
                )));
            };
            let Some(order) = self.compare(stamp) else {
                return Err(RunError::PipelineSeedCeilingUnevaluable(format!(
                    "table `{table}`: `{}` stamp `{}` cannot be ordered against the ceiling `{}`; an RFC 3339 ceiling orders RFC 3339 text and a numeric ceiling orders numbers",
                    self.order_by,
                    render(stamp),
                    self.below.render()
                )));
            };
            if order != Ordering::Less {
                return Err(RunError::PipelineSeedCeilingBreached(format!(
                    "table `{table}`: `{}` stamp `{}` reaches the seed ceiling `{}`; the batch lands nothing",
                    self.order_by,
                    render(stamp),
                    self.below.render()
                )));
            }
        }
        Ok(())
    }

    /// The stamp's order against the ceiling, `None` when the two share no scale. Text is
    /// compared as instants, never as bytes: offsets break lexical order, and `"999"`
    /// sorts after `"1000"`.
    fn compare(&self, stamp: &Value) -> Option<Ordering> {
        match (stamp, &self.below) {
            (Value::String(s), Ceiling::Instant(c)) => Instant::parse(s).ok().map(|v| v.cmp(c)),
            (Value::Number(v), Ceiling::Number(c)) => compare_numbers(v, c),
            _ => None,
        }
    }
}

fn compare_numbers(a: &Number, b: &Number) -> Option<Ordering> {
    match (a.as_i64(), b.as_i64()) {
        (Some(a), Some(b)) => Some(a.cmp(&b)),
        _ => match (a.as_u64(), b.as_u64()) {
            (Some(a), Some(b)) => Some(a.cmp(&b)),
            _ => a.as_f64()?.partial_cmp(&b.as_f64()?),
        },
    }
}

fn render(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}
