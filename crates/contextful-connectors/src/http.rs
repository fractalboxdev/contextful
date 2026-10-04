//! The generic HTTP source: one endpoint, one decoder, one pagination shape, and headers
//! whose values are templates hydrated per read (`connector.source.http-headers`).

use crate::decode::{decode_with_encoding, workbook, Format};
use contextful_core::connector::attach::{endpoint, scrub, scrub_text, Allowlist};
use contextful_core::connector::meter::LimiterDeclaration;
use contextful_core::connector::probe::ScopeProbe;
use contextful_core::connector::reference::{check_material, Template};
use contextful_core::connector::ConnectorError;
use contextful_core::run::ports::{Cancellation, PullRequest, Row, Source};
use contextful_core::run::advance::{admits, clock};
use contextful_core::run::{Failure, FailureTag, RunError};
use contextful_outbound::client::{classify, Client, HeaderValue};
use contextful_outbound::probe::probe_through;
use contextful_outbound::{Limiter, Meter, PreSendHook, Resolver, Transport};
use serde_json::{Map, Value};
use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::{Duration, Instant};
use url::Url;

/// The source's registered name.
pub const NAME: &str = "http";
/// Requests one paginated walk issues: 1000 requests (`connector.source.page-cap`).
pub const PAGE_CAP: usize = 1000;

/// The configuration keys the HTTP source reads (`run.declare.config-key`).
pub const KEYS: [&str; 20] = [
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
    "limiter",
    "scope_probe",
    "bound_columns",
    "conditional",
    "expansion",
    "encoding",
];

/// A detail document address and the column receiving its decoded JSON value.
#[derive(Debug, Clone)]
pub struct Expansion {
    pointer: ExpansionPointer,
    target_column: String,
}

#[derive(Debug, Clone)]
enum ExpansionPointer {
    Template(String),
    Column(String),
}

const EXPANSION_REQUESTS: usize = 200;
const EXPANSION_BYTES: usize = 256 * 1024 * 1024;
const EXPANSION_TIME: Duration = Duration::from_secs(600);

/// One expansion read's budget, started before its first index request.
struct ExpansionBudget {
    started: Instant,
    bytes: usize,
    max_bytes: usize,
    max_time: Duration,
}

impl ExpansionBudget {
    fn new(max_bytes: usize, max_time: Duration) -> Self {
        Self { started: Instant::now(), bytes: 0, max_bytes, max_time }
    }

    fn preflight_requests(&self, count: usize) -> Result<(), Failure> {
        if count > EXPANSION_REQUESTS {
            return Err(Failure::deterministic(FailureTag::Permanent, format!("one expanding read admits at most {EXPANSION_REQUESTS} follow-up requests")));
        }
        Ok(())
    }

    fn check_time(&self) -> Result<(), Failure> {
        if self.started.elapsed() >= self.max_time {
            return Err(Failure::new(FailureTag::Permanent, "the expansion exceeded 600 s"));
        }
        Ok(())
    }

    fn available(&self) -> Result<(u64, Duration), Failure> {
        self.check_time()?;
        let bytes = self.max_bytes.saturating_sub(self.bytes);
        if bytes == 0 {
            return Err(Failure::new(FailureTag::Permanent, "the expansion exceeded 256 MiB"));
        }
        Ok((bytes as u64, self.max_time.saturating_sub(self.started.elapsed())))
    }

    fn charge(&mut self, bytes: usize) -> Result<(), Failure> {
        self.bytes = self.bytes.checked_add(bytes).filter(|total| *total <= self.max_bytes)
            .ok_or_else(|| Failure::new(FailureTag::Permanent, "the expansion exceeded 256 MiB"))?;
        self.check_time()
    }
}

#[cfg(test)]
mod expansion_budget_tests {
    use super::*;

    // spec: connector.source.expansion-budget@75af8328
    #[test]
    fn an_expanding_read_counts_index_detail_and_elapsed_budgets() {
        let mut budget = ExpansionBudget::new(7, EXPANSION_TIME);
        budget.preflight_requests(200).unwrap();
        assert!(budget.preflight_requests(201).is_err());
        budget.charge(4).unwrap(); // Index body.
        assert_eq!(budget.available().unwrap().0, 3);
        budget.charge(3).unwrap(); // Detail body.
        assert!(budget.available().is_err(), "another response cannot start beyond the total byte cap");

        let mut elapsed = ExpansionBudget::new(7, EXPANSION_TIME);
        elapsed.started = Instant::now() - Duration::from_secs(601);
        assert!(elapsed.available().is_err(), "a completed index walk spends the same read deadline");
    }
}

