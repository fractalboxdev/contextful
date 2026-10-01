//! `read.resolve-pin`: the `pin` argument every read tool admits, the
//! `contextful.resolved` entry a response carries per published model it touches, and the
//! cross-query comparison a consumer runs over those entries.

use crate::pipeline::model::{PublishSection, Watermark};
use serde::Serialize;
use serde_json::{Map, Value};
use std::collections::BTreeMap;

/// The argument name every read tool admits (`read.resolve-pin.pin-parameter`).
pub const PIN_ARGUMENT: &str = "pin";

/// The block name a response carries, without its `contextful.` prefix.
pub const RESOLVED_BLOCK: &str = "resolved";

/// A read's pin map: table name to the build identifier it resolves to. A table mapped to
/// `null` is held as unnamed (`read.resolve-pin.null-pin`), so `{t: null}` and `{}` are
/// one map, and one key of the session pool.
#[derive(Debug, Clone, Default, PartialEq, Eq, Hash)]
pub struct Pins(BTreeMap<String, String>);

impl Pins {
    /// Parse the `pin` argument: absent or `null` is no pin, otherwise an object mapping
    /// each table to a build identifier string or `null`.
    pub fn parse(value: Option<&Value>) -> Result<Pins, String> {
        let map = match value {
            None | Some(Value::Null) => return Ok(Pins::default()),
            Some(Value::Object(m)) => m,
            Some(other) => return Err(format!("`{PIN_ARGUMENT}` is an object of table to build identifier, not {other}")),
        };
        Pins::from_map(map)
    }

    fn from_map(map: &Map<String, Value>) -> Result<Pins, String> {
        let mut out = BTreeMap::new();
        for (table, build) in map {
            match build {
                Value::Null => {}
                Value::String(b) if !b.is_empty() => {
                    out.insert(table.clone(), b.clone());
                }
                other => return Err(format!("`{PIN_ARGUMENT}.{table}` is a build identifier string or null, not {other}")),
            }
        }
        Ok(Pins(out))
    }

    /// Pin `table` to `build`.
    pub fn with(mut self, table: &str, build: &str) -> Pins {
        self.0.insert(table.to_string(), build.to_string());
        self
    }

    /// The build `table` is pinned to, or `None` for an unnamed table.
    pub fn build(&self, table: &str) -> Option<&str> {
        self.0.get(table).map(String::as_str)
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// The map's canonical text, which joins the session pool's key
    /// (`read.cache.session-pool`).
    pub fn key(&self) -> String {
        serde_json::to_string(&self.0).expect("a pin map serializes")
    }
}

/// One published model's entry in `contextful.resolved`: the build a read resolved to and
/// its watermark (`read.resolve-pin.resolved-echo`).
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Resolved {
    pub build_id: String,
    /// `None` for a materialization carrying no watermark (`read.resolve-pin.absent-watermark`).
    pub watermark: Option<Watermark>,
}

impl Resolved {
    /// The one constructor of an entry: every face builds its echo here, from the publish
    /// section of the snapshot the read resolved to.
    pub fn of(section: &PublishSection) -> Resolved {
        // A build over no input records no frontier; an input frontier of zero runs is a
        // watermark, and serializes as one.
        let watermark = (section.watermark != Watermark::default()).then(|| section.watermark.clone());
        Resolved { build_id: section.build_id.clone(), watermark }
    }
}

/// The `contextful.resolved` block over every touched published model, or `None` where a
/// read touches none.
pub fn resolved_block(entries: &BTreeMap<String, Resolved>) -> Option<Value> {
    (!entries.is_empty()).then(|| serde_json::to_value(entries).expect("a resolved entry serializes"))
}

/// Two queries of one derivation that resolved one model to different builds.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Stitched {
    pub model: String,
    pub first: String,
    pub second: String,
}

/// The consumer's cross-query comparison (`read.resolve-pin.consumer-comparison`): the
/// first model any two of `responses` resolve to different builds, or `Ok` when every
/// model resolves to one build across all of them.
pub fn compare<'v>(responses: impl IntoIterator<Item = &'v Value>) -> Result<(), Stitched> {
    let mut seen: BTreeMap<String, String> = BTreeMap::new();
    for response in responses {
        let Some(Value::Object(block)) = response.get(format!("contextful.{RESOLVED_BLOCK}")) else { continue };
        for (model, entry) in block {
            let Some(build) = entry.get("build_id").and_then(Value::as_str) else { continue };
            match seen.get(model) {
                Some(first) if first != build => {
                    return Err(Stitched { model: model.clone(), first: first.clone(), second: build.to_string() });
                }
                Some(_) => {}
                None => {
                    seen.insert(model.clone(), build.to_string());
                }
            }
        }
    }
    Ok(())
}
