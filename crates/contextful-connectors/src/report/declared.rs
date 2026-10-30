//! The declared HTTP report vendor: create, poll, state and result fields adapt one
//! vendor's async-report API into [`ReportVendor`], every request through the mediated
//! client under the create URL's host (`connector.source.report-declaration`).

use super::{Partition, ReportConfig, ReportVendor, RunState, VendorFault, Window};
use crate::http::Mediation;
use crate::request::{fill, header_templates, hydrate_headers, percent_encode, placeholder_in_authority, placeholders, verbatim};
use contextful_core::connector::attach::{endpoint, scrub, Allowlist};
use contextful_core::connector::reference::Template;
use contextful_core::connector::ConnectorError;
use contextful_core::run::{Failure, FailureTag, RunError};
use contextful_outbound::client::{Client, HeaderValue, Response};
use contextful_outbound::Resolver;
use serde::Deserialize;
use serde_json::{Map, Value};
use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::{Duration, Instant};
use url::Url;

/// The keys a `report` source reads beside those of [`ReportConfig`].
pub const VENDOR_KEYS: [&str; 5] = ["headers", "create", "poll", "state", "result"];

/// The placeholders every template fills from the partition and window.
const WINDOW_PLACEHOLDERS: [&str; 6] = ["account", "grain", "breakdown", "start", "end", "metrics"];

/// A `report` source's vendor half, declared beside its [`ReportConfig`] keys.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VendorDeclaration {
    /// Header name to value template, hydrated onto every request.
    #[serde(default)]
    pub headers: BTreeMap<String, String>,
    /// The request that creates a report run for one partition and window.
    pub create: CreateDeclaration,
    /// The request that reads a run's state.
    pub poll: PollDeclaration,
    /// Where a poll's answer holds the run's state, and the values naming each outcome.
    pub state: StateDeclaration,
    /// Where a ready run's rows are read.
    pub result: ResultDeclaration,
}

/// The create request.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CreateDeclaration {
    /// `POST`, the default, `PUT` or `GET`.
    #[serde(default = "default_method")]
    pub method: String,
    /// The URL template.
    pub url: String,
    /// A JSON body whose every string is a template; a `GET` carries none.
    #[serde(default)]
    pub body: Option<Value>,
    /// The JSON pointer to the created run's id in the answer, `/id` by default.
    #[serde(default = "default_id")]
    pub id: String,
}

/// The poll request.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PollDeclaration {
    /// The URL template, holding `{id}`.
    pub url: String,
    /// The least span between two polls of one run, such as `30s`.
    pub interval: String,
}

/// The run state a poll answers.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StateDeclaration {
    /// The JSON pointer to the status field.
    pub pointer: String,
    /// The status values of a ready run.
    pub done: Values,
    /// The status values of a run the vendor failed.
    pub failed: Values,
    /// The status values of a run the vendor expired.
    #[serde(default)]
    pub expired: Option<Values>,
}

/// One status value or a list of them.
#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
pub enum Values {
    One(String),
    Many(Vec<String>),
}

impl Values {
    fn list(&self) -> Vec<String> {
        match self {
            Values::One(v) => vec![v.clone()],
            Values::Many(v) => v.clone(),
        }
    }
}

/// Where a ready run's rows are: exactly one of the two.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResultDeclaration {
    /// The rows URL template, holding `{id}`.
    #[serde(default)]
    pub url: Option<String>,
    /// The JSON pointer to the rows URL in the done poll's answer.
    #[serde(default)]
    pub pointer: Option<String>,
}

fn default_method() -> String {
    "POST".into()
}

fn default_id() -> String {
    "/id".into()
}

/// Where a ready run's rows are read.
#[derive(Debug, Clone)]
enum Rows {
    Url(String),
    Pointer(String),
}

/// A checked `report` source declaration: the source's [`ReportConfig`] and its vendor.
#[derive(Debug, Clone)]
pub struct ReportDeclaration {
    pub report: ReportConfig,
    pub headers: BTreeMap<String, Template>,
    method: String,
    create_url: String,
    body: Option<Value>,
    id: String,
    poll_url: String,
    interval: Duration,
    state: String,
    done: Vec<String>,
    failed: Vec<String>,
    expired: Vec<String>,
    rows: Rows,
    origin: Url,
}

fn invalid(why: String) -> RunError {
    RunError::Invalid(format!("report source config: {why}"))
}

