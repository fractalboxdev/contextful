//! The `search` source: one declared provider endpoint, a URL template filled per named
//! query, cursor paging, and one watermark per query in a versioned position
//! (`connector.source.search-config`). Every request passes the mediated client under the
//! template's one host.

use crate::http::{ConfigError, Mediation};
use crate::report::retryable;
use crate::request::{fill, header_templates, hydrate_headers, percent_encode, placeholder_in_authority, placeholders};
use contextful_core::connector::attach::{endpoint, is_loopback_host, scrub, Allowlist};
use contextful_core::connector::reference::Template;
use contextful_core::connector::ConnectorError;
use contextful_core::run::ports::{Cancellation, PullRequest, Source};
use contextful_core::run::{Failure, FailureTag, RunError};
use contextful_core::time::Instant;
use contextful_outbound::client::Client;
use contextful_outbound::Resolver;
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};
use std::collections::BTreeMap;
use std::ops::Bound;
use std::sync::Arc;
use url::Url;

/// The source's registered name.
pub const NAME: &str = "search";
/// The position layout's version.
pub const POSITION_VERSION: u64 = 1;
/// The column carrying the name of the query a result answered.
pub const QUERY_COLUMN: &str = "search_query";
/// The placeholders the `url` template holds.
const PLACEHOLDERS: [&str; 2] = ["query", "since"];

/// A `search` source's declaration.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SearchDeclaration {
    /// The provider URL template, holding `{query}` and `{since}`.
    pub url: String,
    /// Query name to query text; each name holds its own watermark.
    pub queries: BTreeMap<String, String>,
    /// The span before the fire's instant a query holding no watermark reads from, such as `7d`.
    pub freshness: String,
    /// The most results one query's pass lands.
    pub max_results: u64,
    /// Header name to value template, hydrated onto every request.
    #[serde(default)]
    pub headers: BTreeMap<String, String>,
    /// The JSON pointer to a page's result array, `/results` by default.
    #[serde(default = "default_results")]
    pub results: String,
    /// The JSON pointer to a page's next cursor, `/next` by default.
    #[serde(default = "default_cursor")]
    pub cursor: String,
    /// The query parameter carrying the cursor, `cursor` by default.
    #[serde(default = "default_cursor_param")]
    pub cursor_param: String,
    /// The result field holding its instant, `published_at` by default.
    #[serde(default = "default_time")]
    pub time: String,
}

fn default_results() -> String {
    "/results".into()
}
fn default_cursor() -> String {
    "/next".into()
}
fn default_cursor_param() -> String {
    "cursor".into()
}
fn default_time() -> String {
    "published_at".into()
}

/// A checked `search` source declaration.
#[derive(Debug, Clone)]
pub struct SearchConfig {
    url: String,
    origin: Url,
    /// Query name to query text, read in name order.
    pub queries: BTreeMap<String, String>,
    freshness: u64,
    max_results: u64,
    pub headers: BTreeMap<String, Template>,
    results: String,
    cursor: String,
    cursor_param: String,
    time: String,
}

fn invalid(why: String) -> ConfigError {
    ConfigError::Run(RunError::Invalid(format!("`{NAME}` source config: {why}")))
}

impl SearchConfig {
    /// Parse and check a `search` source's configuration before any request.
    pub fn parse(config: &Value) -> Result<SearchConfig, ConfigError> {
        let d: SearchDeclaration = serde_json::from_value(config.clone()).map_err(|e| invalid(e.to_string()))?;
        if d.queries.is_empty() || d.queries.iter().any(|(name, text)| name.is_empty() || text.is_empty()) {
            return Err(invalid("`queries` names at least one query, each name and text non-empty".into()));
        }
        if d.max_results == 0 {
            return Err(invalid("`max_results` is positive".into()));
        }
        let freshness = contextful_core::time::duration_secs(&d.freshness)
            .ok_or_else(|| invalid(format!("`freshness = \"{}\"` is no span of digits followed by `s`, `m`, `h` or `d`", d.freshness)))?;
        for (key, pointer) in [("results", &d.results), ("cursor", &d.cursor)] {
            if !pointer.starts_with('/') {
                return Err(invalid(format!("`{key} = \"{pointer}\"` is not a JSON pointer opening with `/`")));
            }
        }
        if d.cursor_param.is_empty() || d.time.is_empty() {
            return Err(invalid("`cursor_param` and `time` are non-empty".into()));
        }
        let named = placeholders(&d.url).map_err(|e| invalid(format!("`url`: {e}")))?;
        if let Some(other) = named.iter().find(|n| !PLACEHOLDERS.contains(n)) {
            return Err(invalid(format!("`url` names placeholder `{{{other}}}`; the placeholders are `{{query}}` and `{{since}}`")));
        }
        if let Some(missing) = PLACEHOLDERS.iter().find(|p| !named.contains(p)) {
            return Err(invalid(format!("`url` holds no `{{{missing}}}`")));
        }
        if placeholder_in_authority(&d.url) {
            return Err(invalid("`url` places a placeholder in its scheme or host; a template binds only the path and query".into()));
        }
        let origin = endpoint(NAME, &fill(&d.url, &|_| Some("x".to_string()), percent_encode)).map_err(|e| invalid(format!("`url`: {e}")))?;
        if origin.scheme() != "https" && !is_loopback_host(origin.host_str().unwrap_or_default()) {
            return Err(ConnectorError::ConnectorProviderOriginRejected(format!("`{NAME}` source `url` names `{}`; a search provider is reached over TLS", scrub(&origin))).into());
        }
        Ok(SearchConfig {
            url: d.url,
            origin,
            queries: d.queries,
            freshness,
            max_results: d.max_results,
            headers: header_templates("", &d.headers)?,
            results: d.results,
            cursor: d.cursor,
            cursor_param: d.cursor_param,
            time: d.time,
        })
    }