fn expansion(value: &Value) -> Result<Expansion, ConfigError> {
    let block = value.as_object().ok_or_else(|| RunError::Invalid("`expansion` is a table".into()))?;
    if let Some(key) = block.keys().find(|key| !["url_template", "pointer_column", "target_column"].contains(&key.as_str())) {
        return Err(RunError::PipelineUnknownConfigKey(format!("the `expansion` block reads no key `{key}`")).into());
    }
    let template = text(block, "url_template")?;
    let column = text(block, "pointer_column")?;
    let target_column = text(block, "target_column")?.filter(|s| !s.is_empty()).ok_or_else(|| RunError::Invalid("`expansion.target_column` is a non-empty string".into()))?;
    let pointer = match (template, column) {
        (Some(_), Some(_)) => return Err(ConnectorError::ConnectorPointerAmbiguous("`url_template` and `pointer_column` both name the follow-up".into()).into()),
        (None, None) => return Err(RunError::Invalid("`expansion` names `url_template` or `pointer_column`".into()).into()),
        (None, Some(name)) if !name.is_empty() => ExpansionPointer::Column(name),
        (None, Some(_)) => return Err(RunError::Invalid("`expansion.pointer_column` is non-empty".into()).into()),
        (Some(template), None) => {
            let authority_start = if template.starts_with("//") { Some(2) } else { template.find("://").map(|s| s + 3) };
            let binds_authority = authority_start.is_some_and(|start| {
                let end = template[start..].find(['/', '?', '#']).map_or(template.len(), |e| start + e);
                template[..end].contains('{')
            });
            if !template.contains('{') || binds_authority {
                return Err(ConnectorError::ConnectorTemplateRejected("`url_template` needs a row placeholder in its path or query, outside the URL authority".into()).into());
            }
            let mut rest = template.as_str();
            while let Some(open) = rest.find('{') {
                if rest[..open].contains('}') {
                    return Err(ConnectorError::ConnectorTemplateRejected("`url_template` has an unmatched closing brace".into()).into());
                }
                let Some(close) = rest[open + 1..].find('}').map(|i| open + 1 + i) else {
                    return Err(ConnectorError::ConnectorTemplateRejected("`url_template` has an unclosed placeholder".into()).into());
                };
                if close == open + 1 || rest[open + 1..close].contains('{') {
                    return Err(ConnectorError::ConnectorTemplateRejected("`url_template` has an empty or nested placeholder".into()).into());
                }
                rest = &rest[close + 1..];
            }
            if rest.contains('}') {
                return Err(ConnectorError::ConnectorTemplateRejected("`url_template` has an unmatched closing brace".into()).into());
            }
            ExpansionPointer::Template(template)
        }
    };
    Ok(Expansion { pointer, target_column })
}

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

/// `template` with each `{name}` in `values` replaced by `encode` of its value, in one pass
/// so a bound value never expands; a placeholder outside `values` stays as written.
fn fill(template: &str, values: &BTreeMap<String, String>, encode: impl Fn(&str) -> String) -> String {
    let mut out = String::with_capacity(template.len());
    let mut rest = template;
    while let Some((open, close)) = rest.find('{').and_then(|o| rest[o..].find('}').map(|c| (o, o + c))) {
        out.push_str(&rest[..open]);
        match values.get(&rest[open + 1..close]) {
            Some(v) => out.push_str(&encode(v)),
            None => out.push_str(&rest[open..=close]),
        }
        rest = &rest[close + 1..];
    }
    out.push_str(rest);
    out
}