/// Hold `text`'s placeholders to the window's, a split key every partition carries, and
/// `{id}` where `id` is allowed.
fn check_placeholders(key: &str, text: &str, partitions: &[Partition], id: bool) -> Result<(), RunError> {
    for name in placeholders(text).map_err(|e| invalid(format!("`{key}`: {e}")))? {
        let known = WINDOW_PLACEHOLDERS.contains(&name)
            || (id && name == "id")
            || name.strip_prefix("split.").is_some_and(|k| !k.is_empty() && partitions.iter().all(|p| p.split.contains_key(k)));
        if !known {
            return Err(invalid(format!(
                "`{key}` names placeholder `{{{name}}}`; the placeholders are {}{}, and `{{split.<key>}}` for a split key every partition declares",
                WINDOW_PLACEHOLDERS.iter().map(|p| format!("`{{{p}}}`")).collect::<Vec<_>>().join(", "),
                if id { ", `{id}`" } else { "" }
            )));
        }
    }
    Ok(())
}

/// Check a URL template: its placeholders, no placeholder in the scheme or host, and the
/// origin it reaches once filled.
fn check_url(key: &str, text: &str, partitions: &[Partition], id: bool) -> Result<Url, RunError> {
    check_placeholders(key, text, partitions, id)?;
    if placeholder_in_authority(text) {
        return Err(invalid(format!("`{key}` places a placeholder in its scheme or host; a template binds only the path and query")));
    }
    let filled = fill(text, &|_| Some("x".to_string()), percent_encode);
    endpoint(super::NAME, &filled).map_err(|e| invalid(format!("`{key}`: {e}")))
}

fn same_origin(a: &Url, b: &Url) -> bool {
    a.scheme() == b.scheme() && a.host_str() == b.host_str() && a.port_or_known_default() == b.port_or_known_default()
}

fn check_pointer(key: &str, pointer: &str) -> Result<(), RunError> {
    if pointer.starts_with('/') {
        Ok(())
    } else {
        Err(invalid(format!("`{key} = \"{pointer}\"` is not a JSON pointer opening with `/`")))
    }
}

fn check_body(value: &Value, partitions: &[Partition]) -> Result<(), RunError> {
    match value {
        Value::String(s) => check_placeholders("create.body", s, partitions, false),
        Value::Array(items) => items.iter().try_for_each(|v| check_body(v, partitions)),
        Value::Object(map) => map.values().try_for_each(|v| check_body(v, partitions)),
        _ => Ok(()),
    }
}

impl ReportDeclaration {
    /// Parse and check a `report` source's configuration before any request.
    pub fn parse(config: &Value) -> Result<ReportDeclaration, RunError> {
        let cfg = config.as_object().ok_or_else(|| invalid("the config is a table".into()))?;
        let (vendor, rest): (Map<String, Value>, Map<String, Value>) = cfg.clone().into_iter().partition(|(k, _)| VENDOR_KEYS.contains(&k.as_str()));
        let report = ReportConfig::parse(&Value::Object(rest))?;
        let d: VendorDeclaration = serde_json::from_value(Value::Object(vendor)).map_err(|e| invalid(e.to_string()))?;
        let partitions = &report.partitions;
        let method = d.create.method.to_ascii_uppercase();
        if !matches!(method.as_str(), "GET" | "POST" | "PUT") {
            return Err(invalid(format!("`create.method = \"{}\"` is not `GET`, `POST` or `PUT`", d.create.method)));
        }
        if method == "GET" && d.create.body.is_some() {
            return Err(invalid("`create.body` beside `create.method = \"GET\"`; a GET carries no body".into()));
        }
        let origin = check_url("create.url", &d.create.url, partitions, false)?;
        if let Some(body) = &d.create.body {
            check_body(body, partitions)?;
        }
        check_pointer("create.id", &d.create.id)?;
        let poll = check_url("poll.url", &d.poll.url, partitions, true)?;
        if !placeholders(&d.poll.url).unwrap_or_default().contains(&"id") {
            return Err(invalid("`poll.url` holds no `{id}`; a poll reads one created run".into()));
        }
        let interval = contextful_core::time::duration_secs(&d.poll.interval)
            .ok_or_else(|| invalid(format!("`poll.interval = \"{}\"` is no span of digits followed by `s`, `m`, `h` or `d`", d.poll.interval)))?;
        check_pointer("state.pointer", &d.state.pointer)?;
        let (done, failed) = (d.state.done.list(), d.state.failed.list());
        let expired = d.state.expired.as_ref().map(Values::list).unwrap_or_default();
        for (key, values) in [("state.done", &done), ("state.failed", &failed)] {
            if values.is_empty() || values.iter().any(String::is_empty) {
                return Err(invalid(format!("`{key}` names no status value")));
            }
        }
        if let Some(v) = done.iter().chain(&failed).find(|v| [&done, &failed, &expired].iter().filter(|set| set.contains(v)).count() > 1) {
            return Err(invalid(format!("status value `{v}` names two outcomes")));
        }
        let rows = match (d.result.url, d.result.pointer) {
            (Some(url), None) => {
                let at = check_url("result.url", &url, partitions, true)?;
                if !same_origin(&origin, &at) {
                    return Err(invalid(format!("`result.url` reaches `{}`, another origin than `create.url`", scrub(&at))));
                }
                if !placeholders(&url).unwrap_or_default().contains(&"id") {
                    return Err(invalid("`result.url` holds no `{id}`; a result reads one created run".into()));
                }
                Rows::Url(url)
            }
            (None, Some(pointer)) => {
                check_pointer("result.pointer", &pointer)?;
                Rows::Pointer(pointer)
            }
            _ => return Err(invalid("`result` declares exactly one of `url` and `pointer`".into())),
        };
        if !same_origin(&origin, &poll) {
            return Err(invalid(format!("`poll.url` reaches `{}`, another origin than `create.url`", scrub(&poll))));
        }
        let headers = header_templates("", &d.headers)?;
        Ok(ReportDeclaration {
            report,
            headers,
            method,
            create_url: d.create.url,
            body: d.create.body,
            id: d.create.id,
            poll_url: d.poll.url,
            interval: Duration::from_secs(interval),
            state: d.state.pointer,
            done,
            failed,
            expired,
            rows,
            origin,
        })
    }

