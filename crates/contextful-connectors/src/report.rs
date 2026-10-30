//! The async-report source: a vendor builds a report run per partition and window, the
//! source commits the run id, polls within a budget and drains a fixed number of result
//! pages per read, holding one watermark per partition in a versioned position.
//! [`declared`] adapts a declared HTTP vendor into [`ReportVendor`] as the `report` source.

pub mod declared;

use contextful_core::connector::ConnectorError;
use contextful_core::run::ports::{Cancellation, PullRequest, Source};
use contextful_core::run::{Failure, FailureTag, RunError};
use contextful_core::store::reserve::INGESTED_AT;
use contextful_core::time::Instant;
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};

/// The source's registered name.
pub const NAME: &str = "report";
/// The position layout's version.
pub const POSITION_VERSION: u64 = 1;
/// The column carrying the partition key a row was read under.
pub const PARTITION_COLUMN: &str = "report_partition";
/// The column carrying a row's deterministic key.
pub const KEY_COLUMN: &str = "report_key";
/// The column carrying a row's event time.
pub const EVENT_TIME_COLUMN: &str = "event_time";
const DAY_SECS: i64 = 86_400;

/// One report partition: the account, the grain, the breakdown and any split dimension.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Partition {
    pub account: String,
    pub grain: String,
    #[serde(default)]
    pub breakdown: String,
    #[serde(default)]
    pub split: BTreeMap<String, String>,
}

impl Partition {
    /// The partition's key in the position map: every dimension, the split in key order.
    pub fn key(&self) -> String {
        let mut key = format!("{}|{}|{}", self.account, self.grain, self.breakdown);
        for (k, v) in &self.split {
            key.push_str(&format!("|{k}={v}"));
        }
        key
    }

    fn breakdown_fields(&self) -> impl Iterator<Item = &str> {
        self.breakdown.split(',').map(str::trim).filter(|f| !f.is_empty())
    }
}

/// One report window, `start` and `end` as `YYYY-MM-DD` days.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub struct Window {
    pub start: String,
    pub end: String,
}

/// What a poll of a vendor run answers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunState {
    Pending,
    Ready,
    Expired,
    /// The vendor failed the run (`connector.source.report-failed`).
    Failed,
}

/// A vendor's failed answer: its HTTP status and its body, or the failure of a request
/// that drew no answer.
#[derive(Debug, Clone, PartialEq)]
pub struct VendorFault {
    pub status: u16,
    pub body: Value,
    /// A request's own failure, classified already: a transport fault or a refusal of the
    /// mediated client.
    pub failure: Option<Failure>,
}

/// The vendor half of an async-report source.
pub trait ReportVendor {
    /// Create a report run for `partition` over `window`, answering its run id.
    fn create(&mut self, partition: &Partition, window: &Window) -> Result<String, VendorFault>;
    /// One poll of a run's state.
    fn poll(&mut self, partition: &Partition, run_id: &str) -> Result<RunState, VendorFault>;
    /// One result page of a ready run, from `cursor` or the first page.
    fn page(&mut self, partition: &Partition, run_id: &str, cursor: Option<&str>) -> Result<Value, VendorFault>;
}