/// Hold every placeholder of `text` to `{table}` or a field of `pattern`; any other has
/// nothing to bind it (`connector.source.placeholder-unbound`).
fn check_placeholders(what: &str, text: &str, pattern: Option<&TablePattern>) -> Result<(), ConnectorError> {
    let mut rest = text;
    while let Some(open) = rest.find('{') {
        let placeholder = match rest[open..].find('}') {
            Some(close) => &rest[open..=open + close],
            None => &rest[open..],
        };
        let name = placeholder.strip_prefix('{').and_then(|p| p.strip_suffix('}'));
        if !name.is_some_and(|n| n == "table" || pattern.is_some_and(|p| p.binds(n))) {
            let binds = match pattern {
                Some(p) => format!("`{{table}}` and the fields of `table_pattern = \"{p}\"` are the placeholders"),
                None => "`{table}` is the one placeholder without a `table_pattern`".to_string(),
            };
            return Err(ConnectorError::ConnectorPlaceholderUnbound(format!("{what}'s placeholder `{placeholder}` has no table pattern to bind it; {binds}")));
        }
        rest = &rest[open + placeholder.len()..];
    }
    Ok(())
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
    /// The shared vendor quota every request reserves against (`connector.meter.limiter-declaration`).
    pub limiter: Option<LimiterDeclaration>,
    /// The identity endpoint called with the bound credential ahead of the first page.
    pub scope_probe: Option<ScopeProbe>,
    /// Column name to a template over `{table}` and the pattern's fields, stamped onto
    /// every fetched row (`connector.source.bound-columns`).
    pub bound_columns: BTreeMap<String, String>,
    /// Whether a read sends the stored validators and keeps a `304` as no change
    /// (`connector.source.conditional-get`).
    pub conditional: bool,
    /// The row pointer followed after the index read and the column receiving its value.
    pub expansion: Option<Expansion>,
    /// The character encoding of a CSV body; absent means UTF-8.
    pub encoding: Option<String>,
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
        let encoding = text(cfg, "encoding")?;
        if encoding.is_some() && format != Format::Csv {
            return Err(ConnectorError::ConnectorFormatKeyRejected("`encoding` reads delimited text and the source format is not `csv`".into()).into());
        }
        if let Some(label) = &encoding {
            if encoding_rs::Encoding::for_label(label.as_bytes()).is_none() {
                return Err(ConnectorError::ConnectorEncodingInvalid(format!("unknown encoding `{label}`")).into());
            }
        }
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
        check_placeholders("the endpoint", &endpoint_raw, table_pattern.as_ref())?;
        let mut bound_columns = BTreeMap::new();
        if let Some(b) = cfg.get("bound_columns") {
            let b = b.as_object().ok_or_else(|| RunError::Invalid("`bound_columns` is a table of column name to value template".into()))?;
            for (column, v) in b {
                let v = v.as_str().ok_or_else(|| RunError::Invalid(format!("bound column `{column}` is a template string, found {v}")))?;
                check_placeholders(&format!("bound column `{column}`"), v, table_pattern.as_ref())?;
                bound_columns.insert(column.clone(), v.to_string());
            }
        }
        let conditional = match cfg.get("conditional") {
            None => false,
            Some(v) => v.as_bool().ok_or_else(|| RunError::Invalid(format!("`{NAME}` source key `conditional` is a boolean, found {v}")))?,
        };
        let expansion = cfg.get("expansion").map(expansion).transpose()?;
        if let (true, Some(k)) = (conditional, declared.first()) {
            return Err(ConnectorError::ConnectorConditionalRejected(format!("`conditional` beside `{k}`: a validator names one document, and a page walk reads many")).into());
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
        let limiter = cfg.get("limiter").map(limiter_declaration).transpose()?;
        let scope_probe = cfg.get("scope_probe").map(scope_probe).transpose()?;
        let c = HttpConfig {
            endpoint: endpoint_raw,
            table_pattern,
            format,
            records,
            headers,
            pagination,
            since_param,
            sheet,
            skip_rows: skip_rows.unwrap_or(0),
            limiter,
            scope_probe,
            bound_columns,
            conditional,
            expansion,
            encoding,
        };
        if c.scope_probe.is_some() && c.probe_credential().is_none() {
            return Err(RunError::Invalid(format!("the `{NAME}` source's `scope_probe` carries a bound credential, and no `headers` template binds one")).into());
        }
        let exemplar = c.table_pattern.as_ref().map_or_else(|| "table".to_string(), TablePattern::exemplar);
        c.table_url(&exemplar)?;
        Ok(c)
    }

    /// The header whose bound credential the scope probe carries: the first holding a reference.
    pub fn probe_credential(&self) -> Option<&str> {
        self.headers.iter().find(|(_, t)| t.has_reference()).map(|(n, _)| n.as_str())
    }

    /// Whether a pipeline may declare an incremental cursor over this source: a workbook
    /// refuses at build, ahead of the run that would commit its first position
    /// (`connector.source.workbook-incremental`).
    pub fn accepts_incremental(&self) -> Result<(), ConnectorError> {
        if self.conditional {
            return Err(ConnectorError::ConnectorConditionalRejected(
                "`conditional` beside a declared `incremental`: the position holds the watermark, leaving the validators no place to commit".into(),
            ));
        }
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
        endpoint(NAME, &fill(&self.endpoint, &values, percent_encode))
    }

    /// The bound columns' values for `table`, each template filled with its values as
    /// written (`connector.source.bound-columns`).
    pub fn bound_values(&self, table: &str) -> Result<Vec<(String, String)>, ConnectorError> {
        if self.bound_columns.is_empty() {
            return Ok(Vec::new());
        }
        let mut values = match &self.table_pattern {
            Some(p) => p.bind(table)?,
            None => BTreeMap::new(),
        };
        values.insert("table".to_string(), table.to_string());
        Ok(self.bound_columns.iter().map(|(c, t)| (c.clone(), fill(t, &values, str::to_string))).collect())
    }
}

/// What a source's outbound requests pass besides the allowlist: the limiter bound to its
/// declared quota, the operator's pre-send hook, the transport, and the run id each intent
/// carries (`connector.meter.pre-send-hook`).
#[derive(Clone, Default)]
pub struct Mediation {
    /// The limiter bound to the source's declared quota. A declaration with none bound
    /// refuses every request as `ConnectorUnmetered` (`connector.meter.unmetered-request`).
    pub limiter: Option<Arc<Limiter>>,
    /// The operator's hook, composed in front of the reservation (`connector.meter.hook-composition`).
    pub hook: Option<Arc<dyn PreSendHook>>,
    /// The network port; the system transport when unset.
    pub transport: Option<Arc<dyn Transport>>,
    pub run_id: Option<String>,
}