    /// The one host every request reaches. A declaration binding a credential holds to it.
    pub fn allowlist(&self) -> Result<Allowlist, ConnectorError> {
        let allow = Allowlist::parse(&[self.origin.host_str().unwrap_or_default()])?;
        if self.headers.values().any(Template::has_reference) {
            allow.check_bound()?;
        }
        Ok(allow)
    }
}

/// The declared vendor over the mediated client.
pub struct DeclaredVendor {
    declaration: ReportDeclaration,
    resolver: Arc<Resolver>,
    client: Client,
    /// When this vendor last polled each run.
    polled: BTreeMap<String, Instant>,
    /// The rows URL each ready run's done poll named, under `result.pointer`.
    results: BTreeMap<String, Url>,
}

fn fault(response: &Response) -> VendorFault {
    let body = serde_json::from_slice(&response.body).unwrap_or_else(|_| Value::String(String::from_utf8_lossy(&response.body).into_owned()));
    VendorFault { status: response.status, body, failure: None }
}

fn failed(failure: Failure) -> VendorFault {
    VendorFault { status: 0, body: Value::Null, failure: Some(failure) }
}

fn malformed(why: String) -> VendorFault {
    failed(Failure::new(FailureTag::Permanent, format!("the report vendor answered {why}")))
}

fn text_of(value: &Value) -> Option<String> {
    match value {
        Value::String(s) => Some(s.clone()),
        Value::Number(n) => Some(n.to_string()),
        Value::Bool(b) => Some(b.to_string()),
        _ => None,
    }
}

impl DeclaredVendor {
    /// A vendor whose every request passes `mediation` under the declaration's one host.
    pub fn mediated(declaration: ReportDeclaration, resolver: Arc<Resolver>, mediation: &Mediation) -> Result<DeclaredVendor, ConnectorError> {
        let client = mediation.client(declaration.allowlist()?, declaration.origin.clone(), None);
        Ok(DeclaredVendor { declaration, resolver, client, polled: BTreeMap::new(), results: BTreeMap::new() })
    }

    /// The vendor's mediated client.
    pub fn client(&self) -> &Client {
        &self.client
    }

    fn value(partition: &Partition, window: Option<&Window>, metrics: &[String], id: Option<&str>, name: &str) -> Option<String> {
        match name {
            "account" => Some(partition.account.clone()),
            "grain" => Some(partition.grain.clone()),
            "breakdown" => Some(partition.breakdown.clone()),
            "start" => window.map(|w| w.start.clone()),
            "end" => window.map(|w| w.end.clone()),
            "metrics" => Some(metrics.join(",")),
            "id" => id.map(str::to_string),
            other => other.strip_prefix("split.").and_then(|k| partition.split.get(k).cloned()),
        }
    }

