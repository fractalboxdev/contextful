//! The `file` source: a folder of markdown notes, plain text and PDF documents under one
//! root, landed as one `documents` table keyed by slug and ordinal.
//!
//! The walk stays under the root, skips dot-entries, follows no symbolic link and visits
//! entries in sorted order (`connector.source.walk-boundary`). A note lands one row, or one
//! row per heading section past [`CHUNK_BYTES`]; a PDF lands one row per page, decoded
//! behind the process boundary (`connector.source.document-grain`). The position maps each
//! landed path to its content digest and row count, so a read re-lands a changed file alone
//! and tombstones what left (`connector.source.file-position`).

use crate::boundary::PageDecoder;
use crate::decode::document::{self, is_compound};
use crate::http::ConfigError;
use contextful_core::connector::ConnectorError;
use contextful_core::run::ports::{Cancellation, PullRequest, Row, Source};
use contextful_core::run::{Failure, FailureTag, RunError};
use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::io::Read as _;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use url::Url;

/// The source's registered name.
pub const NAME: &str = "file";
/// The one table the source serves.
pub const TABLE: &str = "documents";
/// The configuration keys the source reads (`run.declare.config-key`).
pub const KEYS: [&str; 4] = ["root", "include", "exclude", "base_url"];
/// Bytes one file may carry before the walk declines it (`connector.source.file-cap`).
pub const MAX_FILE_BYTES: u64 = 64 * 1024 * 1024;
/// Body bytes past which a note lands one row per heading section
/// (`connector.source.heading-threshold`).
pub const CHUNK_BYTES: usize = 8192;
/// The columns the source lands itself; a frontmatter key other than `title` naming one is
/// refused (`connector.source.frontmatter-shape`).
pub const COLUMNS: [&str; 10] = ["slug", "ordinal", "path", "kind", "title", "heading", "text", "url", "sha256", "removed"];

/// Extensions of compound-binary office containers (`connector.source.conversion-required`).
const COMPOUND: [&str; 7] = ["doc", "dot", "xls", "xlt", "ppt", "pot", "pps"];

fn invalid(why: String) -> ConfigError {
    ConfigError::Run(RunError::Invalid(format!("`{NAME}` source {why}")))
}

/// One include or exclude pattern over a root-relative path: `*` and `?` match within one
/// segment, and a `**` segment matches any number of segments.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Glob {
    segments: Vec<String>,
}

impl Glob {
    pub fn parse(raw: &str) -> Result<Glob, String> {
        if raw.is_empty() || raw.starts_with('/') {
            return Err(format!("glob `{raw}` is a non-empty root-relative pattern"));
        }
        let segments: Vec<String> = raw.split('/').map(str::to_string).collect();
        if segments.iter().any(|s| s.is_empty() || s == "." || s == "..") {
            return Err(format!("glob `{raw}` holds an empty, `.` or `..` segment"));
        }
        Ok(Glob { segments })
    }

    pub fn matches(&self, path: &str) -> bool {
        let parts: Vec<&str> = path.split('/').collect();
        let pattern: Vec<&str> = self.segments.iter().map(String::as_str).collect();
        segments_match(&pattern, &parts)
    }
}

fn segments_match(pattern: &[&str], path: &[&str]) -> bool {
    match pattern.split_first() {
        None => path.is_empty(),
        Some((&"**", rest)) => (0..=path.len()).any(|skip| segments_match(rest, &path[skip..])),
        Some((p, rest)) => path.split_first().is_some_and(|(s, tail)| segment_match(p.as_bytes(), s.as_bytes()) && segments_match(rest, tail)),
    }
}

fn segment_match(p: &[u8], s: &[u8]) -> bool {
    match p.split_first() {
        None => s.is_empty(),
        Some((b'*', rest)) => (0..=s.len()).any(|skip| segment_match(rest, &s[skip..])),
        Some((b'?', rest)) => s.split_first().is_some_and(|(_, tail)| segment_match(rest, tail)),
        Some((c, rest)) => s.split_first().is_some_and(|(d, tail)| c == d && segment_match(rest, tail)),
    }
}

/// A parsed `file` source configuration.
#[derive(Debug, Clone)]
pub struct FileConfig {
    /// The root, resolved against the project directory when relative.
    pub root: PathBuf,
    /// Empty: every path the walk reaches.
    pub include: Vec<Glob>,
    pub exclude: Vec<Glob>,
    pub base_url: Option<Url>,
}

