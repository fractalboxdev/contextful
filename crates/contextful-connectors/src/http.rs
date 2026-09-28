//! The generic HTTP source: one endpoint, one decoder, one pagination shape, and headers
//! whose values are templates hydrated per read (`connector.source.http-headers`).

use crate::decode::{decode, workbook, Format};
use contextful_core::connector::attach::{endpoint, scrub, Allowlist};
use contextful_core::connector::reference::{check_material, Template};
use contextful_core::connector::ConnectorError;
use contextful_core::run::ports::{Cancellation, PullRequest, Row, Source};
use contextful_core::run::{Failure, FailureTag, RunError};
use contextful_outbound::client::{classify, Client, HeaderValue};
use contextful_outbound::Resolver;
use serde_json::{Map, Value};
use std::collections::BTreeMap;
use std::sync::Arc;
use url::Url;

/// The source's registered name.
pub const NAME: &str = "http";
/// Requests one paginated walk issues: 1000 requests (`connector.source.page-cap`).
pub const PAGE_CAP: usize = 1000;

/// The configuration keys the HTTP source reads (`run.declare.config-key`).
pub const KEYS: [&str; 14] = [
    "endpoint",
    "table_pattern",
    "format",
    "records",
    "headers",
    "page_param",
    "start_page",
    "next_cursor_path",
    "cursor_param",
    "next_url_path",
    "link_header",
    "since_param",
    "sheet",
    "skip_rows",
];

/// One `/`-separated segment of a table pattern.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Segment {
    /// Matches the table's segment exactly and binds nothing.
    Literal(String),
    /// Binds the table's non-empty segment under this name.
    Field(String),
}

/// A parsed `table_pattern`, such as `{stream}/{owner}/{repo}` (`connector.source.table-pattern`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TablePattern {
    segments: Vec<Segment>,
}

impl TablePattern {
    /// Parse a pattern: each segment is a literal or one `{name}`, no name repeats, and no
    /// field is `table`, the placeholder reserved for the whole name.
    pub fn parse(pattern: &str) -> Result<TablePattern, RunError> {
        let invalid = |why: String| RunError::Invalid(format!("`{NAME}` source `table_pattern` {why}"));
        if pattern.is_empty() {
            return Err(invalid("is empty".into()));
        }
        let mut segments = Vec::new();
        for raw in pattern.split('/') {
            let segment = match raw.strip_prefix('{').and_then(|s| s.strip_suffix('}')) {
                Some(name) if name.is_empty() || name.contains(['{', '}']) => return Err(invalid(format!("segment `{raw}` is not one `{{name}}`"))),
                Some("table") => return Err(invalid("names `{table}`, which always binds the whole table name".into())),
                Some(name) if segments.contains(&Segment::Field(name.to_string())) => return Err(invalid(format!("names `{{{name}}}` twice"))),
                Some(name) => Segment::Field(name.to_string()),
                None if raw.contains(['{', '}']) => return Err(invalid(format!("segment `{raw}` mixes literal text with a placeholder"))),
                None => Segment::Literal(raw.to_string()),
            };
            segments.push(segment);
        }
        Ok(TablePattern { segments })
    }

    /// Whether the pattern binds `{name}`.
    fn binds(&self, name: &str) -> bool {
        self.fields().any(|f| f == name)
    }

    /// The names the pattern binds, in segment order.
    fn fields(&self) -> impl Iterator<Item = &str> {
        self.segments.iter().filter_map(|s| match s {
            Segment::Field(f) => Some(f.as_str()),
            Segment::Literal(_) => None,
        })
    }

    /// A table name the pattern matches, each field bound to its own name.
    fn exemplar(&self) -> String {
        self.segments
            .iter()
            .map(|s| match s {
                Segment::Literal(l) | Segment::Field(l) => l.as_str(),
            })
            .collect::<Vec<_>>()
            .join("/")
    }

    /// Bind `table`'s segments by name. A table with another segment count, a differing
    /// literal, or an empty, `.` or `..` field raises `ConnectorTableUnmatched` rather than
    /// falling back to an unbound URL or a folded dot segment (`connector.source.table-unmatched`).
    pub fn bind(&self, table: &str) -> Result<BTreeMap<String, String>, ConnectorError> {
        let parts: Vec<&str> = table.split('/').collect();
        let unmatched = || ConnectorError::ConnectorTableUnmatched(format!("table `{table}` does not match the source's `table_pattern = \"{self}\"`"));
        if parts.len() != self.segments.len() {
            return Err(unmatched());
        }
        let mut bound = BTreeMap::new();
        for (segment, part) in self.segments.iter().zip(parts) {
            match segment {
                Segment::Literal(l) if l == part => {}
                Segment::Field(name) if !part.is_empty() && !is_dot_segment(part) => {
                    bound.insert(name.clone(), part.to_string());
                }
                _ => return Err(unmatched()),
            }
        }
        Ok(bound)
    }
}