impl Mediation {
    /// A client reaching `origin` under `allow`, metered against `declared` when the source
    /// declares a quota (`connector.meter.reservation-point`).
    fn client(&self, allow: Allowlist, origin: Url, declared: Option<&LimiterDeclaration>) -> Client {
        let mut client = Client::new(allow, origin);
        if let Some(t) = &self.transport {
            client = client.with_transport(t.clone());
        }
        if let Some(h) = &self.hook {
            client = client.with_hook(h.clone());
        }
        if let Some(run_id) = &self.run_id {
            client = client.for_run(run_id);
        }
        if let Some(d) = declared {
            client = client.metered(Meter::new(d.clone(), self.limiter.clone()));
        }
        client
    }
}

/// One HTTP source bound to a table, its resolver and its mediated client.
pub struct HttpSource {
    pub config: HttpConfig,
    pub table: String,
    pub resolver: Arc<Resolver>,
    pub client: Arc<Client>,
    allow: Allowlist,
    /// The client the scope probe takes: the page client's mediation, the probe endpoint's
    /// origin, and no body read.
    probe_client: Option<Client>,
    probed: bool,
    /// Whether a pull walks every page: under a monotonic cursor the position is a
    /// watermark, which carries no page token.
    watermarked: bool,
    clock_field: Option<String>,
    /// Tokens this walk served, for `ConnectorPageLoop`, and the requests it issued.
    seen: Vec<String>,
    requests: usize,
}

impl HttpSource {
    /// A source whose requests pass no hook and no limiter. A declared quota then refuses
    /// every request as `ConnectorUnmetered`.
    pub fn new(config: HttpConfig, table: &str, resolver: Arc<Resolver>) -> Result<HttpSource, ConnectorError> {
        HttpSource::mediated(config, table, resolver, Mediation::default())
    }

    /// A source whose every request, the scope probe's included, passes `mediation`.
    pub fn mediated(config: HttpConfig, table: &str, resolver: Arc<Resolver>, mediation: Mediation) -> Result<HttpSource, ConnectorError> {
        let origin = config.table_url(table)?;
        let allow = config.allowlist(table)?;
        let probe_client = match &config.scope_probe {
            Some(p) => {
                p.check_transport(&allow, config.probe_credential().unwrap_or_default())?;
                Some(mediation.client(allow.clone(), p.endpoint.clone(), config.limiter.as_ref()).without_body())
            }
            None => None,
        };
        let client = Arc::new(mediation.client(allow.clone(), origin, config.limiter.as_ref()));
        Ok(HttpSource { config, table: table.to_string(), resolver, client, allow, probe_client, probed: false, watermarked: false, clock_field: None, seen: Vec::new(), requests: 0 })
    }

    /// The source serving a monotonic cursor: one pull walks every page from the watermark.
    pub fn watermarked(mut self) -> HttpSource {
        self.watermarked = true;
        self
    }

    /// A source whose incremental field validates delimited clock spellings from its first read.
    pub fn watermarked_for(mut self, field: &str) -> HttpSource {
        self.watermarked = true;
        self.clock_field = Some(field.to_string());
        self
    }

    /// Whether one pull walks every page: under a watermark, and under next-URL or
    /// Link-header pagination, whose token is a whole URL whose query may carry a
    /// credential the journal would hold unscrubbed (`connector.source.http-url-walk`).
    fn walks_whole(&self) -> bool {
        self.watermarked || self.config.expansion.is_some() || matches!(self.config.pagination, Pagination::NextUrl { .. } | Pagination::LinkHeader)
    }

    /// Hydrate every header just in time, a template holding a reference as sensitive material.
    fn headers(&self, idempotency_key: &str) -> Result<Vec<(String, HeaderValue)>, Failure> {
        let mut out = vec![("Idempotency-Key".to_string(), HeaderValue::Plain(idempotency_key.to_string()))];
        for (name, t) in &self.config.headers {
            let v = self.resolver.render(t)?;
            out.push((name.clone(), if t.has_reference() { HeaderValue::Sensitive(v.into()) } else { HeaderValue::Plain(v.reveal().to_string()) }));
        }
        Ok(out)
    }

    /// Run the declared scope probe once, ahead of the source's first request
    /// (`connector.declare-capability.scope-probe`).
    fn open(&mut self) -> Result<(), Failure> {
        let (Some(probe), Some(client), false) = (&self.config.scope_probe, &self.probe_client, self.probed) else { return Ok(()) };
        let name = self.config.probe_credential().unwrap_or_default();
        let value = self.resolver.render(&self.config.headers[name])?;
        probe_through(client, &self.allow, probe, (name, value))?;
        self.probed = true;
        Ok(())
    }

    /// The table's URL, carrying the watermark under `since_param`. A watermark against a
    /// workbook refuses (`connector.source.workbook-incremental`).
    fn base_url(&self, request: &PullRequest) -> Result<Url, Failure> {
        let mut url = self.config.table_url(&self.table).map_err(|e| Failure::deterministic(FailureTag::Config, e.to_string()))?;
        let watermark = request.position.as_ref().filter(|p| is_watermark(p));
        if self.config.format == Format::Workbook && watermark.is_some() {
            return Err(Failure::deterministic(FailureTag::Config, incremental("the read carries a stored position").to_string()));
        }
        if let (Some(param), Some(at)) = (&self.config.since_param, watermark.and_then(|p| p.get("at"))) {
            let v = match at {
                Value::String(s) => s.clone(),
                other => other.to_string(),
            };
            url.query_pairs_mut().append_pair(param, &v);
        }
        Ok(url)
    }