    /// The one host every request reaches, the provider's. A declaration binding a
    /// credential holds to it (`connector.source.provider-origin`).
    pub fn allowlist(&self) -> Result<Allowlist, ConnectorError> {
        let allow = Allowlist::parse(&[self.origin.host_str().unwrap_or_default()])?;
        if self.headers.values().any(Template::has_reference) {
            allow.check_bound()?;
        }
        Ok(allow)
    }
}

/// One query's pass: the `{since}` it began with, the cursor of its next page, the
/// greatest result instant it has landed, and how many results it has landed.
#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
struct Pass {
    since: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    cursor: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    high: Option<String>,
    #[serde(default)]
    landed: u64,
}

/// One query's committed state.
#[derive(Debug, Clone, Default, PartialEq, Deserialize, Serialize)]
struct Held {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    watermark: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pass: Option<Pass>,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
struct Position {
    version: u64,
    #[serde(default)]
    queries: BTreeMap<String, Held>,
    /// The query the read reads next, in name order; absent between reads. The position,
    /// not the source instance, carries it, so a pull after replayed ones resumes where the
    /// recorded batches left the read (`run.journal.replay-lands`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    reading: Option<String>,
    /// The `{since}` the read fills for a query holding no watermark, from its first pull's
    /// instant; absent between reads.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    floor: Option<String>,
}

/// The later of two instants, either absent.
fn later(a: Option<String>, b: Option<Instant>) -> Option<String> {
    let a_at = a.as_deref().and_then(|t| Instant::parse(t).ok());
    match (a_at, b) {
        (Some(x), Some(y)) if y > x => Some(y.to_rfc3339()),
        (None, Some(y)) => Some(y.to_rfc3339()),
        _ => a,
    }
}

/// The `search` source over the mediated client.
pub struct SearchSource {
    config: SearchConfig,
    resolver: Arc<Resolver>,
    client: Client,
    /// The `{since}` of a query holding no watermark in a read starting at this source's
    /// first pull: the fire's instant less `freshness`.
    floor: String,
    /// Audit entries the next batch carries.
    audit: Vec<String>,
}

impl SearchSource {
    /// A source whose every request passes `mediation` under the provider's one host, for a
    /// fire at `now`.
    pub fn mediated(config: SearchConfig, resolver: Arc<Resolver>, mediation: &Mediation, now: Instant) -> Result<SearchSource, ConnectorError> {
        let client = mediation.client(config.allowlist()?, config.origin.clone(), None);
        let floor_secs = now.unix_secs().saturating_sub(i64::try_from(config.freshness).unwrap_or(i64::MAX));
        let floor = Instant::from_unix_secs(floor_secs).unwrap_or(now).to_rfc3339();
        Ok(SearchSource { config, resolver, client, floor, audit: Vec::new() })
    }

    /// The declared query after `name` in name order, `None` past the last.
    fn after(&self, name: &str) -> Option<String> {
        self.config.queries.range::<str, _>((Bound::Excluded(name), Bound::Unbounded)).next().map(|(n, _)| n.clone())
    }

