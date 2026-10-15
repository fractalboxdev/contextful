//! The `receive` source: a pipeline whose rows a sender delivers rather than the engine
//! pulls (`connector.source.receive-form`). A delivered batch splits into each declared
//! table's rows, and each table's run takes them as its single pull
//! (`run.land.receive-entry`).

use contextful_core::run::ports::{Cancellation, PullRequest, Row, Source};
use contextful_core::run::{Failure, FailureTag, RunError};
use serde_json::Value;
use std::collections::BTreeMap;

pub const NAME: &str = "receive";
/// The one body format a `receive` source decodes.
pub const FORMAT: &str = "jsonl";
/// The retry interval a backpressured sender waits (`run.land.receive-retry-after`).
pub const RETRY_AFTER_MS: u64 = 1_000;
const KEYS: [&str; 3] = ["format", "max_batch_bytes", "max_queue_depth"];

/// A `receive` source's declared ceilings; neither carries a default.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReceiveConfig {
    /// Largest body one batch carries (`run.land.receive-batch-too-large`).
    pub max_batch_bytes: u64,
    /// Batches of the pipeline holding a queue slot at once (`run.land.receive-queue`).
    pub max_queue_depth: u32,
}

impl ReceiveConfig {
    pub fn parse(value: &Value) -> Result<ReceiveConfig, RunError> {
        let object = value.as_object().ok_or_else(|| RunError::Invalid(format!("`{NAME}` config is an object")))?;
        if let Some(k) = object.keys().find(|k| !KEYS.contains(&k.as_str())) {
            return Err(RunError::PipelineUnknownConfigKey(format!("the `{NAME}` source reads no key `{k}`; it reads {}", KEYS.join(", "))));
        }
        match object.get("format").and_then(Value::as_str) {
            Some(FORMAT) => {}
            Some(other) => return Err(RunError::Invalid(format!("the `{NAME}` source decodes `format = \"{FORMAT}\"`, not `{other}`"))),
            None => return Err(RunError::Invalid(format!("the `{NAME}` source declares `format = \"{FORMAT}\"`"))),
        }
        let positive = |key: &str| -> Result<u64, RunError> {
            object
                .get(key)
                .and_then(Value::as_u64)
                .filter(|n| *n > 0)
                .ok_or_else(|| RunError::Invalid(format!("the `{NAME}` source declares a positive integer `{key}`; it has no default")))
        };
        let max_batch_bytes = positive("max_batch_bytes")?;
        let max_queue_depth = u32::try_from(positive("max_queue_depth")?)
            .map_err(|_| RunError::Invalid(format!("the `{NAME}` source's `max_queue_depth` fits in 32 bits")))?;
        Ok(ReceiveConfig { max_batch_bytes, max_queue_depth })
    }
}

/// Split a `jsonl` body into each declared table's rows, in body order. Every non-blank line
/// is an object whose `table` names one of `tables` and whose `row` is an object; any other
/// line fails the whole batch, naming the line (`connector.source.receive-body`).
pub fn decode(body: &[u8], tables: &[&str]) -> Result<BTreeMap<String, Vec<Row>>, RunError> {
    let unreadable = |n: usize, why: String| RunError::PipelineUnreadableInput(format!("receive body line {n}: {why}"));
    let mut split: BTreeMap<String, Vec<Row>> = BTreeMap::new();
    for (i, line) in body.split(|b| *b == b'\n').enumerate() {
        let n = i + 1;
        if line.iter().all(u8::is_ascii_whitespace) {
            continue;
        }
        let value: Value = serde_json::from_slice(line).map_err(|e| unreadable(n, format!("not a JSON object: {e}")))?;
        let Value::Object(mut item) = value else { return Err(unreadable(n, "not a JSON object".into())) };
        let table = match item.remove("table") {
            Some(Value::String(t)) => t,
            _ => return Err(unreadable(n, "carries no string `table`".into())),
        };
        if !tables.contains(&table.as_str()) {
            return Err(unreadable(n, format!("names table `{table}`, which the pipeline does not declare; it declares {}", tables.join(", "))));
        }
        let row = match item.remove("row") {
            Some(Value::Object(row)) => row,
            _ => return Err(unreadable(n, "carries no object `row`".into())),
        };
        if let Some(extra) = item.keys().next() {
            return Err(unreadable(n, format!("carries key `{extra}`; a line holds `table` and `row`")));
        }
        split.entry(table).or_default().push(row);
    }
    Ok(split)
}

/// One table's delivered rows as the single pull of its run.
pub struct Delivered(pub Vec<Row>);

impl Source for Delivered {
    fn pull(&mut self, _request: &PullRequest, _cancel: &dyn Cancellation) -> Result<Vec<u8>, Failure> {
        serde_json::to_vec(&serde_json::json!({ "rows": self.0, "more": false })).map_err(|e| Failure::new(FailureTag::Permanent, e.to_string()))
    }
}