    /// The URL of the page `token` names; `None` names the walk's first page.
    fn page_url(&self, base: &Url, token: Option<&str>) -> Result<Url, Failure> {
        let unreadable = |why: &str| Failure::deterministic(FailureTag::Config, format!("the page position `{}` {why}", scrub_text(token.unwrap_or_default())));
        Ok(match (&self.config.pagination, token) {
            (Pagination::Page { param, start }, _) => {
                let page = match token {
                    Some(t) => t.parse::<i64>().map_err(|_| unreadable("is no page number"))?,
                    None => *start,
                };
                let mut u = base.clone();
                u.query_pairs_mut().append_pair(param, &page.to_string());
                u
            }
            (Pagination::NextCursor { param, .. }, Some(t)) => {
                let mut u = base.clone();
                u.query_pairs_mut().append_pair(param, t);
                u
            }
            (Pagination::NextUrl { .. } | Pagination::LinkHeader, Some(t)) => Url::parse(t).map_err(|_| unreadable("names no URL"))?,
            (Pagination::None, Some(_)) => return Err(unreadable("names a page of a source declaring no pagination")),
            (_, None) => base.clone(),
        })
    }

    /// Fetch the page `token` names: its rows and the token of the page after it. A next
    /// URL is joined against the page's own, so the token alone names the page.
    fn page(&self, base: &Url, token: Option<&str>, request: &PullRequest, mut budget: Option<&mut ExpansionBudget>) -> Result<(Vec<Row>, Option<String>), Failure> {
        let current = self.page_url(base, token)?;
        // Hydrated per request: the resolver's cache retires a lease ahead of its expiry,
        // so a long walk re-hydrates rather than sending an expired credential.
        let headers = self.headers(&request.idempotency_key)?;
        let resp = match budget.as_ref().map(|b| b.available()).transpose()? {
            Some((bytes, time)) => self.client.send_bounded("GET", &current, &headers, None, bytes, time)?,
            None => self.client.send("GET", &current, &headers, None)?,
        };
        if let Some(b) = budget.as_deref_mut() { b.charge(resp.body.len())?; }
        if !(200..300).contains(&resp.status) {
            let retry_after = resp.header("retry-after").and_then(|v| v.trim().parse().ok());
            return Err(classify(resp.status, retry_after, &scrub(&resp.url)));
        }
        let (batch, body) = match self.config.format {
            Format::Workbook => (workbook::rows(&resp.body, self.config.sheet.as_deref(), self.config.skip_rows, &scrub(&resp.url))?, None),
            format => decode_with_encoding(format, &resp.body, self.config.records.as_deref(), &scrub(&resp.url), self.config.encoding.as_deref())?,
        };
        let batch = self.stamp(batch, &resp.url)?;
        let next = match &self.config.pagination {
            Pagination::None => None,
            Pagination::Page { start, .. } => {
                let page = token.and_then(|t| t.parse::<i64>().ok()).unwrap_or(*start);
                (!batch.is_empty()).then(|| (page + 1).to_string())
            }
            Pagination::NextCursor { path, .. } => body.as_ref().and_then(|b| b.pointer(path)).and_then(scalar),
            Pagination::NextUrl { path } => body.as_ref().and_then(|b| b.pointer(path)).and_then(scalar).map(|t| join(&resp.url, &t)).transpose()?,
            Pagination::LinkHeader => resp.header("link").and_then(next_link).map(|t| join(&resp.url, &t)).transpose()?,
        };
        if let Some(b) = budget { b.check_time()?; }
        Ok((batch, next))
    }

    /// Hold the walk to one visit per token and to [`PAGE_CAP`] requests.
    fn advance(seen: &mut Vec<String>, next: &str, base: &Url) -> Result<(), Failure> {
        if seen.iter().any(|s| s == next) {
            return Err(Failure::deterministic(
                FailureTag::Permanent,
                ConnectorError::ConnectorPageLoop(format!("`{}` served a page token it served before", scrub(base))).to_string(),
            ));
        }
        seen.push(next.to_string());
        Ok(())
    }

    fn capped(base: &Url) -> Failure {
        Failure::new(FailureTag::Permanent, format!("the walk over `{}` reached {PAGE_CAP} requests", scrub(base)))
    }

    /// Walk every page from `position`, returning every record fetched.
    pub fn walk(&self, request: &PullRequest, cancel: &dyn Cancellation) -> Result<Vec<Row>, Failure> {
        let mut budget = ExpansionBudget::new(EXPANSION_BYTES, EXPANSION_TIME);
        let base = self.base_url(request)?;
        let mut rows = Vec::new();
        let mut seen: Vec<String> = Vec::new();
        let mut token: Option<String> = None;
        for _ in 0..PAGE_CAP {
            if cancel.requested() {
                return Err(Failure::canceled("stopped during the page walk"));
            }
            let index_budget = self.config.expansion.as_ref().map(|_| &mut budget);
            let (batch, next) = self.page(&base, token.as_deref(), request, index_budget)?;
            rows.extend(batch);
            let Some(next) = next else {
                self.validate_clock(&rows, request)?;
                return self.expand(rows, request, cancel, &base, budget)
            };
            HttpSource::advance(&mut seen, &next, &base)?;
            token = Some(next);
        }
        Err(HttpSource::capped(&base))
    }