impl FileConfig {
    /// Parse and check a configuration before any I/O.
    pub fn parse(config: &Value) -> Result<FileConfig, ConfigError> {
        let cfg = config.as_object().ok_or_else(|| invalid("config is an object".into()))?;
        if let Some(k) = cfg.keys().find(|k| !KEYS.contains(&k.as_str())) {
            return Err(RunError::PipelineUnknownConfigKey(format!("the `{NAME}` source reads no key `{k}`; it reads {}", KEYS.join(", "))).into());
        }
        let root = match cfg.get("root") {
            Some(Value::String(s)) if !s.is_empty() => PathBuf::from(s),
            Some(other) => return Err(invalid(format!("`root` is a directory path, found {other}"))),
            None => return Err(invalid("names no `root`, the directory the walk starts from".into())),
        };
        let globs = |key: &str| -> Result<Vec<Glob>, ConfigError> {
            match cfg.get(key) {
                None => Ok(Vec::new()),
                Some(Value::Array(items)) => items
                    .iter()
                    .map(|v| match v {
                        Value::String(s) => Glob::parse(s).map_err(|why| invalid(format!("`{key}`: {why}"))),
                        other => Err(invalid(format!("`{key}` is a list of globs, found {other}"))),
                    })
                    .collect(),
                Some(other) => Err(invalid(format!("`{key}` is a list of globs, found {other}"))),
            }
        };
        let (include, exclude) = (globs("include")?, globs("exclude")?);
        let base_url = match cfg.get("base_url") {
            None => None,
            Some(Value::String(s)) => match Url::parse(s) {
                Ok(u) if matches!(u.scheme(), "http" | "https") && u.query().is_none() && u.fragment().is_none() => Some(u),
                _ => return Err(invalid(format!("`base_url` is an http(s) URL with no query or fragment, found `{s}`"))),
            },
            Some(other) => return Err(invalid(format!("`base_url` is a URL, found {other}"))),
        };
        Ok(FileConfig { root, include, exclude, base_url })
    }

    /// Refuse a table the source does not serve, ahead of the walk
    /// (`connector.source.file-table-unmatched`).
    pub fn table(&self, table: &str) -> Result<(), ConnectorError> {
        if table == TABLE {
            return Ok(());
        }
        Err(ConnectorError::ConnectorTableUnmatched(format!("the `{NAME}` source serves `{TABLE}`, not `{table}`")))
    }

    fn admits(&self, path: &str) -> bool {
        (self.include.is_empty() || self.include.iter().any(|g| g.matches(path))) && !self.exclude.iter().any(|g| g.matches(path))
    }
}

/// How the source reads one file, by its extension.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Markdown,
    Text,
    Pdf,
    Compound,
    Other,
}

impl Kind {
    fn of(extension: &str) -> Kind {
        match extension {
            "md" | "markdown" => Kind::Markdown,
            "txt" => Kind::Text,
            "pdf" => Kind::Pdf,
            e if COMPOUND.contains(&e) => Kind::Compound,
            _ => Kind::Other,
        }
    }

    fn name(self) -> &'static str {
        match self {
            Kind::Markdown => "markdown",
            Kind::Text => "text",
            Kind::Pdf => "pdf",
            Kind::Compound | Kind::Other => "other",
        }
    }
}

/// One file the walk reached under the root and the globs admit.
#[derive(Debug, Clone)]
struct Found {
    path: String,
    abs: PathBuf,
    size: u64,
    extension: String,
}

/// A document's slug: its root-relative path without extension, lowercased, each run of
/// characters other than ASCII letters and digits folded to `-` within each segment
/// (`connector.source.document-slug`).
pub fn slug(path: &str) -> String {
    let stem = match path.rsplit_once('/') {
        Some((dir, name)) => format!("{dir}/{}", name.rsplit_once('.').map_or(name, |(s, _)| if s.is_empty() { name } else { s })),
        None => path.rsplit_once('.').map_or(path, |(s, _)| if s.is_empty() { path } else { s }).to_string(),
    };
    stem.split('/')
        .map(|segment| {
            let mut out = String::new();
            for c in segment.chars().flat_map(char::to_lowercase) {
                if c.is_ascii_alphanumeric() {
                    out.push(c);
                } else if !out.ends_with('-') {
                    out.push('-');
                }
            }
            let trimmed = out.trim_matches('-');
            if trimmed.is_empty() { "-".to_string() } else { trimmed.to_string() }
        })
        .collect::<Vec<_>>()
        .join("/")
}

/// One landed part of a document.
struct Part {
    heading: Option<String>,
    anchor: Option<String>,
    text: String,
}

