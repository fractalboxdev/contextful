//! The generic HTTP source: one endpoint, one decoder, one pagination shape, and headers
//! whose values are templates hydrated per read (`connector.source.http-headers`).

use crate::decode::{decode, Format};
use contextful_core::connector::attach::{endpoint, scrub, Allowlist};
use contextful_core::connector::reference::{check_material, Template};
use contextful_core::connector::ConnectorError;
use contextful_core::run::ports::{Cancellation, PullRequest, Row, Source};
use contextful_core::run::{Failure, FailureTag, RunError};
use contextful_runtime::client::{classify, Client, HeaderValue};
use contextful_runtime::Resolver;
use serde_json::{Map, Value};
use std::collections::BTreeMap;
use std::sync::Arc;
use url::Url;

/// The source's registered name.
pub const NAME: &str = "http";
/// Requests one paginated walk issues: 1000 requests (`connector.source.page-cap`).
pub const PAGE_CAP: usize = 1000;

/// The configuration keys the HTTP source reads (`run.declare.config-key`).
pub const KEYS: [&str; 11] =
    ["endpoint", "format", "records", "headers", "page_param", "start_page", "next_cursor_path", "cursor_param", "next_url_path", "link_header", "since_param"];

/// How a walk finds its next page (`connector.source.pagination`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Pagination {
    /// One request.
    None,
    /// A page number parameter from a start page, ending on an empty page.
    Page { param: String, start: i64 },
    /// A cursor read at a JSON pointer, sent back in a parameter.
    NextCursor { path: String, param: String },
    /// A next URL read at a JSON pointer.
    NextUrl { path: String },
    /// The `Link` header's `rel="next"`.
    LinkHeader,
}

/// A parsed HTTP source configuration.
#[derive(Debug, Clone)]
pub struct HttpConfig {
    pub endpoint: String,
    pub format: Format,
    pub records: Option<String>,
    pub headers: BTreeMap<String, Template>,
    pub pagination: Pagination,
    pub since_param: Option<String>,
}

fn key_error(k: &str) -> RunError {
    RunError::PipelineUnknownConfigKey(format!("the `{NAME}` source reads no key `{k}`; it reads {}", KEYS.join(", ")))
}

fn text(cfg: &Map<String, Value>, k: &str) -> Result<Option<String>, RunError> {
    match cfg.get(k) {
        None => Ok(None),
        Some(Value::String(s)) => Ok(Some(s.clone())),
        Some(other) => Err(RunError::Invalid(format!("`{NAME}` source key `{k}` is a string, found {other}"))),
    }
}

/// Why a configuration refuses: the pipeline's rules, or the connector's.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConfigError {
    Run(RunError),
    Connector(ConnectorError),
}

impl std::fmt::Display for ConfigError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ConfigError::Run(e) => e.fmt(f),
            ConfigError::Connector(e) => e.fmt(f),
        }
    }
}

impl std::error::Error for ConfigError {}

impl From<RunError> for ConfigError {
    fn from(e: RunError) -> ConfigError {
        ConfigError::Run(e)
    }
}

impl From<ConnectorError> for ConfigError {
    fn from(e: ConnectorError) -> ConfigError {
        ConfigError::Connector(e)
    }
}