    fn validate_clock(&self, rows: &[Row], request: &PullRequest) -> Result<(), Failure> {
        if self.config.format != Format::Csv || !self.watermarked { return Ok(()) }
        let field = self.clock_field.as_deref().or_else(|| request.position.as_ref()?.get("field")?.as_str());
        let Some(field) = field else { return Ok(()) };
        let mut digits_width = None;
        let mut instant_seen = false;
        for row in rows {
            let value = clock(row, field).and_then(Value::as_str);
            let valid = match value {
                Some(s) if contextful_core::time::Instant::parse(s).is_ok() => {
                    instant_seen = true;
                    digits_width.is_none()
                }
                Some(s) if !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit()) => {
                    match digits_width {
                        Some(width) => width == s.len() && !instant_seen,
                        None => { digits_width = Some(s.len()); !instant_seen }
                    }
                }
                _ => false,
            };
            if !valid {
                return Err(Failure::deterministic(FailureTag::SchemaIncompatible, ConnectorError::ConnectorClockColumnRejected(format!("CSV column `{field}` contains an invalid or variable-width clock value")).to_string()));
            }
        }
        Ok(())
    }

    /// Resolve every pointer before the first follow-up so a bad row or an oversized
    /// index never causes a partial expansion walk.
    fn expand(&self, rows: Vec<Row>, request: &PullRequest, cancel: &dyn Cancellation, base: &Url, mut budget: ExpansionBudget) -> Result<Vec<Row>, Failure> {
        let Some(expansion) = &self.config.expansion else { return Ok(rows) };
        let mut admitted = Vec::new();
        let field = request.position.as_ref().and_then(|p| p.get("field")).and_then(Value::as_str);
        let at = request.position.as_ref().and_then(|p| p.get("at"));
        for row in rows {
            if let Some(field) = field {
                let Some(value) = clock(&row, field) else { continue };
                if !admits(at, value).map_err(|e| Failure::deterministic(FailureTag::SchemaIncompatible, e.to_string()))? {
                    continue;
                }
            }
            admitted.push(row);
        }
        budget.preflight_requests(admitted.len())?;
        let mut pointers = Vec::with_capacity(admitted.len());
        for row in &admitted {
            if row.contains_key(&expansion.target_column) {
                return Err(Failure::deterministic(FailureTag::Config, ConnectorError::ConnectorTargetColumnOccupied(format!("an index row already carries `{}`", expansion.target_column)).to_string()));
            }
            let raw = match &expansion.pointer {
                ExpansionPointer::Column(name) => row.get(name).and_then(Value::as_str).ok_or_else(|| {
                    Failure::deterministic(FailureTag::SchemaIncompatible, ConnectorError::ConnectorPointerColumnMissing(format!("`{name}` is absent or not a scalar URL")).to_string())
                })?.to_string(),
                ExpansionPointer::Template(template) => {
                    let mut values = BTreeMap::new();
                    let mut rest = template.as_str();
                    while let Some(open) = rest.find('{') {
                        let Some(close) = rest[open + 1..].find('}').map(|c| open + c + 1) else {
                            return Err(Failure::deterministic(FailureTag::Config, ConnectorError::ConnectorTemplateRejected("an unclosed placeholder".into()).to_string()));
                        };
                        let name = &rest[open + 1..close];
                        let value = row.get(name).and_then(|v| match v { Value::Bool(b) => Some(b.to_string()), other => scalar(other) }).ok_or_else(|| {
                            Failure::deterministic(FailureTag::SchemaIncompatible, ConnectorError::ConnectorPointerColumnMissing(format!("`{name}` is absent or not a scalar value")).to_string())
                        })?;
                        values.insert(name.to_string(), value);
                        rest = &rest[close + 1..];
                    }
                    fill(template, &values, percent_encode)
                }
            };
            let pointer = base.join(&raw).map_err(|e| Failure::deterministic(FailureTag::SchemaIncompatible, ConnectorError::ConnectorPointerColumnMissing(format!("`{}` names no URL: {e}", scrub_text(&raw))).to_string()))?;
            pointers.push(pointer);
        }
        for (row, pointer) in admitted.iter_mut().zip(pointers) {
            if cancel.requested() {
                return Err(Failure::canceled("stopped during expansion"));
            }
            let (remaining_body, remaining_time) = budget.available()?;
            let headers = self.headers(&request.idempotency_key)?;
            let response = self.client.send_bounded("GET", &pointer, &headers, None, remaining_body, remaining_time).map_err(|f| Failure {
                message: ConnectorError::ConnectorExpansionFailed(format!("`{}`: {}", scrub(&pointer), f.message)).to_string(),
                ..f
            })?;
            if !(200..300).contains(&response.status) {
                let failure = classify(response.status, response.header("retry-after").and_then(|v| v.trim().parse().ok()), &scrub(&response.url));
                return Err(Failure { message: ConnectorError::ConnectorExpansionFailed(failure.message).to_string(), ..failure });
            }
            budget.charge(response.body.len())?;
            let document: Value = serde_json::from_slice(&response.body).map_err(|e| Failure::deterministic(
                FailureTag::SchemaIncompatible,
                ConnectorError::ConnectorExpansionFailed(format!("`{}` served no JSON document: {e}", scrub(&response.url))).to_string(),
            ))?;
            row.insert(expansion.target_column.clone(), document);
        }
        budget.check_time()?;
        Ok(admitted)
    }

    /// Read the one page `request`'s position names, answering its rows and the page
    /// after it; `None` once the walk ends (`connector.export.native-read`).
    fn read_page(&mut self, request: &PullRequest) -> Result<(Vec<Row>, Option<String>), Failure> {
        let base = self.base_url(request)?;
        let token = match &request.position {
            None => None,
            Some(p) => match p.get("next") {
                Some(Value::Null) => None,
                Some(Value::String(t)) => Some(t.clone()),
                _ => return Err(Failure::deterministic(FailureTag::Config, format!("position {p} is no page position `{{\"next\": <token or null>}}`"))),
            },
        };
        if token.is_none() {
            self.seen.clear();
            self.requests = 0;
        }
        if self.requests >= PAGE_CAP {
            return Err(HttpSource::capped(&base));
        }
        self.requests += 1;
        let (rows, next) = self.page(&base, token.as_deref(), request, None)?;
        if let Some(n) = &next {
            HttpSource::advance(&mut self.seen, n, &base)?;
        }
        Ok((rows, next))
    }
}