    fn url(&self, template: &str, partition: &Partition, window: Option<&Window>, id: Option<&str>) -> Result<Url, VendorFault> {
        let metrics = &self.declaration.report.metrics;
        let filled = fill(template, &|name| Self::value(partition, window, metrics, id, name), percent_encode);
        endpoint(super::NAME, &filled).map_err(|e| failed(Failure::deterministic(FailureTag::Config, e.to_string())))
    }

    fn body(&self, value: &Value, partition: &Partition, window: &Window) -> Value {
        let metrics = &self.declaration.report.metrics;
        match value {
            Value::String(s) => Value::String(fill(s, &|name| Self::value(partition, Some(window), metrics, None, name), verbatim)),
            Value::Array(items) => Value::Array(items.iter().map(|v| self.body(v, partition, window)).collect()),
            Value::Object(map) => Value::Object(map.iter().map(|(k, v)| (k.clone(), self.body(v, partition, window))).collect()),
            other => other.clone(),
        }
    }

    fn headers(&self, json: bool) -> Result<Vec<(String, HeaderValue)>, VendorFault> {
        hydrate_headers(&self.resolver, &self.declaration.headers, json.then_some("application/json")).map_err(failed)
    }

    /// Send one request and answer its JSON body, a non-2xx answer as a vendor fault.
    fn exchange(&self, method: &str, url: &Url, body: Option<&Value>) -> Result<Value, VendorFault> {
        let bytes = body.map(|b| serde_json::to_vec(b).unwrap_or_default());
        let response = self.client.send(method, url, &self.headers(body.is_some())?, bytes.as_deref()).map_err(failed)?;
        if !(200..300).contains(&response.status) {
            return Err(fault(&response));
        }
        serde_json::from_slice(&response.body).map_err(|e| malformed(format!("a body that is not JSON from `{}`: {e}", scrub(url))))
    }
}

impl ReportVendor for DeclaredVendor {
    fn create(&mut self, partition: &Partition, window: &Window) -> Result<String, VendorFault> {
        let d = &self.declaration;
        let url = self.url(&d.create_url, partition, Some(window), None)?;
        let body = d.body.as_ref().map(|b| self.body(b, partition, window));
        let answer = self.exchange(&d.method, &url, body.as_ref())?;
        answer
            .pointer(&d.id)
            .and_then(text_of)
            .filter(|id| !id.is_empty())
            .ok_or_else(|| malformed(format!("a created run naming no id at `{}`", d.id)))
    }

    fn poll(&mut self, partition: &Partition, run_id: &str) -> Result<RunState, VendorFault> {
        if let Some(wait) = self.polled.get(run_id).and_then(|at| self.declaration.interval.checked_sub(at.elapsed())) {
            std::thread::sleep(wait);
        }
        let url = self.url(&self.declaration.poll_url, partition, None, Some(run_id))?;
        self.polled.insert(run_id.to_string(), Instant::now());
        let answer = self.exchange("GET", &url, None)?;
        let d = &self.declaration;
        let Some(state) = answer.pointer(&d.state).and_then(text_of) else {
            return Ok(RunState::Pending);
        };
        if d.failed.contains(&state) {
            return Ok(RunState::Failed);
        }
        if d.expired.contains(&state) {
            return Ok(RunState::Expired);
        }
        if !d.done.contains(&state) {
            return Ok(RunState::Pending);
        }
        if let Rows::Pointer(pointer) = &d.rows {
            let named = answer.pointer(pointer).and_then(Value::as_str).ok_or_else(|| malformed(format!("a done run naming no rows URL at `{pointer}`")))?;
            let at = url.join(named).map_err(|e| malformed(format!("a rows URL that does not parse: {e}")))?;
            self.results.insert(run_id.to_string(), at);
        }
        Ok(RunState::Ready)
    }

    fn page(&mut self, partition: &Partition, run_id: &str, cursor: Option<&str>) -> Result<Value, VendorFault> {
        let mut url = match &self.declaration.rows {
            Rows::Url(template) => self.url(template, partition, None, Some(run_id))?,
            Rows::Pointer(_) => self.results.get(run_id).cloned().ok_or_else(|| malformed(format!("no rows URL for run `{run_id}`")))?,
        };
        if let Some(cursor) = cursor {
            url.query_pairs_mut().append_pair(&self.declaration.report.cursor_param, cursor);
        }
        self.exchange("GET", &url, None)
    }
}