/// An async-report source's declaration.
#[derive(Debug, Clone)]
pub struct ReportConfig {
    pub partitions: Vec<Partition>,
    pub metrics: Vec<String>,
    /// Per metric, the grains the vendor estimates rather than computes.
    pub estimated: BTreeMap<String, Vec<String>>,
    pub entity: String,
    pub date: String,
    pub order_by: Option<String>,
    pub pages_per_read: usize,
    pub poll_budget: usize,
    pub lookback_days: u32,
    pub window_days: u32,
    pub throttle_codes: Vec<i64>,
    pub rows: String,
    pub more: String,
    pub cursor: String,
    pub next: String,
    pub cursor_param: String,
    pub error_code: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Declared {
    partitions: Vec<Partition>,
    metrics: Vec<String>,
    #[serde(default)]
    estimated: BTreeMap<String, Vec<String>>,
    entity: String,
    date: String,
    #[serde(default)]
    order_by: Option<String>,
    #[serde(default = "default_pages")]
    pages_per_read: usize,
    #[serde(default = "default_polls")]
    poll_budget: usize,
    #[serde(default = "default_days")]
    lookback_days: u32,
    #[serde(default = "default_days")]
    window_days: u32,
    #[serde(default)]
    throttle_codes: Vec<i64>,
    #[serde(default = "default_rows")]
    rows: String,
    #[serde(default = "default_more")]
    more: String,
    #[serde(default = "default_cursor")]
    cursor: String,
    #[serde(default = "default_next")]
    next: String,
    #[serde(default = "default_cursor_param")]
    cursor_param: String,
    #[serde(default = "default_error_code")]
    error_code: String,
}

fn default_pages() -> usize {
    8
}
fn default_polls() -> usize {
    10
}
fn default_days() -> u32 {
    28
}
fn default_rows() -> String {
    "/data".into()
}
fn default_more() -> String {
    "/has_more".into()
}
fn default_cursor() -> String {
    "/paging/cursors/after".into()
}
fn default_next() -> String {
    "/paging/next".into()
}
fn default_cursor_param() -> String {
    "after".into()
}
fn default_error_code() -> String {
    "/error/code".into()
}

impl ReportConfig {
    pub fn parse(value: &Value) -> Result<Self, RunError> {
        let d: Declared = serde_json::from_value(value.clone()).map_err(|e| RunError::Invalid(format!("report source config: {e}")))?;
        if d.partitions.is_empty() || d.metrics.is_empty() {
            return Err(RunError::Invalid("a report source declares at least one partition and one metric".into()));
        }
        if d.pages_per_read == 0 || d.poll_budget == 0 {
            return Err(RunError::Invalid("a report source's `pages_per_read` and `poll_budget` are positive".into()));
        }
        for partition in &d.partitions {
            for metric in &d.metrics {
                if d.estimated.get(metric).is_some_and(|grains| grains.contains(&partition.grain)) {
                    return Err(RunError::Invalid(
                        ConnectorError::ConnectorMetricGrainRejected(format!("metric `{metric}` at grain `{}` is a vendor estimate", partition.grain)).to_string(),
                    ));
                }
            }
        }
        if d.order_by.as_deref() == Some(INGESTED_AT) {
            return Err(RunError::Invalid(
                ConnectorError::ConnectorOrderingColumnRejected(format!("a report stream orders by a vendor column, not the ingest stamp `{INGESTED_AT}`")).to_string(),
            ));
        }
        Ok(ReportConfig {
            partitions: d.partitions,
            metrics: d.metrics,
            estimated: d.estimated,
            entity: d.entity,
            date: d.date,
            order_by: d.order_by,
            pages_per_read: d.pages_per_read,
            poll_budget: d.poll_budget,
            lookback_days: d.lookback_days,
            window_days: d.window_days,
            throttle_codes: d.throttle_codes,
            rows: d.rows,
            more: d.more,
            cursor: d.cursor,
            next: d.next,
            cursor_param: d.cursor_param,
            error_code: d.error_code,
        })
    }
}

/// One partition's committed state.
#[derive(Debug, Clone, Default, PartialEq, Deserialize, Serialize)]
struct Held {
    #[serde(default)]
    watermark: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    run: Option<HeldRun>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    cursor: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
struct HeldRun {
    id: String,
    #[serde(flatten)]
    window: Window,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
struct Position {
    version: u64,
    #[serde(default)]
    partitions: BTreeMap<String, Held>,
    /// The read's progress; absent between reads. The position, not the source instance,
    /// carries it, so a pull after replayed ones resumes where the recorded batches left the
    /// read (`run.journal.replay-lands`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    reading: Option<Reading>,
}

/// One read's progress across its pulls.
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
struct Reading {
    /// The day the read's windows end on: its first pull's day, `YYYY-MM-DD`.
    day: String,
    /// The pages the read has drained.
    #[serde(default)]
    pages: usize,
    /// Partitions the read has finished or held.
    #[serde(default, skip_serializing_if = "BTreeSet::is_empty")]
    finished: BTreeSet<String>,
    /// Partitions whose run the read has recreated.
    #[serde(default, skip_serializing_if = "BTreeSet::is_empty")]
    recreated: BTreeSet<String>,
}

enum Step {
    /// A run was created and its id committed.
    Created,
    /// The polling budget ran out with the run still building.
    Pending,
    /// One page of rows.
    Page(Value),
    /// The vendor answered `Expired` for a run this read already recreated.
    ExpiredAgain,
    /// The vendor failed the held run, whose id this is.
    Failed(String),
}

/// The async-report source over one vendor.
pub struct ReportSource<V> {
    config: ReportConfig,
    vendor: V,
    /// The day a read starting at this source's first pull ends its windows on.
    today: i64,
    /// Partitions whose held run this source instance has polled. A resumed process polls a
    /// held run again before its next page: a run can expire between processes, and the
    /// rows URL a done poll names lives in the vendor's memory.
    polled: BTreeSet<String>,
    /// Audit entries the next batch carries.
    audit: Vec<String>,
    /// The failure the next pull raises, once the batch dropping a failed run commits.
    failed: Option<Failure>,
}

fn day_of(text: &str) -> Option<i64> {
    let day = text.get(..10)?;
    let instant = Instant::parse(&format!("{day}T00:00:00Z")).ok()?;
    let days = instant.unix_secs().div_euclid(DAY_SECS);
    (date_of(days) == day).then_some(days)
}

fn date_of(days: i64) -> String {
    Instant::from_unix_secs(days * DAY_SECS).map(|i| i.to_rfc3339()[..10].to_string()).unwrap_or_default()
}

impl<V: ReportVendor> ReportSource<V> {
    /// A source over `vendor`, reading windows that end on `today` (`YYYY-MM-DD`).
    pub fn new(config: ReportConfig, vendor: V, today: &str) -> Result<Self, RunError> {
        let today = day_of(today).filter(|_| today.len() == 10).ok_or_else(|| RunError::Invalid(format!("`{today}` is not a YYYY-MM-DD day")))?;
        Ok(ReportSource {
            config,
            vendor,
            today,
            polled: BTreeSet::new(),
            audit: Vec::new(),
            failed: None,
        })
    }