/// Whether a position is a monotonic watermark `{"field", "at"}`.
fn is_watermark(position: &Value) -> bool {
    position.get("at").is_some() || position.get("field").is_some()
}

/// `token` joined against the page URL it came from.
fn join(page: &Url, token: &str) -> Result<String, Failure> {
    page.join(token).map(String::from).map_err(|e| Failure::new(FailureTag::Permanent, format!("a next link names no URL: {e}")))
}

/// The keys of a `limiter` table and of a `scope_probe` table.
const LIMITER_KEYS: [&str; 3] = ["quota", "class", "usage_headers"];
const PROBE_KEYS: [&str; 3] = ["endpoint", "scopes_header", "expect"];

/// A table's string key `k`, and its string-array key `k` (empty when absent).
fn table_text(block: &Map<String, Value>, table: &str, k: &str) -> Result<String, RunError> {
    block.get(k).and_then(Value::as_str).map(str::to_string).ok_or_else(|| RunError::Invalid(format!("`{NAME}` source `{table}` names no string `{k}`")))
}

fn table_list(block: &Map<String, Value>, table: &str, k: &str) -> Result<Vec<String>, RunError> {
    match block.get(k) {
        None => Ok(Vec::new()),
        Some(Value::Array(items)) => items
            .iter()
            .map(|i| i.as_str().map(str::to_string).ok_or_else(|| RunError::Invalid(format!("`{NAME}` source `{table}.{k}` is a list of strings, found {i}"))))
            .collect(),
        Some(other) => Err(RunError::Invalid(format!("`{NAME}` source `{table}.{k}` is a list of strings, found {other}"))),
    }
}

fn table_of<'a>(v: &'a Value, table: &str, keys: &[&str]) -> Result<&'a Map<String, Value>, RunError> {
    let block = v.as_object().ok_or_else(|| RunError::Invalid(format!("`{NAME}` source `{table}` is a table of {}", keys.join(", "))))?;
    if let Some(k) = block.keys().find(|k| !keys.contains(&k.as_str())) {
        return Err(RunError::PipelineUnknownConfigKey(format!("the `{NAME}` source's `{table}` reads no key `{k}`; it reads {}", keys.join(", "))));
    }
    Ok(block)
}

/// The `limiter` table: the shared quota, the traffic class and the forwarded quota-state headers.
fn limiter_declaration(v: &Value) -> Result<LimiterDeclaration, ConfigError> {
    let block = table_of(v, "limiter", &LIMITER_KEYS)?;
    let forward = table_list(block, "limiter", "usage_headers")?;
    Ok(LimiterDeclaration::new(&table_text(block, "limiter", "quota")?, &table_text(block, "limiter", "class")?, &forward)?)
}

