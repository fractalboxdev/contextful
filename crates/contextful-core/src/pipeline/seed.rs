//! `run.seed`: the bulk-load block, the seeded-table declaration and the ordering-stamp
//! ceiling a seeded batch passes before it lands.
//!
//! The ceiling refuses rather than repairs. A stamp at or past `below` fails the whole
//! batch naming the value: clamping it fabricates event time, and dropping the row loses
//! history. A stamp that cannot be ordered against the ceiling fails too, because a
//! ceiling the gate cannot read is a ceiling it does not enforce.
//!
//! A text stamp lands as text and the store ranks it byte-wise, so the gate orders only
//! text whose byte order is its instant order: the `Z`-suffixed UTC spelling at the
//! ceiling's fractional width. An offset or another width reorders the column under the
//! store's `ORDER BY` while the instants still sort, and the gate refuses it.

use super::declare::{PipelineSpec, SourceBlock};
use crate::run::ports::Row;
use crate::run::RunError;
use crate::store::declare::FoldCoverage;
use crate::store::reserve::INGESTED_AT;
use crate::time::Instant;
use serde_json::{Number, Value};
use std::cmp::Ordering;

/// The ordering-stamp ceiling, on the seeded table's `order_by` scale.
#[derive(Debug, Clone, PartialEq)]
pub enum Ceiling {
    /// An RFC 3339 instant, for a text column of `Z`-suffixed UTC stamps carrying
    /// `fraction_digits` fractional digits, the width the declared ceiling spells.
    Instant { at: Instant, fraction_digits: usize },
    /// A number, for a numeric event clock.
    Number(Number),
}

impl Ceiling {
    /// Read a declared `below`: an RFC 3339 string or a number. Anything else is on no
    /// ordering scale and raises `PipelineSpecInvalid`.
    pub fn from_value(v: &Value) -> Result<Ceiling, RunError> {
        match v {
            Value::Number(n) => Ok(Ceiling::Number(n.clone())),
            Value::String(s) => Instant::parse(s).map(|at| Ceiling::Instant { at, fraction_digits: fraction_digits(s) }).map_err(|e| {
                RunError::PipelineSpecInvalid(format!("seed.below `{s}` is neither an RFC 3339 instant nor a number: {e}"))
            }),
            other => Err(RunError::PipelineSpecInvalid(format!(
                "seed.below `{other}` is neither an RFC 3339 instant nor a number"
            ))),
        }
    }

    fn render(&self) -> String {
        match self {
            Ceiling::Instant { at, .. } => at.to_rfc3339_nanos(),
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
}

const SEED_KEYS: [&str; 2] = ["source", "below"];

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
        Ok(SeedBlock { source, below })
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

/// Hold every table of a seeded pipeline to a scheduled, enabled fold covering it
/// (`run.seed.compaction-cadence`). Until a fold writes a snapshot, a key's seeded and live
/// rows both survive the union read.
pub fn check_compaction(spec: &PipelineSpec, coverage: &FoldCoverage) -> Result<(), RunError> {
    if spec.seed_block()?.is_none() {
        return Ok(());
    }
    for t in &spec.tables {
        let table = spec.table_name(t.name());
        if !coverage.covers(&table) {
            return Err(RunError::PipelineSeedCompactionMissing(format!(
                "pipeline `{}` seeds table `{table}` and no enabled `[[job]]` of kind `fold` with a `schedule` targets it",
                spec.id
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
                    "table `{table}`: `{}` stamp `{}` cannot be ordered against the ceiling `{}`; an RFC 3339 ceiling orders `Z`-suffixed UTC text at its own fractional width and a numeric ceiling orders numbers",
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

    /// The stamp's order against the ceiling, `None` when the two share no scale. Text
    /// orders only in the ceiling's spelling, where its byte order, the order the store
    /// ranks it by, is its instant order; `"999"` never orders against a number.
    fn compare(&self, stamp: &Value) -> Option<Ordering> {
        match (stamp, &self.below) {
            (Value::String(s), Ceiling::Instant { at, fraction_digits }) => {
                let v = Instant::parse(s).ok()?;
                is_utc_spelling(s, *fraction_digits).then(|| v.cmp(at))
            }
            (Value::Number(v), Ceiling::Number(c)) => compare_numbers(v, c),
            _ => None,
        }
    }
}

/// The fractional-second digits an RFC 3339 text spells, zero when it spells none.
fn fraction_digits(s: &str) -> usize {
    match s.as_bytes().get(19) {
        Some(b'.') => s.as_bytes()[20..].iter().take_while(|b| b.is_ascii_digit()).count(),
        _ => 0,
    }
}

/// `YYYY-MM-DDTHH:MM:SS[.f…]Z` with exactly `width` fractional digits: the one spelling
/// per instant in which byte order is instant order.
fn is_utc_spelling(s: &str, width: usize) -> bool {
    let b = s.as_bytes();
    let len = if width == 0 { 20 } else { 21 + width };
    b.len() == len
        && b[10] == b'T'
        && b[len - 1] == b'Z'
        && (width == 0 || (b[19] == b'.' && b[20..len - 1].iter().all(u8::is_ascii_digit)))
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