impl HttpConfig {
    /// Parse and check a source configuration before any I/O: unknown keys, the format's
    /// keys, one pagination shape, header templates, and the bound host.
    pub fn parse(config: &Value) -> Result<HttpConfig, ConfigError> {
        let cfg = config.as_object().ok_or_else(|| RunError::Invalid(format!("the `{NAME}` source config is an object")))?;
        // Header names are the vendor's vocabulary and are not held to the key set.
        if let Some(k) = cfg.keys().find(|k| !KEYS.contains(&k.as_str())) {
            return Err(key_error(k).into());
        }
        let endpoint_raw = text(cfg, "endpoint")?.ok_or_else(|| RunError::Invalid(format!("the `{NAME}` source names no `endpoint`")))?;
        let format = Format::parse(text(cfg, "format")?.as_deref().unwrap_or("json"))?;
        let records = text(cfg, "records")?;
        let next_cursor_path = text(cfg, "next_cursor_path")?;
        let next_url_path = text(cfg, "next_url_path")?;
        if format != Format::Json {
            for (k, v) in [("records", &records), ("next_cursor_path", &next_cursor_path), ("next_url_path", &next_url_path)] {
                if v.is_some() {
                    return Err(ConnectorError::ConnectorFormatKeyRejected(format!("`{k}` reads a JSON body and the source's format is `{}`", format.name())).into());
                }
            }
        }
        let page_param = text(cfg, "page_param")?;
        let link_header = cfg.get("link_header").and_then(Value::as_bool).unwrap_or(false);
        let declared: Vec<&str> = [
            page_param.as_ref().map(|_| "page_param"),
            next_cursor_path.as_ref().map(|_| "next_cursor_path"),
            next_url_path.as_ref().map(|_| "next_url_path"),
            link_header.then_some("link_header"),
        ]
        .into_iter()
        .flatten()
        .collect();
        if declared.len() > 1 {
            return Err(ConnectorError::ConnectorPaginationAmbiguous(format!("the source declares {}; a walk takes one pagination shape", declared.join(" and "))).into());
        }
        let pagination = match (page_param, next_cursor_path, next_url_path) {
            (Some(param), _, _) => Pagination::Page { param, start: cfg.get("start_page").and_then(Value::as_i64).unwrap_or(1) },
            (_, Some(path), _) => Pagination::NextCursor { path, param: text(cfg, "cursor_param")?.unwrap_or_else(|| "cursor".into()) },
            (_, _, Some(path)) => Pagination::NextUrl { path },
            _ if link_header => Pagination::LinkHeader,
            _ => Pagination::None,
        };
        let mut headers = BTreeMap::new();
        if let Some(h) = cfg.get("headers") {
            let h = h.as_object().ok_or_else(|| RunError::Invalid("`headers` is a table of header name to value template".into()))?;
            for (name, v) in h {
                let v = v.as_str().ok_or_else(|| RunError::Invalid(format!("header `{name}` is a string")))?;
                headers.insert(name.clone(), check_material(&format!("headers.{name}"), v)?);
            }
        }
        let parsed = endpoint(NAME, &endpoint_raw.replace("{table}", "table"))?;
        if let Some(open) = endpoint_raw.find('{') {
            let close = endpoint_raw[open..].find('}').map(|c| open + c).unwrap_or(endpoint_raw.len());
            let placeholder = &endpoint_raw[open..=close.min(endpoint_raw.len() - 1)];
            if placeholder != "{table}" {
                return Err(ConnectorError::ConnectorPlaceholderUnbound(format!("`{placeholder}` in `{}` has no table pattern to bind it", scrub(&parsed))).into());
            }
        }
        let c = HttpConfig { endpoint: endpoint_raw, format, records, headers, pagination, since_param: text(cfg, "since_param")? };
        c.allowlist()?;
        Ok(c)
    }

    /// The hosts this source reaches: its endpoint's host. A source binding a credential
    /// holds to one exact host.
    pub fn allowlist(&self) -> Result<Allowlist, ConnectorError> {
        let url = endpoint(NAME, &self.endpoint.replace("{table}", "table"))?;
        let allow = Allowlist::parse(&[url.host_str().unwrap_or_default()])?;
        if self.headers.values().any(Template::has_reference) {
            allow.check_bound()?;
        }
        Ok(allow)
    }

    /// The endpoint for `table`, the table name percent-encoded into a `{table}` segment.
    pub fn table_url(&self, table: &str) -> Result<Url, ConnectorError> {
        let encoded: String = table
            .bytes()
            .map(|b| if b.is_ascii_alphanumeric() || b"-_.~".contains(&b) { (b as char).to_string() } else { format!("%{b:02X}") })
            .collect();
        endpoint(NAME, &self.endpoint.replace("{table}", &encoded))
    }
}

/// One HTTP source bound to a table, its resolver and its mediated client.
pub struct HttpSource {
    pub config: HttpConfig,
    pub table: String,
    pub resolver: Arc<Resolver>,
    pub client: Arc<Client>,
}

impl HttpSource {
    pub fn new(config: HttpConfig, table: &str, resolver: Arc<Resolver>) -> Result<HttpSource, ConnectorError> {
        let origin = config.table_url(table)?;
        let client = Arc::new(Client::new(config.allowlist()?, origin));
        Ok(HttpSource { config, table: table.to_string(), resolver, client })
    }

    /// Hydrate every header just in time, a template holding a reference as sensitive material.
    fn headers(&self, idempotency_key: &str) -> Result<Vec<(String, HeaderValue)>, Failure> {
        let mut out = vec![("Idempotency-Key".to_string(), HeaderValue::Plain(idempotency_key.to_string()))];
        for (name, t) in &self.config.headers {
            let v = self.resolver.render(t)?;
            out.push((name.clone(), if t.has_reference() { HeaderValue::Sensitive(v) } else { HeaderValue::Plain(v.reveal().to_string()) }));
        }
        Ok(out)
    }