    /// Classify a vendor fault: a throttle code in the error body or a throttle status is
    /// rate limiting, whatever the status; an authentication status is terminal; a `5xx` is
    /// transient.
    pub fn classify(config: &ReportConfig, fault: &VendorFault) -> Failure {
        if let Some(failure) = &fault.failure {
            return failure.clone();
        }
        let code = fault.body.pointer(&config.error_code).and_then(Value::as_i64);
        match fault.status {
            _ if code.is_some_and(|c| config.throttle_codes.contains(&c)) => Failure::new(
                FailureTag::RateLimited,
                format!("the report vendor throttled the read ({}, code {})", fault.status, code.unwrap_or_default()),
            ),
            429 => Failure::new(FailureTag::RateLimited, "the report vendor throttled the read (429)"),
            401 | 403 => Failure::deterministic(
                FailureTag::Permanent,
                ConnectorError::ConnectorAuthTerminal(format!("the report vendor answered {}", fault.status)).to_string(),
            ),
            500..=599 => Failure::new(FailureTag::Transient, format!("the report vendor answered {}", fault.status)),
            status => Failure::new(FailureTag::Permanent, format!("the report vendor answered {status}: {}", fault.body)),
        }
    }

    /// The window a run for a partition holding `held` covers: a caught-up partition
    /// re-opens its lookback, and the window is never narrower than the lookback.
    fn window(&self, held: &Held, today: i64) -> Window {
        let lookback = i64::from(self.config.lookback_days);
        let width = lookback.max(i64::from(self.config.window_days));
        let start = match held.watermark.as_deref().and_then(day_of) {
            Some(mark) => mark.min(today) - lookback,
            None => today - width,
        };
        Window { start: date_of(start), end: date_of(today) }
    }