impl std::fmt::Display for TablePattern {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let parts: Vec<String> = self
            .segments
            .iter()
            .map(|s| match s {
                Segment::Literal(l) => l.clone(),
                Segment::Field(n) => format!("{{{n}}}"),
            })
            .collect();
        f.write_str(&parts.join("/"))
    }
}

/// Whether `value` is `.` or `..`, which a URL parser folds out of a path as a dot segment.
fn is_dot_segment(value: &str) -> bool {
    matches!(value, "." | "..")
}

/// Percent-encode every byte outside RFC 3986's unreserved set, so a bound value other than
/// `.` or `..` holds its position in a path or a query and adds no segment or query.
fn percent_encode(value: &str) -> String {
    value.bytes().map(|b| if b.is_ascii_alphanumeric() || b"-_.~".contains(&b) { (b as char).to_string() } else { format!("%{b:02X}") }).collect()
}

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
    /// Binds table-name segments into the endpoint; absent, `{table}` binds the whole name.
    pub table_pattern: Option<TablePattern>,
    pub format: Format,
    pub records: Option<String>,
    pub headers: BTreeMap<String, Template>,
    pub pagination: Pagination,
    pub since_param: Option<String>,
    /// The worksheet a workbook body lands; the first sheet when unset.
    pub sheet: Option<String>,
    /// Sheet rows 1 through `skip_rows`, by row number, ahead of a workbook's header row.
    pub skip_rows: usize,
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
        let sheet = text(cfg, "sheet")?;
        let skip_rows = match cfg.get("skip_rows") {
            None => None,
            Some(v) => Some(v.as_u64().ok_or_else(|| RunError::Invalid(format!("`{NAME}` source key `skip_rows` is a non-negative integer, found {v}")))? as usize),
        };
        if format != Format::Workbook {
            if let Some(k) = [sheet.as_ref().map(|_| "sheet"), skip_rows.map(|_| "skip_rows")].into_iter().flatten().next() {
                return Err(ConnectorError::ConnectorFormatKeyRejected(format!("`{k}` reads a workbook and the source's format is `{}`", format.name())).into());
            }
        }
        let since_param = text(cfg, "since_param")?;
        if format == Format::Workbook && since_param.is_some() {
            return Err(incremental("`since_param` declares an incremental position").into());
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
        // A workbook is one document; no page of it follows another.
        if let (Format::Workbook, Some(k)) = (format, declared.first()) {
            return Err(ConnectorError::ConnectorFormatKeyRejected(format!("`{k}` walks pages and the source's format is `xlsx`, one document per read")).into());
        }
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
        let table_pattern = text(cfg, "table_pattern")?.as_deref().map(TablePattern::parse).transpose()?;
        // Every placeholder is `{table}` or a field of the table pattern; any other has
        // nothing to bind it.
        let mut rest = endpoint_raw.as_str();
        while let Some(open) = rest.find('{') {
            let placeholder = match rest[open..].find('}') {
                Some(close) => &rest[open..=open + close],
                None => &rest[open..],
            };
            let name = placeholder.strip_prefix('{').and_then(|p| p.strip_suffix('}'));
            if !name.is_some_and(|n| n == "table" || table_pattern.as_ref().is_some_and(|p| p.binds(n))) {
                let binds = match &table_pattern {
                    Some(p) => format!("`{{table}}` and the fields of `table_pattern = \"{p}\"` are the placeholders"),
                    None => "`{table}` is the one placeholder without a `table_pattern`".to_string(),
                };
                return Err(ConnectorError::ConnectorPlaceholderUnbound(format!("the endpoint's placeholder `{placeholder}` has no table pattern to bind it; {binds}")).into());
            }
            rest = &rest[open + placeholder.len()..];
        }
        // A field the endpoint never places leaves every table matching the pattern on one URL.
        if let Some(p) = &table_pattern {
            if let Some(unused) = p.fields().find(|f| !endpoint_raw.contains(&format!("{{{f}}}"))) {
                return Err(RunError::Invalid(format!("`{NAME}` source `table_pattern = \"{p}\"` names `{{{unused}}}`, which the endpoint never places")).into());
            }
        }
        // The scheme and authority end at the first `/`, `?` or `#` past `://`; a placeholder
        // there lets a table name choose where a bound credential goes (`connector.attach.bound-host`).
        let authority_end = endpoint_raw.find("://").map_or(endpoint_raw.len(), |s| endpoint_raw[s + 3..].find(['/', '?', '#']).map_or(endpoint_raw.len(), |e| s + 3 + e));
        if headers.values().any(Template::has_reference) && endpoint_raw[..authority_end].contains('{') {
            return Err(ConnectorError::SecretWildcardHost(
                "the endpoint binds a table name into its scheme or host beside a bound credential; a credential attaches to one exact host, and a table binds only the path and query".into(),
            )
            .into());
        }
        let c = HttpConfig { endpoint: endpoint_raw, table_pattern, format, records, headers, pagination, since_param, sheet, skip_rows: skip_rows.unwrap_or(0) };
        let exemplar = c.table_pattern.as_ref().map_or_else(|| "table".to_string(), TablePattern::exemplar);
        c.table_url(&exemplar)?;
        Ok(c)
    }

    /// Whether a pipeline may declare an incremental cursor over this source: a workbook
    /// refuses at build, ahead of the run that would commit its first position
    /// (`connector.source.workbook-incremental`).
    pub fn accepts_incremental(&self) -> Result<(), ConnectorError> {
        match self.format {
            Format::Workbook => Err(incremental("the pipeline declares `incremental`")),
            _ => Ok(()),
        }
    }

    /// The hosts this source reaches for `table`: its endpoint's host with the table bound.
    /// A source binding a credential holds to one exact host.
    pub fn allowlist(&self, table: &str) -> Result<Allowlist, ConnectorError> {
        let url = self.table_url(table)?;
        let allow = Allowlist::parse(&[url.host_str().unwrap_or_default()])?;
        if self.headers.values().any(Template::has_reference) {
            allow.check_bound()?;
        }
        Ok(allow)
    }

    /// The endpoint for `table`: `{table}` binds the whole name and each pattern field its
    /// segment, every value percent-encoded, in one pass so a bound value never expands. A
    /// `.` or `..` table raises `ConnectorTableUnmatched` (`connector.source.table-unmatched`).
    pub fn table_url(&self, table: &str) -> Result<Url, ConnectorError> {
        let mut values = match &self.table_pattern {
            Some(p) => p.bind(table)?,
            None => BTreeMap::new(),
        };
        if is_dot_segment(table) && self.endpoint.contains("{table}") {
            return Err(ConnectorError::ConnectorTableUnmatched(format!("table `{table}` binds a dot segment into the endpoint's `{{table}}`")));
        }
        values.insert("table".to_string(), table.to_string());
        let mut out = String::with_capacity(self.endpoint.len());
        let mut rest = self.endpoint.as_str();
        while let Some((open, close)) = rest.find('{').and_then(|o| rest[o..].find('}').map(|c| (o, o + c))) {
            out.push_str(&rest[..open]);
            match values.get(&rest[open + 1..close]) {
                Some(v) => out.push_str(&percent_encode(v)),
                None => out.push_str(&rest[open..=close]),
            }
            rest = &rest[close + 1..];
        }
        out.push_str(rest);
        endpoint(NAME, &out)
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
        let client = Arc::new(Client::new(config.allowlist(table)?, origin));
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
        if self.config.format == Format::Workbook && request.position.is_some() {
            return Err(Failure::deterministic(FailureTag::Config, incremental("the read carries a stored position").to_string()));
        }
        if let (Some(param), Some(at)) = (&self.config.since_param, request.position.as_ref().and_then(|p| p.get("at"))) {
            let v = match at {
                Value::String(s) => s.clone(),
                other => other.to_string(),
            };
            url.query_pairs_mut().append_pair(param, &v);
        }
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
            // Hydrated per request: the resolver's cache retires a lease ahead of its expiry,
            // so a long walk re-hydrates rather than sending an expired credential.
            let headers = self.headers(&request.idempotency_key)?;
            let resp = self.client.send("GET", &current, &headers, None)?;
            if !(200..300).contains(&resp.status) {
                let retry_after = resp.header("retry-after").and_then(|v| v.trim().parse().ok());
                return Err(classify(resp.status, retry_after, &scrub(&resp.url)));
            }
            let (batch, body) = match self.config.format {
                Format::Workbook => (workbook::rows(&resp.body, self.config.sheet.as_deref(), self.config.skip_rows, &scrub(&resp.url))?, None),
                format => decode(format, &resp.body, self.config.records.as_deref(), &scrub(&resp.url))?,
            };
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

/// A workbook holds no incremental position (`connector.source.workbook-incremental`).
fn incremental(why: &str) -> ConnectorError {
    ConnectorError::ConnectorIncrementalUnsupported(format!(
        "{why} and the source's format is `xlsx`; a date lands as a variable-width serial number, which no text watermark orders"
    ))
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