    fn page(&self, query: &str, pass: &Pass) -> Result<Value, Failure> {
        let filled = fill(&self.config.url, &|name| match name {
            "query" => Some(query.to_string()),
            "since" => Some(pass.since.clone()),
            _ => None,
        }, percent_encode);
        let mut url = endpoint(NAME, &filled).map_err(|e| Failure::deterministic(FailureTag::Config, e.to_string()))?;
        if let Some(cursor) = &pass.cursor {
            url.query_pairs_mut().append_pair(&self.config.cursor_param, cursor);
        }
        let headers = hydrate_headers(&self.resolver, &self.config.headers, None)?;
        let response = self.client.send("GET", &url, &headers, None)?;
        let status = response.status;
        if !(200..300).contains(&status) {
            let why = format!("the search provider answered {status} for `{}`", scrub(&url));
            return Err(match status {
                429 => Failure::new(FailureTag::RateLimited, why),
                500..=599 => Failure::new(FailureTag::Transient, why),
                _ => Failure::new(FailureTag::Permanent, why),
            });
        }
        serde_json::from_slice(&response.body).map_err(|e| Failure::new(FailureTag::Permanent, format!("the search provider answered `{}` with a body that is not JSON: {e}", scrub(&url))))
    }

    /// A batch carrying the audit entries recorded since the last one; the last batch of a
    /// read drops the read's floor.
    fn batch(&mut self, rows: Vec<Value>, position: &mut Position, more: bool) -> Result<Vec<u8>, Failure> {
        if !more {
            position.floor = None;
        }
        let audit = std::mem::take(&mut self.audit);
        serde_json::to_vec(&json!({"rows": rows, "cursor": position, "more": more, "audit": audit})).map_err(|e| Failure::new(FailureTag::Permanent, e.to_string()))
    }
}

impl Source for SearchSource {
    fn pull(&mut self, request: &PullRequest, cancel: &dyn Cancellation) -> Result<Vec<u8>, Failure> {
        let mut position: Position = match &request.position {
            None => Position::default(),
            Some(value) => serde_json::from_value(value.clone()).map_err(|e| Failure::deterministic(FailureTag::Config, format!("search position: {e}")))?,
        };
        if position.version > POSITION_VERSION {
            return Err(Failure::deterministic(FailureTag::Config, format!("search position version {} is newer than {POSITION_VERSION}", position.version)));
        }
        position.version = POSITION_VERSION;
        let floor = position.floor.get_or_insert_with(|| self.floor.clone()).clone();
        loop {
            if cancel.requested() {
                return Err(Failure::canceled("stopped during search read"));
            }
            let next = match position.reading.as_deref() {
                None => self.config.queries.iter().next(),
                Some(at) => self.config.queries.range::<str, _>((Bound::Included(at), Bound::Unbounded)).next(),
            };
            let Some((name, text)) = next.map(|(n, t)| (n.clone(), t.clone())) else {
                position.reading = None;
                return self.batch(Vec::new(), &mut position, false);
            };
            let mut held = position.queries.get(&name).cloned().unwrap_or_default();
            let mut pass = held.pass.clone().unwrap_or_else(|| Pass { since: held.watermark.clone().unwrap_or_else(|| floor.clone()), cursor: None, high: None, landed: 0 });
            let page = match self.page(&text, &pass) {
                Ok(page) => page,
                Err(failure) if retryable(&failure) => {
                    self.audit.push(format!("search query `{name}` held at its committed position: {failure}"));
                    position.reading = self.after(&name);
                    if position.reading.is_none() {
                        return self.batch(Vec::new(), &mut position, false);
                    }
                    continue;
                }
                Err(failure) => return Err(failure),
            };
            let results = page.pointer(&self.config.results).and_then(Value::as_array).cloned().unwrap_or_default();
            let room = usize::try_from(self.config.max_results.saturating_sub(pass.landed)).unwrap_or(usize::MAX);
            let rows: Vec<Value> = results
                .into_iter()
                .take(room)
                .map(|result| {
                    let mut row = match result {
                        Value::Object(map) => map,
                        other => Map::from_iter([("value".to_string(), other)]),
                    };
                    row.insert(QUERY_COLUMN.into(), Value::String(name.clone()));
                    Value::Object(row)
                })
                .collect();
            let newest = rows.iter().filter_map(|r| r.get(&self.config.time).and_then(Value::as_str).and_then(|t| Instant::parse(t).ok())).max();
            pass.high = later(pass.high.take(), newest);
            pass.landed += rows.len() as u64;
            let next = page.pointer(&self.config.cursor).and_then(Value::as_str).filter(|c| !c.is_empty()).map(str::to_string);
            let more = match next {
                Some(cursor) if !rows.is_empty() && pass.landed < self.config.max_results => {
                    pass.cursor = Some(cursor);
                    held.pass = Some(pass);
                    position.reading = Some(name.clone());
                    true
                }
                _ => {
                    held.watermark = later(held.watermark.take(), pass.high.as_deref().and_then(|t| Instant::parse(t).ok()));
                    held.pass = None;
                    position.reading = self.after(&name);
                    position.reading.is_some()
                }
            };
            position.queries.insert(name, held);
            return self.batch(rows, &mut position, more);
        }
    }
}