    /// One advance of a partition. A held run is polled before its first page this read,
    /// cursor or not, so a run that expired between reads is recreated over its window and
    /// restarts from its first page; a second expiry in one read holds the partition.
    fn step(&mut self, partition: &Partition, held: &mut Held, reading: &mut Reading, today: i64) -> Result<Step, VendorFault> {
        let key = partition.key();
        let run = match held.run.clone() {
            None => {
                let window = self.window(held, today);
                let id = self.vendor.create(partition, &window)?;
                held.run = Some(HeldRun { id, window });
                held.cursor = None;
                return Ok(Step::Created);
            }
            Some(run) => run,
        };
        if held.cursor.is_none() || !self.polled.contains(&key) {
            self.polled.insert(key.clone());
            let mut ready = false;
            for _ in 0..self.config.poll_budget {
                match self.vendor.poll(partition, &run.id)? {
                    RunState::Ready => {
                        ready = true;
                        break;
                    }
                    RunState::Pending => {}
                    RunState::Failed => return Ok(Step::Failed(run.id)),
                    RunState::Expired => {
                        if !reading.recreated.insert(key) {
                            return Ok(Step::ExpiredAgain);
                        }
                        let id = self.vendor.create(partition, &run.window)?;
                        held.run = Some(HeldRun { id, window: run.window });
                        held.cursor = None;
                        return Ok(Step::Created);
                    }
                }
            }
            if !ready {
                return Ok(Step::Pending);
            }
        }
        let page = self.vendor.page(partition, &run.id, held.cursor.as_deref())?;
        Ok(Step::Page(page))
    }

    fn next_cursor(&self, page: &Value) -> Result<Option<String>, Failure> {
        let more = page.pointer(&self.config.more).and_then(Value::as_bool).unwrap_or(false);
        if !more {
            return Ok(None);
        }
        let named = page.pointer(&self.config.cursor).and_then(Value::as_str).filter(|c| !c.is_empty()).map(str::to_string);
        let linked = || {
            let link = page.pointer(&self.config.next).and_then(Value::as_str)?;
            let url = url::Url::parse(link).ok()?;
            url.query_pairs().find(|(k, _)| k == self.config.cursor_param.as_str()).map(|(_, v)| v.to_string()).filter(|c| !c.is_empty())
        };
        named.or_else(linked).map(Some).ok_or_else(|| {
            Failure::deterministic(
                FailureTag::Permanent,
                ConnectorError::ConnectorPageCursorMissing(format!(
                    "a page announces another while naming no cursor at `{}` or `{}`",
                    self.config.cursor, self.config.next
                ))
                .to_string(),
            )
        })
    }

    fn rows(&self, partition: &Partition, page: &Value) -> Vec<Value> {
        let key = partition.key();
        let raw = page.pointer(&self.config.rows).and_then(Value::as_array).cloned().unwrap_or_default();
        raw.into_iter()
            .map(|row| {
                let mut row = match row {
                    Value::Object(map) => map,
                    other => Map::from_iter([("value".to_string(), other)]),
                };
                let date = row.get(&self.config.date).and_then(Value::as_str).and_then(|d| day_of(d).map(date_of));
                let breakdown: Vec<Value> = partition.breakdown_fields().map(|f| row.get(f).cloned().unwrap_or(Value::Null)).collect();
                let entity = row.get(&self.config.entity).cloned().unwrap_or(Value::Null);
                let identity = json!([key, entity, date, breakdown]);
                let digest = Sha256::digest(identity.to_string().as_bytes());
                row.insert(PARTITION_COLUMN.into(), Value::String(key.clone()));
                row.insert(KEY_COLUMN.into(), Value::String(format!("{digest:x}")));
                row.insert(EVENT_TIME_COLUMN.into(), date.map_or(Value::Null, |d| Value::String(format!("{d}T00:00:00Z"))));
                Value::Object(row)
            })
            .collect()
    }

    /// A batch carrying the audit entries recorded since the last one; its position carries
    /// the read's progress while the read goes on.
    fn batch(&mut self, rows: Vec<Value>, mut position: Position, reading: Reading, more: bool) -> Result<Vec<u8>, Failure> {
        position.reading = more.then_some(reading);
        let audit = std::mem::take(&mut self.audit);
        serde_json::to_vec(&json!({"rows": rows, "cursor": position, "more": more, "audit": audit})).map_err(|e| Failure::new(FailureTag::Permanent, e.to_string()))
    }