    /// Walk every page from `position`, returning every record fetched.
    pub fn walk(&self, request: &PullRequest, cancel: &dyn Cancellation) -> Result<Vec<Row>, Failure> {
        let mut url = self.config.table_url(&self.table).map_err(|e| Failure::deterministic(FailureTag::Config, e.to_string()))?;
        if let (Some(param), Some(at)) = (&self.config.since_param, request.position.as_ref().and_then(|p| p.get("at"))) {
            let v = match at {
                Value::String(s) => s.clone(),
                other => other.to_string(),
            };
            url.query_pairs_mut().append_pair(param, &v);
        }
        let headers = self.headers(&request.idempotency_key)?;
        let mut rows = Vec::new();
        let mut seen: Vec<String> = Vec::new();
        let mut page = match &self.config.pagination {
            Pagination::Page { start, .. } => *start,
            _ => 0,
        };
        let mut next = Some(url.clone());
        for _ in 0..PAGE_CAP {
            let Some(mut current) = next.take() else { return Ok(rows) };
            if cancel.requested() {
                return Err(Failure::canceled("stopped during the page walk"));
            }
            if let Pagination::Page { param, .. } = &self.config.pagination {
                current.query_pairs_mut().append_pair(param, &page.to_string());
            }
            let resp = self.client.send("GET", &current, &headers, None)?;
            if !(200..300).contains(&resp.status) {
                let retry_after = resp.header("retry-after").and_then(|v| v.trim().parse().ok());
                return Err(classify(resp.status, retry_after, &scrub(&resp.url)));
            }
            let (batch, body) = decode(self.config.format, &resp.body, self.config.records.as_deref(), &scrub(&resp.url))?;
            let empty = batch.is_empty();
            rows.extend(batch);
            let token = match &self.config.pagination {
                Pagination::None => None,
                Pagination::Page { .. } => {
                    page += 1;
                    (!empty).then(|| page.to_string())
                }
                Pagination::NextCursor { path, .. } => body.as_ref().and_then(|b| b.pointer(path)).and_then(scalar),
                Pagination::NextUrl { path } => body.as_ref().and_then(|b| b.pointer(path)).and_then(scalar),
                Pagination::LinkHeader => resp.header("link").and_then(next_link),
            };
            let Some(token) = token else { return Ok(rows) };
            if seen.contains(&token) {
                return Err(Failure::deterministic(
                    FailureTag::Permanent,
                    ConnectorError::ConnectorPageLoop(format!("`{}` served a page token it served before", scrub(&resp.url))).to_string(),
                ));
            }
            seen.push(token.clone());
            next = Some(match &self.config.pagination {
                Pagination::NextCursor { param, .. } => {
                    let mut u = url.clone();
                    u.query_pairs_mut().append_pair(param, &token);
                    u
                }
                Pagination::NextUrl { .. } | Pagination::LinkHeader => resp.url.join(&token).map_err(|e| Failure::new(FailureTag::Permanent, format!("a next link names no URL: {e}")))?,
                _ => url.clone(),
            });
        }
        Err(Failure::new(FailureTag::Permanent, format!("the walk over `{}` reached {PAGE_CAP} requests", scrub(&url))))
    }
}

fn scalar(v: &Value) -> Option<String> {
    match v {
        Value::String(s) if !s.is_empty() => Some(s.clone()),
        Value::Number(n) => Some(n.to_string()),
        _ => None,
    }
}

/// The `rel="next"` target of a `Link` header.
fn next_link(header: &str) -> Option<String> {
    header.split(',').find(|part| part.contains("rel=\"next\"") || part.contains("rel=next")).and_then(|part| {
        let start = part.find('<')? + 1;
        let end = part[start..].find('>')? + start;
        Some(part[start..end].to_string())
    })
}

impl Source for HttpSource {
    /// One pull is one whole walk; the engine derives the position from the rows.
    fn pull(&mut self, request: &PullRequest, cancel: &dyn Cancellation) -> Result<Vec<u8>, Failure> {
        let rows = self.walk(request, cancel)?;
        serde_json::to_vec(&serde_json::json!({ "rows": rows, "more": false })).map_err(|e| Failure::new(FailureTag::Permanent, e.to_string()))
    }
}