/// A decoded document.
struct Doc {
    kind: Kind,
    title: String,
    frontmatter: Vec<document::Entry>,
    parts: Vec<Part>,
}

/// What the position holds for one landed path.
#[derive(Debug, Clone)]
struct Held {
    sha256: String,
    slug: String,
    rows: u64,
}

fn held(position: Option<&Value>) -> BTreeMap<String, Held> {
    let Some(files) = position.and_then(|p| p.get("files")).and_then(Value::as_object) else { return BTreeMap::new() };
    files
        .iter()
        .map(|(path, v)| {
            let text = |k: &str| v.get(k).and_then(Value::as_str).unwrap_or_default().to_string();
            (path.clone(), Held { sha256: text("sha256"), slug: text("slug"), rows: v.get("rows").and_then(Value::as_u64).unwrap_or(0) })
        })
        .collect()
}

/// One read of the `documents` table.
#[derive(Debug, Clone)]
pub struct Read {
    pub rows: Vec<Row>,
    pub position: Value,
    /// The files the walk declined, tallied by extension (`connector.source.declined-tally`).
    pub declined: BTreeMap<String, u64>,
}

/// The `file` source over one root.
pub struct FileSource {
    config: FileConfig,
    root: PathBuf,
    decoder: Option<Arc<dyn PageDecoder>>,
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn io_fault(path: &Path, e: std::io::Error) -> Failure {
    Failure::new(FailureTag::Permanent, format!("`{NAME}` source reading `{}`: {e}", path.display()))
}

impl FileSource {
    /// A source over `config.root`, resolved against `base` when relative. `decoder` reads
    /// PDF pages; a build without one refuses a PDF with the feature to rebuild with.
    pub fn new(config: FileConfig, base: &Path, decoder: Option<Arc<dyn PageDecoder>>) -> FileSource {
        let root = if config.root.is_absolute() { config.root.clone() } else { base.join(&config.root) };
        FileSource { config, root, decoder }
    }

    /// Every file under the root the globs admit, in sorted path order.
    fn walk(&self, cancel: &dyn Cancellation) -> Result<Vec<Found>, Failure> {
        let meta = std::fs::metadata(&self.root).map_err(|e| Failure::deterministic(FailureTag::Config, format!("`{NAME}` source `root` `{}` names no directory: {e}", self.config.root.display())))?;
        if !meta.is_dir() {
            return Err(Failure::deterministic(FailureTag::Config, format!("`{NAME}` source `root` `{}` is not a directory", self.config.root.display())));
        }
        let mut found = Vec::new();
        let mut stack: Vec<(PathBuf, String)> = vec![(self.root.clone(), String::new())];
        while let Some((dir, prefix)) = stack.pop() {
            if cancel.requested() {
                return Err(Failure::canceled("stopped during the directory walk"));
            }
            let mut entries: Vec<(String, PathBuf)> = Vec::new();
            for entry in std::fs::read_dir(&dir).map_err(|e| io_fault(&dir, e))? {
                let entry = entry.map_err(|e| io_fault(&dir, e))?;
                let name = entry.file_name().to_string_lossy().to_string();
                if name.starts_with('.') {
                    continue;
                }
                entries.push((name, entry.path()));
            }
            entries.sort();
            let mut dirs = Vec::new();
            for (name, abs) in entries {
                let path = if prefix.is_empty() { name.clone() } else { format!("{prefix}/{name}") };
                // `symlink_metadata` describes the link itself, so no link is followed.
                let meta = std::fs::symlink_metadata(&abs).map_err(|e| io_fault(&abs, e))?;
                if meta.is_dir() {
                    dirs.push((abs, path));
                } else if meta.is_file() && self.config.admits(&path) {
                    let extension = name.rsplit_once('.').filter(|(s, _)| !s.is_empty()).map(|(_, e)| e.to_lowercase()).unwrap_or_default();
                    found.push(Found { path, abs, size: meta.len(), extension });
                }
            }
            stack.extend(dirs.into_iter().rev());
        }
        found.sort_by(|a, b| a.path.cmp(&b.path));
        Ok(found)
    }

    /// The file's bytes, at most one past the cap.
    fn bytes(&self, f: &Found) -> Result<Vec<u8>, Failure> {
        let file = std::fs::File::open(&f.abs).map_err(|e| io_fault(&f.abs, e))?;
        let mut out = Vec::new();
        file.take(MAX_FILE_BYTES + 1).read_to_end(&mut out).map_err(|e| io_fault(&f.abs, e))?;
        Ok(out)
    }