    /// Record a partition held at its committed position.
    fn hold(&mut self, reading: &mut Reading, key: String, reason: &str) {
        self.audit.push(format!("report partition `{key}` held at its committed position: {reason}"));
        reading.finished.insert(key);
    }
}

/// Whether a failure holds its partition rather than failing the read: a transient or
/// rate-limited failure no rerun is certain to repeat.
pub(crate) fn retryable(failure: &Failure) -> bool {
    !failure.deterministic && matches!(failure.tag, FailureTag::Transient | FailureTag::RateLimited)
}

impl<V: ReportVendor> Source for ReportSource<V> {
    fn pull(&mut self, request: &PullRequest, cancel: &dyn Cancellation) -> Result<Vec<u8>, Failure> {
        let mut position: Position = match &request.position {
            None => Position::default(),
            Some(value) => serde_json::from_value(value.clone()).map_err(|e| Failure::deterministic(FailureTag::Config, format!("report position: {e}")))?,
        };
        if position.version > POSITION_VERSION {
            return Err(Failure::deterministic(FailureTag::Config, format!("report position version {} is newer than {POSITION_VERSION}", position.version)));
        }
        position.version = POSITION_VERSION;
        if let Some(failure) = self.failed.take() {
            return Err(failure);
        }
        let mut reading = position.reading.take().unwrap_or_else(|| Reading { day: date_of(self.today), ..Reading::default() });
        let today = day_of(&reading.day)
            .filter(|_| reading.day.len() == 10)
            .ok_or_else(|| Failure::deterministic(FailureTag::Config, format!("report position: `{}` is not a YYYY-MM-DD day", reading.day)))?;
        loop {
            if cancel.requested() {
                return Err(Failure::canceled("stopped during report read"));
            }
            if reading.pages >= self.config.pages_per_read {
                return self.batch(Vec::new(), position, reading, false);
            }
            let Some(partition) = self.config.partitions.iter().find(|p| !reading.finished.contains(&p.key())).cloned() else {
                return self.batch(Vec::new(), position, reading, false);
            };
            let key = partition.key();
            let committed = position.partitions.get(&key).cloned().unwrap_or_default();
            let mut held = committed.clone();
            match self.step(&partition, &mut held, &mut reading, today) {
                Ok(Step::Created) => {
                    position.partitions.insert(key, held);
                    return self.batch(Vec::new(), position, reading, true);
                }
                Ok(Step::Pending) => return self.batch(Vec::new(), position, reading, false),
                Ok(Step::ExpiredAgain) => self.hold(&mut reading, key, "the vendor answered Expired for a run this read recreated"),
                // The batch dropping the failed run commits first, so the next fire creates
                // a new one; the pull after it fails the read.
                Ok(Step::Failed(id)) => {
                    held.run = None;
                    held.cursor = None;
                    position.partitions.insert(key.clone(), held);
                    self.failed = Some(Failure::deterministic(
                        FailureTag::Permanent,
                        ConnectorError::ConnectorReportFailed(format!("report run `{id}` of partition `{key}` answered a declared failed state")).to_string(),
                    ));
                    return self.batch(Vec::new(), position, reading, true);
                }
                Ok(Step::Page(page)) => {
                    let next = self.next_cursor(&page)?;
                    let rows = self.rows(&partition, &page);
                    match next {
                        Some(cursor) => held.cursor = Some(cursor),
                        None => {
                            held.watermark = held.run.take().map(|run| run.window.end);
                            held.cursor = None;
                            reading.finished.insert(key.clone());
                        }
                    }
                    position.partitions.insert(key, held);
                    reading.pages += 1;
                    let more = reading.pages < self.config.pages_per_read;
                    return self.batch(rows, position, reading, more);
                }
                Err(fault) => {
                    let failure = Self::classify(&self.config, &fault);
                    if !retryable(&failure) {
                        return Err(failure);
                    }
                    self.hold(&mut reading, key, &failure.to_string());
                }
            }
        }
    }
}