/// The `scope_probe` table: the identity endpoint, the granted-scopes header and the expected grant.
fn scope_probe(v: &Value) -> Result<ScopeProbe, ConfigError> {
    let block = table_of(v, "scope_probe", &PROBE_KEYS)?;
    let expect = table_list(block, "scope_probe", "expect")?;
    Ok(ScopeProbe::new(&table_text(block, "scope_probe", "endpoint")?, &table_text(block, "scope_probe", "scopes_header")?, &expect)?)
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

/// The request headers a conditional read sends from its stored validators.
fn conditional_headers(position: Option<&Value>) -> Vec<(String, HeaderValue)> {
    [("etag", "If-None-Match"), ("last_modified", "If-Modified-Since")]
        .into_iter()
        .filter_map(|(key, header)| position?.get(key)?.as_str().map(|v| (header.to_string(), HeaderValue::Plain(v.to_string()))))
        .collect()
}

/// The validators a `2xx` serves, as the position a conditional read commits: `etag` and
/// `last_modified`, each present when served.
fn served_validators(resp: &contextful_outbound::client::Response) -> Value {
    let mut out = Map::new();
    for (header, key) in [("etag", "etag"), ("last-modified", "last_modified")] {
        if let Some(v) = resp.header(header) {
            out.insert(key.to_string(), Value::String(v.to_string()));
        }
    }
    Value::Object(out)
}

impl HttpSource {
    /// Stamp every bound column onto each of `batch`'s rows (`connector.source.bound-columns`).
    fn stamp(&self, batch: Vec<Row>, url: &Url) -> Result<Vec<Row>, Failure> {
        let bound = self.config.bound_values(&self.table).map_err(|e| Failure::deterministic(FailureTag::Config, e.to_string()))?;
        let mut out = Vec::with_capacity(batch.len());
        for mut row in batch {
            for (column, value) in &bound {
                if row.contains_key(column) {
                    return Err(Failure::deterministic(
                        FailureTag::Permanent,
                        ConnectorError::ConnectorBoundColumnOccupied(format!(
                            "a row from `{}` already carries `{column}`, which the source binds from table `{}`",
                            scrub(url),
                            self.table
                        ))
                        .to_string(),
                    ));
                }
                row.insert(column.clone(), Value::String(value.clone()));
            }
            out.push(row);
        }
        Ok(out)
    }

    /// A conditional pull: one request for the one document, carrying the stored validators.
    /// The validators are the whole position, and the pull reports no further page
    /// (`connector.source.conditional-position`).
    fn conditional_pull(&self, request: &PullRequest, cancel: &dyn Cancellation) -> Result<Value, Failure> {
        let mut budget = ExpansionBudget::new(EXPANSION_BYTES, EXPANSION_TIME);
        if cancel.requested() {
            return Err(Failure::canceled("stopped ahead of the conditional request"));
        }
        let url = self.config.table_url(&self.table).map_err(|e| Failure::deterministic(FailureTag::Config, e.to_string()))?;
        let mut headers = self.headers(&request.idempotency_key)?;
        headers.extend(conditional_headers(request.position.as_ref()));
        let resp = if self.config.expansion.is_some() {
            let (bytes, time) = budget.available()?;
            self.client.send_bounded("GET", &url, &headers, None, bytes, time)?
        } else {
            self.client.send("GET", &url, &headers, None)?
        };
        if self.config.expansion.is_some() { budget.charge(resp.body.len())?; }
        if resp.status == 304 {
            return Ok(serde_json::json!({ "rows": [], "cursor": request.position.clone().unwrap_or_else(|| Value::Object(Map::new())), "more": false, "snapshot_complete": false }));
        }
        if !(200..300).contains(&resp.status) {
            let retry_after = resp.header("retry-after").and_then(|v| v.trim().parse().ok());
            return Err(classify(resp.status, retry_after, &scrub(&resp.url)));
        }
        let batch = match self.config.format {
            Format::Workbook => workbook::rows(&resp.body, self.config.sheet.as_deref(), self.config.skip_rows, &scrub(&resp.url))?,
            format => decode_with_encoding(format, &resp.body, self.config.records.as_deref(), &scrub(&resp.url), self.config.encoding.as_deref())?.0,
        };
        let rows = self.expand(self.stamp(batch, &resp.url)?, request, cancel, &resp.url, budget)?;
        Ok(serde_json::json!({ "rows": rows, "cursor": served_validators(&resp), "more": false, "snapshot_complete": true }))
    }
}

impl Source for HttpSource {
    /// One pull reads one page and carries the next page's token as its position, so a
    /// walk resumes at the page it stopped on; the last page's position names the start.
    /// A watermarked source, and one following next URLs, walks every page in one pull.
    fn pull(&mut self, request: &PullRequest, cancel: &dyn Cancellation) -> Result<Vec<u8>, Failure> {
        if cancel.requested() {
            return Err(Failure::canceled("stopped ahead of the page request"));
        }
        self.open()?;
        let pulled = if self.config.conditional {
            self.conditional_pull(request, cancel)?
        } else if self.walks_whole() {
            serde_json::json!({ "rows": self.walk(request, cancel)?, "more": false, "snapshot_complete": !self.watermarked })
        } else {
            let (rows, next) = self.read_page(request)?;
            serde_json::json!({ "rows": rows, "cursor": { "next": next }, "more": next.is_some(), "snapshot_complete": next.is_none() })
        };
        serde_json::to_vec(&pulled).map_err(|e| Failure::new(FailureTag::Permanent, e.to_string()))
    }
}