    /// The file's leading bytes, where a compound-binary container announces itself.
    fn head(&self, f: &Found) -> Result<Vec<u8>, Failure> {
        let file = std::fs::File::open(&f.abs).map_err(|e| io_fault(&f.abs, e))?;
        let mut out = Vec::new();
        file.take(document::COMPOUND_MAGIC.len() as u64).read_to_end(&mut out).map_err(|e| io_fault(&f.abs, e))?;
        Ok(out)
    }

    fn decode(&self, f: &Found, kind: Kind, bytes: &[u8]) -> Result<Doc, Failure> {
        let stem = f.path.rsplit('/').next().unwrap_or(&f.path);
        let stem = stem.rsplit_once('.').filter(|(s, _)| !s.is_empty()).map_or(stem, |(s, _)| s).to_string();
        let utf8 = || {
            std::str::from_utf8(bytes).map_err(|e| {
                Failure::deterministic(FailureTag::Permanent, RunError::PipelineUnreadableInput(format!("`{}` at byte {}: not UTF-8 text", f.path, e.valid_up_to())).to_string())
            })
        };
        match kind {
            Kind::Markdown => {
                let taken: Vec<&str> = COLUMNS.iter().copied().filter(|c| *c != "title").collect();
                let (mut frontmatter, body) = document::frontmatter(utf8()?, &f.path, &taken)?;
                let declared = frontmatter.iter().position(|(k, _)| k == "title").map(|i| frontmatter.remove(i).1);
                let title = match declared {
                    Some(Value::String(t)) if !t.is_empty() => t,
                    _ => document::title(body).unwrap_or(stem),
                };
                let parts = if body.len() > CHUNK_BYTES {
                    document::sections(body).into_iter().map(|s| Part { heading: s.heading, anchor: s.anchor, text: s.text }).collect()
                } else {
                    vec![Part { heading: None, anchor: None, text: body.trim().to_string() }]
                };
                Ok(Doc { kind, title, frontmatter, parts })
            }
            Kind::Text => Ok(Doc { kind, title: stem, frontmatter: Vec::new(), parts: vec![Part { heading: None, anchor: None, text: utf8()?.trim().to_string() }] }),
            Kind::Pdf => {
                let decoder = self.decoder.as_ref().ok_or_else(|| {
                    Failure::deterministic(FailureTag::Config, format!("`{}` is a PDF, and the PDF decoder is compiled out of this build; rebuild with `--features pdf`", f.path))
                })?;
                let pages = decoder.pages(bytes, &f.path)?;
                let parts = pages.into_iter().enumerate().map(|(i, text)| Part { heading: None, anchor: Some(format!("page={}", i + 1)), text }).collect();
                Ok(Doc { kind, title: stem, frontmatter: Vec::new(), parts })
            }
            Kind::Compound | Kind::Other => Err(Failure::deterministic(FailureTag::Permanent, format!("`{}` has no document decoder", f.path))),
        }
    }

    fn url(&self, slug: &str, anchor: Option<&str>) -> Value {
        match &self.config.base_url {
            None => Value::Null,
            Some(base) => {
                let mut url = format!("{}/{slug}", base.as_str().trim_end_matches('/'));
                if let Some(a) = anchor {
                    url.push('#');
                    url.push_str(a);
                }
                Value::String(url)
            }
        }
    }

    fn tombstone(slug: &str, ordinal: u64, path: &str) -> Row {
        let mut r = Map::new();
        for c in COLUMNS {
            r.insert(c.into(), Value::Null);
        }
        r.insert("slug".into(), json!(slug));
        r.insert("ordinal".into(), json!(ordinal));
        r.insert("path".into(), json!(path));
        r.insert("removed".into(), json!(true));
        r
    }

    /// The rows since `position`, the position after them, and what the walk declined.
    pub fn read(&self, position: Option<&Value>, cancel: &dyn Cancellation) -> Result<Read, Failure> {
        let before = held(position);
        let found = self.walk(cancel)?;
        let mut declined: BTreeMap<String, u64> = BTreeMap::new();
        let mut landing: Vec<(Found, Kind)> = Vec::new();
        let mut slugs: BTreeMap<String, String> = BTreeMap::new();
        for f in found {
            if cancel.requested() {
                return Err(Failure::canceled("stopped between files"));
            }
            let kind = Kind::of(&f.extension);
            if kind == Kind::Compound || is_compound(&self.head(&f)?) {
                let to = if matches!(f.extension.as_str(), "xls" | "xlt") { "xlsx" } else { "pdf" };
                return Err(Failure::deterministic(
                    FailureTag::Permanent,
                    ConnectorError::ConnectorConversionRequired(format!(
                        "`{}` is a compound-binary office container; convert it with `soffice --headless --convert-to {to} '{}'` and land the result",
                        f.path, f.path
                    ))
                    .to_string(),
                ));
            }
            if kind == Kind::Other || f.size > MAX_FILE_BYTES {
                *declined.entry(f.extension.clone()).or_default() += 1;
                continue;
            }
            let s = slug(&f.path);
            if let Some(other) = slugs.insert(s.clone(), f.path.clone()) {
                return Err(Failure::deterministic(FailureTag::Config, format!("`{other}` and `{}` both fold to slug `{s}`; rename one or exclude it", f.path)));
            }
            landing.push((f, kind));
        }

        let mut rows: Vec<Row> = Vec::new();
        let mut after = Map::new();
        let mut columns: BTreeSet<String> = BTreeSet::new();
        for (f, kind) in landing {
            if cancel.requested() {
                return Err(Failure::canceled("stopped between files"));
            }
            let bytes = self.bytes(&f)?;
            if bytes.len() as u64 > MAX_FILE_BYTES {
                // The file grew past the cap after the walk measured it.
                *declined.entry(f.extension.clone()).or_default() += 1;
                continue;
            }
            let digest = hex(&Sha256::digest(&bytes));
            let s = slug(&f.path);
            let prior = before.get(&f.path);
            if let Some(h) = prior.filter(|h| h.sha256 == digest) {
                after.insert(f.path.clone(), json!({"sha256": digest, "slug": h.slug, "rows": h.rows}));
                continue;
            }
            let doc = self.decode(&f, kind, &bytes)?;
            let count = doc.parts.len() as u64;
            for (i, part) in doc.parts.into_iter().enumerate() {
                let mut r = Map::new();
                r.insert("slug".into(), json!(s));
                r.insert("ordinal".into(), json!(i as u64 + 1));
                r.insert("path".into(), json!(f.path));
                r.insert("kind".into(), json!(doc.kind.name()));
                r.insert("title".into(), json!(doc.title));
                r.insert("heading".into(), json!(part.heading));
                r.insert("url".into(), self.url(&s, part.anchor.as_deref()));
                r.insert("text".into(), json!(part.text));
                r.insert("sha256".into(), json!(digest));
                r.insert("removed".into(), json!(false));
                for (k, v) in &doc.frontmatter {
                    columns.insert(k.clone());
                    let cell = match v {
                        Value::Array(_) => Value::String(v.to_string()),
                        other => other.clone(),
                    };
                    r.insert(k.clone(), cell);
                }
                rows.push(r);
            }
            if let Some(h) = prior {
                rows.extend((count + 1..=h.rows).map(|o| Self::tombstone(&s, o, &f.path)));
            }
            after.insert(f.path.clone(), json!({"sha256": digest, "slug": s, "rows": count}));
        }
        for (path, h) in before.iter().filter(|(p, _)| !after.contains_key(*p)) {
            rows.extend((1..=h.rows).map(|o| Self::tombstone(&h.slug, o, path)));
        }
        // Every row of one pull carries every frontmatter column the pull lands.
        for r in &mut rows {
            for c in &columns {
                r.entry(c.clone()).or_insert(Value::Null);
            }
        }
        Ok(Read { rows, position: json!({ "files": after }), declined })
    }

    fn types(rows: &[Row]) -> BTreeMap<String, String> {
        let mut types: BTreeMap<String, String> = COLUMNS.iter().map(|c| (c.to_string(), "utf8".to_string())).collect();
        types.insert("ordinal".into(), "int64".into());
        types.insert("removed".into(), "boolean".into());
        for c in rows.iter().flat_map(|r| r.keys()) {
            types.entry(c.clone()).or_insert_with(|| "utf8".into());
        }
        types
    }
}

impl Source for FileSource {
    /// One pull is one whole walk; the position rides the pull as an opaque token.
    fn pull(&mut self, request: &PullRequest, cancel: &dyn Cancellation) -> Result<Vec<u8>, Failure> {
        let read = self.read(request.position.as_ref(), cancel)?;
        let skipped: u64 = read.declined.values().sum();
        let types = Self::types(&read.rows);
        serde_json::to_vec(&json!({ "rows": read.rows, "cursor": read.position, "more": false, "types": types, "skipped": skipped, "declined": read.declined }))
            .map_err(|e| Failure::new(FailureTag::Permanent, e.to_string()))
    }
}
