//! The Google Drive source: every file under one folder, walked breadth-first through the
//! Drive v3 API, landed as a `files` row per file and a `pages` row per PDF page.
//!
//! Docs, Sheets and Slides land as their PDF export and other files as their bytes; a PDF
//! body decodes behind the process boundary (`connector.source.drive-page-grain`). Bytes
//! land in no column: each body read whole lands as a content-addressed blob that its file
//! row names by digest (`connector.source.drive-bytes`). The position records each file's
//! `modifiedTime`, path and page count, so a read re-lands a changed file alone and lands a
//! tombstone for what left the tree (`connector.source.drive-incremental`). The access
//! token is minted from `secret://` references once per fire, held in memory, and written
//! nowhere (`connector.source.drive-oauth`).

use crate::http::ConfigError;
use contextful_core::connector::attach::{is_loopback_host, scrub, Allowlist};
use contextful_core::connector::reference::{check_material, Hydrated, Template};
use contextful_core::connector::ConnectorError;
use contextful_core::run::ports::{Cancellation, PullRequest, Row, Source};
use contextful_core::run::{Failure, FailureTag, RunError};
use contextful_outbound::client::{classify, is_over_limit, Client, HeaderValue, Response};
use contextful_outbound::Resolver;
use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use url::Url;

/// The source's registered name.
pub const NAME: &str = crate::DRIVE;
/// The tables the source serves.
pub const TABLES: [&str; 2] = ["files", "pages"];
/// The configuration keys the source reads (`run.declare.config-key`).
pub const KEYS: [&str; 8] = ["folder_id", "folder_ids", "drive_id", "max_file_bytes", "mode", "oauth", "api_base", "token_url"];
/// Selected folder roots in one Shared Drive (`connector.source.drive-root-set`).
pub const MAX_ROOTS: usize = 16;
/// The keys of the `oauth` table.
pub const OAUTH_KEYS: [&str; 3] = ["refresh_token", "client_id", "client_secret"];
/// Bytes one file's body may carry: 64 MiB unless declared (`connector.source.drive-file-cap`).
pub const MAX_FILE_BYTES: u64 = 64 * 1024 * 1024;
/// Listing requests one read issues (`connector.source.page-cap`).
pub const LISTING_CAP: usize = crate::http::PAGE_CAP;

const API_BASE: &str = "https://www.googleapis.com";
const API_HOST: &str = "www.googleapis.com";
const TOKEN_URL: &str = "https://oauth2.googleapis.com/token";
const TOKEN_HOST: &str = "oauth2.googleapis.com";
const FOLDER: &str = "application/vnd.google-apps.folder";
const SHORTCUT: &str = "application/vnd.google-apps.shortcut";
const NATIVE: &str = "application/vnd.google-apps.";
const PDF: &str = "application/pdf";
/// The Google-native types exported as PDF: Docs, Sheets and Slides.
const EXPORTED: [&str; 3] = ["application/vnd.google-apps.document", "application/vnd.google-apps.spreadsheet", "application/vnd.google-apps.presentation"];
/// A token within this much of its expiry is minted again before a request.
const EXPIRY_MARGIN: Duration = Duration::from_secs(60);
/// The reason Drive answers, with `403`, an export over its size limit.
const EXPORT_LIMIT: &str = "exportSizeLimitExceeded";
const FILE_FIELDS: &str = "id,name,mimeType,modifiedTime,version,size,md5Checksum";

/// The one-reference credentials the token exchange reads.
#[derive(Debug, Clone)]
pub struct OAuth {
    pub refresh_token: Template,
    pub client_id: Template,
    pub client_secret: Template,
}

/// A parsed drive source configuration.
#[derive(Debug, Clone)]
pub struct DriveConfig {
    pub folder_id: Option<String>,
    pub folder_ids: Option<Vec<String>>,
    pub drive_id: Option<String>,
    pub max_file_bytes: u64,
    pub mode: CaptureMode,
    pub oauth: OAuth,
    /// The API origin, `https://www.googleapis.com` unless a loopback test origin replaces it.
    pub api_base: Url,
    pub token_url: Url,
}

/// How a Drive file's captured bytes are retained.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CaptureMode {
    BytesAndPages,
    MetadataOnly,
}

impl CaptureMode {
    fn as_str(self) -> &'static str {
        match self {
            Self::BytesAndPages => "bytes-and-pages",
            Self::MetadataOnly => "metadata-only",
        }
    }
}

fn invalid(why: String) -> ConfigError {
    ConfigError::Run(RunError::Invalid(format!("`{NAME}` source {why}")))
}

/// A Drive id: letters, digits, `-` and `_`, so it binds into a query without escaping.
fn drive_id(key: &str, v: Option<&Value>) -> Result<Option<String>, ConfigError> {
    match v {
        None => Ok(None),
        Some(Value::String(s)) if !s.is_empty() && s.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_') => Ok(Some(s.clone())),
        Some(other) => Err(invalid(format!("`{key}` is a Drive id of letters, digits, `-` and `_`, found {other}"))),
    }
}

/// An endpoint pinned to `host` over TLS; a loopback origin is admitted for a recorded fake
/// (`connector.source.drive-origin`).
fn pinned(key: &str, raw: &str, host: &str) -> Result<Url, ConfigError> {
    let url = Url::parse(raw).map_err(|e| invalid(format!("`{key}` names no URL: {e}")))?;
    let got = url.host_str().unwrap_or_default();
    if is_loopback_host(got) || (url.scheme() == "https" && got == host && url.port().is_none()) {
        return Ok(url);
    }
    Err(ConnectorError::ConnectorProviderOriginRejected(format!("`{NAME}` source `{key}` names `{}`; a Google credential goes to https://{host} alone", scrub(&url))).into())
}

impl DriveConfig {
    /// Parse and check a configuration before any I/O.
    pub fn parse(config: &Value) -> Result<DriveConfig, ConfigError> {
        let cfg = config.as_object().ok_or_else(|| invalid("config is an object".into()))?;
        if let Some(k) = cfg.keys().find(|k| !KEYS.contains(&k.as_str())) {
            return Err(RunError::PipelineUnknownConfigKey(format!("the `{NAME}` source reads no key `{k}`; it reads {}", KEYS.join(", "))).into());
        }
        let folder_id = drive_id("folder_id", cfg.get("folder_id"))?;
        let folder_ids = match cfg.get("folder_ids") {
            None => None,
            Some(Value::Array(ids)) => {
                let parsed = ids.iter().map(|id| drive_id("folder_ids entry", Some(id))).collect::<Result<Vec<_>, _>>();
                let parsed = parsed.map_err(|_| ConnectorError::ConnectorDriveRootsInvalid("`folder_ids` holds Drive ids of letters, digits, `-` and `_`".into()))?;
                let ids = parsed.into_iter().flatten().collect::<Vec<_>>();
                if ids.is_empty() || ids.len() > MAX_ROOTS || ids.iter().collect::<BTreeSet<_>>().len() != ids.len() {
                    return Err(ConnectorError::ConnectorDriveRootsInvalid(format!("`folder_ids` holds 1 to {MAX_ROOTS} distinct folder ids")).into());
                }
                Some(ids)
            }
            Some(_) => return Err(ConnectorError::ConnectorDriveRootsInvalid("`folder_ids` is an array of Drive ids".into()).into()),
        };
        let drive_id = drive_id("drive_id", cfg.get("drive_id"))?;
        if folder_ids.is_some() && (folder_id.is_some() || drive_id.is_none()) {
            return Err(ConnectorError::ConnectorDriveRootsInvalid("`folder_ids` requires `drive_id` and excludes `folder_id`".into()).into());
        }
        if folder_id.is_none() && folder_ids.is_none() {
            return Err(invalid("names no `folder_id`, the root the walk starts from".into()));
        }
        let mode = match cfg.get("mode") {
            None => CaptureMode::BytesAndPages,
            Some(Value::String(s)) if s == "bytes-and-pages" => CaptureMode::BytesAndPages,
            Some(Value::String(s)) if s == "metadata-only" => CaptureMode::MetadataOnly,
            Some(other) => return Err(ConnectorError::ConnectorDriveModeUnknown(format!("`mode` is `bytes-and-pages` or `metadata-only`, found {other}")).into()),
        };
        let max_file_bytes = match cfg.get("max_file_bytes") {
            None => MAX_FILE_BYTES,
            Some(v) => match v.as_u64() {
                Some(n) if (1..=contextful_outbound::client::MAX_BODY_BYTES).contains(&n) => n,
                _ => return Err(invalid(format!("`max_file_bytes` is a byte count from 1 to {}, found {v}", contextful_outbound::client::MAX_BODY_BYTES))),
            },
        };
        let oauth = cfg.get("oauth").and_then(Value::as_object).ok_or_else(|| invalid("names no `oauth` table of `refresh_token`, `client_id` and `client_secret` references".into()))?;
        if let Some(k) = oauth.keys().find(|k| !OAUTH_KEYS.contains(&k.as_str())) {
            return Err(RunError::PipelineUnknownConfigKey(format!("the `{NAME}` source's `oauth` table reads no key `{k}`; it reads {}", OAUTH_KEYS.join(", "))).into());
        }
        let reference = |k: &str| -> Result<Template, ConfigError> {
            let key = format!("oauth.{k}");
            let raw = oauth.get(k).and_then(Value::as_str).ok_or_else(|| invalid(format!("`{key}` is a `${{secret://<name>}}` reference")))?;
            let t = check_material(&key, raw)?;
            match t.parts.as_slice() {
                [contextful_core::connector::reference::Part::Secret(_)] => Ok(t),
                _ => Err(invalid(format!("`{key}` is one `${{secret://<name>}}` reference and nothing else"))),
            }
        };
        let oauth = OAuth { refresh_token: reference("refresh_token")?, client_id: reference("client_id")?, client_secret: reference("client_secret")? };
        let text = |k: &str, default: &str| -> Result<String, ConfigError> {
            match cfg.get(k) {
                None => Ok(default.to_string()),
                Some(Value::String(s)) => Ok(s.clone()),
                Some(other) => Err(invalid(format!("`{k}` is a URL, found {other}"))),
            }
        };
        let api_base = pinned("api_base", &text("api_base", API_BASE)?, API_HOST)?;
        let token_url = pinned("token_url", &text("token_url", TOKEN_URL)?, TOKEN_HOST)?;
        Ok(DriveConfig { folder_id, folder_ids, drive_id, max_file_bytes, mode, oauth, api_base, token_url })
    }

    /// The references the source hydrates, for preflight.
    pub fn templates(&self) -> [&Template; 3] {
        [&self.oauth.refresh_token, &self.oauth.client_id, &self.oauth.client_secret]
    }

    fn roots(&self) -> Vec<String> {
        let mut roots = self.folder_ids.clone().unwrap_or_else(|| vec![self.folder_id.clone().expect("validated single root")]);
        roots.sort();
        roots
    }

    /// Refuse a table the source does not serve, ahead of any request
    /// (`connector.source.drive-table-unmatched`).
    pub fn table(&self, table: &str) -> Result<Table, ConnectorError> {
        match table {
            "files" => Ok(Table::Files),
            "pages" => Ok(Table::Pages),
            other => Err(ConnectorError::ConnectorTableUnmatched(format!("the `{NAME}` source serves {}, not `{other}`", TABLES.join(" and ")))),
        }
    }

    fn api(&self, path: &str) -> Url {
        let mut url = self.api_base.clone();
        url.set_path(&format!("{}{path}", self.api_base.path().trim_end_matches('/')));
        url
    }
}

/// A table the source serves.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Table {
    Files,
    Pages,
}

/// Where a body read whole lands, content-addressed by the SHA-256 of its bytes
/// (`store.lay-out.landed-blob`).
pub trait BodyStore: Send + Sync {
    fn put(&self, sha256: &str, bytes: &[u8]) -> Result<(), Failure>;
}

pub use crate::boundary::PageDecoder;

/// One file the walk found under the root.
#[derive(Debug, Clone)]
struct Entry {
    id: String,
    name: String,
    path: String,
    resolved_root: String,
    mime: String,
    modified: String,
    version: Option<i64>,
    size: Option<u64>,
    md5: Option<String>,
}

impl Entry {
    /// Why the source reads no bytes for this file, before any download.
    fn declined(&self, cap: u64) -> Option<String> {
        if self.mime == SHORTCUT {
            return Some("a shortcut, which the walk does not follow".into());
        }
        if self.mime.starts_with(NATIVE) && !EXPORTED.contains(&self.mime.as_str()) {
            return Some(format!("`{}` has no PDF export", self.mime));
        }
        match self.size {
            Some(n) if n > cap => Some(over_cap(n, cap)),
            _ => None,
        }
    }

    fn exported(&self) -> bool {
        EXPORTED.contains(&self.mime.as_str())
    }
}

fn over_cap(size: u64, cap: u64) -> String {
    format!("{size} bytes, over max_file_bytes of {cap}")
}

/// What one file's bytes came to.
#[derive(Debug, Clone)]
struct Fetched {
    sha256: Option<String>,
    bytes: Option<u64>,
    pages: Option<Vec<String>>,
    skipped: Option<String>,
}

/// One fire over a drive: its clients, its minted token, and the walk and bodies both
/// tables of the fire share.
pub struct Drive {
    config: DriveConfig,
    resolver: Arc<Resolver>,
    decoder: Arc<dyn PageDecoder>,
    bodies: Arc<dyn BodyStore>,
    api: Client,
    download: Client,
    token: Client,
    minted: Mutex<Option<(Hydrated, Instant)>>,
    walked: Mutex<Option<Arc<Vec<Entry>>>>,
    fetched: Mutex<BTreeMap<(String, String), Fetched>>,
}

impl Drive {
    pub fn new(config: DriveConfig, resolver: Arc<Resolver>, decoder: Arc<dyn PageDecoder>, bodies: Arc<dyn BodyStore>) -> Result<Arc<Drive>, ConnectorError> {
        let client = |url: &Url| -> Result<Client, ConnectorError> {
            let allow = Allowlist::parse(&[url.host_str().unwrap_or_default()])?;
            allow.check_bound()?;
            let mut origin = url.clone();
            origin.set_path("/");
            Ok(Client::new(allow, origin))
        };
        let api = client(&config.api_base)?;
        let download = client(&config.api_base)?.with_body_limit(config.max_file_bytes);
        let token = client(&config.token_url)?;
        Ok(Arc::new(Drive { config, resolver, decoder, bodies, api, download, token, minted: Mutex::default(), walked: Mutex::default(), fetched: Mutex::default() }))
    }

    /// The source landing `table` from this fire.
    pub fn source(self: &Arc<Drive>, table: &str) -> Result<DriveSource, ConnectorError> {
        Ok(DriveSource { drive: self.clone(), table: self.config.table(table)? })
    }

    /// Header names that carried a credential, by name only.
    pub fn sensitive_headers(&self) -> Vec<String> {
        let mut names = self.api.sensitive_headers();
        names.extend(self.download.sensitive_headers());
        names.sort();
        names.dedup();
        names
    }

    /// Exchange the refresh token for an access token, held in memory for this fire.
    fn mint(&self) -> Result<Hydrated, Failure> {
        let o = &self.config.oauth;
        let (refresh, id, secret) = (self.resolver.render(&o.refresh_token)?, self.resolver.render(&o.client_id)?, self.resolver.render(&o.client_secret)?);
        let body = Hydrated::new(
            url::form_urlencoded::Serializer::new(String::new())
                .append_pair("grant_type", "refresh_token")
                .append_pair("client_id", id.reveal())
                .append_pair("client_secret", secret.reveal())
                .append_pair("refresh_token", refresh.reveal())
                .finish(),
        );
        let headers = [("Content-Type".to_string(), HeaderValue::Plain("application/x-www-form-urlencoded".into()))];
        let resp = self.token.send_once("POST", &self.config.token_url, &headers, Some(body.reveal().as_bytes()))?;
        if !(200..300).contains(&resp.status) {
            return Err(classify(resp.status, None, &format!("the token exchange at `{}`", scrub(&resp.url))));
        }
        let answer: Value = serde_json::from_slice(&resp.body).map_err(|e| Failure::new(FailureTag::Permanent, format!("the token exchange answered no JSON: {e}")))?;
        let token = answer.get("access_token").and_then(Value::as_str).filter(|t| !t.is_empty()).ok_or_else(|| Failure::new(FailureTag::Permanent, "the token exchange answered no `access_token`"))?;
        let lifetime = Duration::from_secs(answer.get("expires_in").and_then(Value::as_u64).unwrap_or(0));
        let bearer = Hydrated::new(format!("Bearer {token}"));
        if let Ok(mut m) = self.minted.lock() {
            *m = Some((bearer.clone(), Instant::now() + lifetime));
        }
        Ok(bearer)
    }

    /// The held token, minted again when it is absent or near its expiry.
    fn bearer(&self) -> Result<Hydrated, Failure> {
        let held = self.minted.lock().ok().and_then(|m| m.clone());
        match held {
            Some((b, until)) if Instant::now() + EXPIRY_MARGIN < until => Ok(b),
            _ => self.mint(),
        }
    }

    /// A GET under the minted token; a `401` mints once more and repeats the request.
    fn get(&self, client: &Client, url: &Url) -> Result<Response, Failure> {
        let mut bearer = self.bearer()?;
        for attempt in 0..2 {
            let headers = [("Authorization".to_string(), HeaderValue::Sensitive(bearer.clone()))];
            let resp = client.send("GET", url, &headers, None)?;
            if resp.status == 401 && attempt == 0 {
                bearer = self.mint()?;
                continue;
            }
            return Ok(resp);
        }
        unreachable!("the loop returns on its second attempt")
    }

    fn get_json(&self, url: &Url) -> Result<Value, Failure> {
        let resp = self.get(&self.api, url)?;
        if !(200..300).contains(&resp.status) {
            let retry_after = resp.header("retry-after").and_then(|v| v.trim().parse().ok());
            return Err(classify(resp.status, retry_after, &scrub(&resp.url)));
        }
        serde_json::from_slice(&resp.body).map_err(|e| Failure::new(FailureTag::Permanent, format!("`{}` answered no JSON: {e}", scrub(&resp.url))))
    }

    /// The tree under the root, breadth-first, each folder's children sorted by name and
    /// id; folders are visited once and shortcuts are listed, never followed.
    fn walk(&self, cancel: &dyn Cancellation) -> Result<Arc<Vec<Entry>>, Failure> {
        if let Some(w) = self.walked.lock().ok().and_then(|w| w.clone()) {
            return Ok(w);
        }
        let config_fault = |why: String| Failure::deterministic(FailureTag::Config, format!("`{NAME}` source: {why}"));
        let roots = self.config.roots();
        // Validate the full selection before the first files.list request.
        for id in &roots {
            let mut root = self.config.api(&format!("/drive/v3/files/{id}"));
            root.query_pairs_mut().append_pair("fields", "id,name,mimeType,driveId").append_pair("supportsAllDrives", "true");
            let meta = self.get_json(&root).map_err(|f| {
                if f.message.contains("answered 404") {
                    if self.config.folder_ids.is_some() {
                        Failure::deterministic(FailureTag::Config, ConnectorError::ConnectorDriveRootRejected(format!("root `{id}` names no folder this credential sees")).to_string())
                    } else {
                        config_fault(format!("`folder_id` `{id}` names no folder this credential sees"))
                    }
                } else {
                    f
                }
            })?;
            let wrong_kind = meta.get("mimeType").and_then(Value::as_str) != Some(FOLDER);
            let wrong_drive = self.config.drive_id.as_ref().is_some_and(|d| meta.get("driveId").and_then(Value::as_str) != Some(d));
            if wrong_kind || wrong_drive {
                if self.config.folder_ids.is_some() {
                    return Err(Failure::deterministic(FailureTag::Config, ConnectorError::ConnectorDriveRootRejected(format!("root `{id}` is not a folder in drive `{}`", self.config.drive_id.as_deref().unwrap_or_default())).to_string()));
                }
                if wrong_kind {
                    return Err(config_fault(format!("`folder_id` `{id}` is not a folder")));
                }
                return Err(config_fault(format!("`folder_id` `{id}` is not in drive `{}`", self.config.drive_id.as_deref().unwrap_or_default())));
            }
        }
        let mut entries = Vec::new();
        let selected = roots.iter().cloned().collect::<BTreeSet<_>>();
        let mut seen = selected.clone();
        let mut queue = roots.iter().map(|id| (id.clone(), String::new(), id.clone())).collect::<VecDeque<_>>();
        let mut selected_edges = Vec::new();
        let mut requests = 0usize;
        while let Some((folder, prefix, owner)) = queue.pop_front() {
            let mut children: Vec<Value> = Vec::new();
            let mut token: Option<String> = None;
            loop {
                if cancel.requested() {
                    return Err(Failure::canceled("stopped during the folder walk"));
                }
                requests += 1;
                if requests > LISTING_CAP {
                    return Err(Failure::new(FailureTag::Permanent, ConnectorError::ConnectorDriveListingExceeded(format!("folder `{folder}` remains incomplete at {LISTING_CAP} listing requests")).to_string()));
                }
                let mut url = self.config.api("/drive/v3/files");
                {
                    let mut q = url.query_pairs_mut();
                    q.append_pair("q", &format!("'{folder}' in parents and trashed = false"))
                        .append_pair("fields", &format!("nextPageToken,files({FILE_FIELDS})"))
                        .append_pair("pageSize", "1000")
                        .append_pair("supportsAllDrives", "true")
                        .append_pair("includeItemsFromAllDrives", "true");
                    if let Some(d) = &self.config.drive_id {
                        q.append_pair("corpora", "drive").append_pair("driveId", d);
                    }
                    if let Some(t) = &token {
                        q.append_pair("pageToken", t);
                    }
                }
                let page = self.get_json(&url)?;
                children.extend(page.get("files").and_then(Value::as_array).cloned().unwrap_or_default());
                token = page.get("nextPageToken").and_then(Value::as_str).filter(|t| !t.is_empty()).map(str::to_string);
                if token.is_none() {
                    break;
                }
            }
            let text = |v: &Value, k: &str| v.get(k).and_then(Value::as_str).map(str::to_string);
            children.sort_by_key(|c| (text(c, "name").unwrap_or_default(), text(c, "id").unwrap_or_default()));
            for c in children {
                let (Some(id), Some(name), Some(mime)) = (text(&c, "id"), text(&c, "name"), text(&c, "mimeType")) else { continue };
                let path = if prefix.is_empty() { name.clone() } else { format!("{prefix}/{name}") };
                if mime == FOLDER {
                    if selected.contains(&id) && id != owner {
                        selected_edges.push((owner.clone(), id.clone(), path.clone()));
                    }
                    if seen.insert(id.clone()) {
                        queue.push_back((id, path, owner.clone()));
                    }
                    continue;
                }
                entries.push(Entry {
                    version: text(&c, "version").and_then(|v| v.parse().ok()),
                    size: text(&c, "size").and_then(|v| v.parse().ok()),
                    md5: text(&c, "md5Checksum"),
                    modified: text(&c, "modifiedTime").unwrap_or_default(),
                    id,
                    name,
                    path,
                    resolved_root: owner.clone(),
                    mime,
                });
            }
        }
        // A selected folder can sit inside another selected root. Resolve the least
        // ancestor root without issuing a second listing of that folder.
        let mut preferred = roots.iter().map(|id| (id.clone(), (id.clone(), String::new()))).collect::<BTreeMap<_, _>>();
        for _ in 0..roots.len() {
            let mut changed = false;
            for (parent, child, path) in &selected_edges {
                let Some((winner, prefix)) = preferred.get(parent).cloned() else { continue };
                let candidate = (winner, if prefix.is_empty() { path.clone() } else { format!("{prefix}/{path}") });
                if preferred.get(child).is_none_or(|current| candidate < *current) {
                    preferred.insert(child.clone(), candidate);
                    changed = true;
                }
            }
            if !changed { break; }
        }
        let mut unique = BTreeMap::<String, Entry>::new();
        for mut entry in entries {
            if let Some((winner, prefix)) = preferred.get(&entry.resolved_root) {
                if !prefix.is_empty() { entry.path = format!("{prefix}/{}", entry.path); }
                entry.resolved_root = winner.clone();
            }
            if unique.get(&entry.id).is_none_or(|prior| (&entry.resolved_root, &entry.path) < (&prior.resolved_root, &prior.path)) {
                unique.insert(entry.id.clone(), entry);
            }
        }
        let mut entries = unique.into_values().collect::<Vec<_>>();
        entries.sort_by(|a, b| (&a.path, &a.id).cmp(&(&b.path, &b.id)));
        let walked = Arc::new(entries);
        if let Ok(mut w) = self.walked.lock() {
            *w = Some(walked.clone());
        }
        Ok(walked)
    }

    /// One file's bytes: its PDF export or its media, capped, digested and decoded.
    fn fetch(&self, e: &Entry) -> Result<Fetched, Failure> {
        let key = (e.id.clone(), e.modified.clone());
        if let Some(f) = self.fetched.lock().ok().and_then(|m| m.get(&key).cloned()) {
            return Ok(f);
        }
        let cap = self.config.max_file_bytes;
        let fetched = match e.declined(cap) {
            Some(why) => Fetched { sha256: None, bytes: None, pages: None, skipped: Some(why) },
            None => {
                let mut url = self.config.api(&format!("/drive/v3/files/{}{}", e.id, if e.exported() { "/export" } else { "" }));
                if e.exported() {
                    url.query_pairs_mut().append_pair("mimeType", PDF);
                } else {
                    url.query_pairs_mut().append_pair("alt", "media").append_pair("supportsAllDrives", "true");
                }
                match self.get(&self.download, &url) {
                    // An export reports no size ahead; its body stops at the cap.
                    Err(f) if is_over_limit(&f) => Fetched { sha256: None, bytes: None, pages: None, skipped: Some(format!("more than {cap} bytes, over max_file_bytes")) },
                    Err(f) => return Err(f),
                    // Drive refuses this export on every fire, so it skips that file alone
                    // (`connector.source.drive-export-limit`).
                    Ok(resp) if e.exported() && resp.status == 403 && export_limited(&resp.body) => {
                        Fetched { sha256: None, bytes: None, pages: None, skipped: Some(format!("Drive refuses the PDF export as `{EXPORT_LIMIT}`, over its export size limit")) }
                    }
                    Ok(resp) if !(200..300).contains(&resp.status) => {
                        let retry_after = resp.header("retry-after").and_then(|v| v.trim().parse().ok());
                        return Err(classify(resp.status, retry_after, &scrub(&resp.url)));
                    }
                    Ok(resp) => {
                        let digest = hex(&Sha256::digest(&resp.body));
                        let (sha256, bytes) = (Some(digest), Some(resp.body.len() as u64));
                        if self.config.mode == CaptureMode::MetadataOnly {
                            Fetched { sha256, bytes, pages: None, skipped: None }
                        } else if !(e.exported() || e.mime == PDF) {
                            self.bodies.put(sha256.as_deref().unwrap(), &resp.body)?;
                            Fetched { sha256, bytes, pages: None, skipped: None }
                        } else {
                            self.bodies.put(sha256.as_deref().unwrap(), &resp.body)?;
                            // One body the decoder refuses or crashes on skips that file alone
                            // (`connector.source.drive-unreadable`); a stop still ends the read.
                            match self.decoder.pages(&resp.body, &e.path) {
                                Ok(pages) => Fetched { sha256, bytes, pages: Some(pages), skipped: None },
                                Err(f) if f.tag == FailureTag::Canceled => return Err(f),
                                Err(f) => Fetched { sha256, bytes, pages: None, skipped: Some(format!("the PDF did not decode: {}", f.message)) },
                            }
                        }
                    }
                }
            }
        };
        if let Ok(mut m) = self.fetched.lock() {
            m.insert(key, fetched.clone());
        }
        Ok(fetched)
    }
}

/// Whether a Drive error body lists `exportSizeLimitExceeded` among its `error.errors` reasons.
fn export_limited(body: &[u8]) -> bool {
    let v: Value = serde_json::from_slice(body).unwrap_or_default();
    v.pointer("/error/errors").and_then(Value::as_array).is_some_and(|errors| errors.iter().any(|e| e.get("reason").and_then(Value::as_str) == Some(EXPORT_LIMIT)))
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// What the position holds for one file.
#[derive(Debug, Clone, Default)]
struct Held {
    modified: String,
    path: String,
    name: String,
    pages: u64,
}

fn held(position: Option<&Value>) -> BTreeMap<String, Held> {
    let Some(files) = position.and_then(|p| p.get("files")).and_then(Value::as_object) else { return BTreeMap::new() };
    files
        .iter()
        .map(|(id, v)| {
            let text = |k: &str| v.get(k).and_then(Value::as_str).unwrap_or_default().to_string();
            (id.clone(), Held { modified: text("modified"), path: text("path"), name: text("name"), pages: v.get("pages").and_then(Value::as_u64).unwrap_or(0) })
        })
        .collect()
}

/// One read of a drive table.
#[derive(Debug, Clone)]
pub struct Read {
    pub rows: Vec<Row>,
    pub position: Value,
    /// Files the read lands with a `skipped` reason.
    pub skipped: u64,
}

/// The source landing one table of a drive fire.
pub struct DriveSource {
    pub drive: Arc<Drive>,
    pub table: Table,
}

impl DriveSource {
    fn file_row(e: &Entry, f: &Fetched) -> Row {
        let mut r = Map::new();
        r.insert("file_id".into(), json!(e.id));
        r.insert("version".into(), json!(e.version));
        r.insert("name".into(), json!(e.name));
        r.insert("path".into(), json!(e.path));
        r.insert("resolved_root".into(), json!(e.resolved_root));
        r.insert("mime_type".into(), json!(e.mime));
        r.insert("export_mime_type".into(), if e.exported() && f.sha256.is_some() { json!(PDF) } else { Value::Null });
        r.insert("modified_time".into(), json!(e.modified));
        r.insert("md5_checksum".into(), json!(e.md5));
        r.insert("sha256".into(), json!(f.sha256));
        r.insert("bytes".into(), json!(f.bytes));
        r.insert("pages".into(), json!(f.pages.as_ref().map(Vec::len)));
        r.insert("skipped".into(), json!(f.skipped));
        r.insert("capture_status".into(), json!(if f.skipped.is_some() { "skipped" } else { "captured" }));
        r.insert("removed".into(), json!(false));
        r
    }

    fn tombstone(id: &str, h: &Held) -> Row {
        let mut r = Map::new();
        for k in ["version", "mime_type", "export_mime_type", "md5_checksum", "sha256", "bytes", "pages", "skipped"] {
            r.insert(k.into(), Value::Null);
        }
        r.insert("file_id".into(), json!(id));
        r.insert("name".into(), json!(h.name));
        r.insert("path".into(), json!(h.path));
        r.insert("modified_time".into(), json!(h.modified));
        r.insert("removed".into(), json!(true));
        r.insert("capture_status".into(), json!("removed"));
        r
    }

    fn page_row(id: &str, version: Option<i64>, path: &str, page: u64, text: Option<&str>) -> Row {
        let mut r = Map::new();
        r.insert("file_id".into(), json!(id));
        r.insert("page".into(), json!(page));
        r.insert("version".into(), json!(version));
        r.insert("path".into(), json!(path));
        r.insert("text".into(), json!(text));
        r.insert("removed".into(), json!(text.is_none()));
        r
    }

    fn types(&self) -> BTreeMap<String, String> {
        let spelled: &[(&str, &str)] = match self.table {
            Table::Files => &[
                ("file_id", "utf8"),
                ("version", "int64"),
                ("name", "utf8"),
                ("path", "utf8"),
                ("resolved_root", "utf8"),
                ("mime_type", "utf8"),
                ("export_mime_type", "utf8"),
                ("modified_time", "utf8"),
                ("md5_checksum", "utf8"),
                ("sha256", "utf8"),
                ("bytes", "int64"),
                ("pages", "int64"),
                ("skipped", "utf8"),
                ("capture_status", "utf8"),
                ("removed", "boolean"),
            ],
            Table::Pages => &[("file_id", "utf8"), ("page", "int64"), ("version", "int64"), ("path", "utf8"), ("text", "utf8"), ("removed", "boolean")],
        };
        spelled.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect()
    }

    /// The rows of this table since `position`, the position after them, and the files they
    /// land with a `skipped` reason.
    pub fn read(&self, position: Option<&Value>, cancel: &dyn Cancellation) -> Result<Read, Failure> {
        let before = held(position);
        let previous_mode = position.and_then(|p| p.get("mode")).and_then(Value::as_str).unwrap_or("bytes-and-pages");
        let mode_changed = previous_mode != self.drive.config.mode.as_str();
        let entries = self.drive.walk(cancel)?;
        let mut rows = Vec::new();
        let mut after = Map::new();
        let mut skipped = 0u64;
        for e in entries.iter() {
            if cancel.requested() {
                return Err(Failure::canceled("stopped between files"));
            }
            let prior = before.get(&e.id);
            let unchanged = !mode_changed && prior.is_some_and(|h| h.modified == e.modified && h.path == e.path);
            let mut pages = prior.map_or(0, |h| h.pages);
            if !unchanged {
                let f = self.drive.fetch(e)?;
                skipped += u64::from(f.skipped.is_some());
                let landed = f.pages.as_ref().map_or(0, |p| p.len() as u64);
                match self.table {
                    Table::Files => rows.push(Self::file_row(e, &f)),
                    Table::Pages => {
                        for (i, text) in f.pages.iter().flatten().enumerate() {
                            rows.push(Self::page_row(&e.id, e.version, &e.path, i as u64 + 1, Some(text)));
                        }
                        for p in landed + 1..=pages {
                            rows.push(Self::page_row(&e.id, e.version, &e.path, p, None));
                        }
                    }
                }
                pages = landed;
            }
            after.insert(e.id.clone(), json!({"modified": e.modified, "path": e.path, "name": e.name, "pages": pages}));
        }
        for (id, h) in before.iter().filter(|(id, _)| !after.contains_key(*id)) {
            match self.table {
                Table::Files => rows.push(Self::tombstone(id, h)),
                Table::Pages => rows.extend((1..=h.pages).map(|p| Self::page_row(id, None, &h.path, p, None))),
            }
        }
        Ok(Read { rows, position: json!({ "files": after, "mode": self.drive.config.mode.as_str() }), skipped })
    }
}

impl Source for DriveSource {
    /// One pull is one whole walk; the position rides the pull as an opaque token.
    fn pull(&mut self, request: &PullRequest, cancel: &dyn Cancellation) -> Result<Vec<u8>, Failure> {
        let read = self.read(request.position.as_ref(), cancel)?;
        // The skipped files are the pull's tally on the run record (`connector.source.drive-skip-count`).
        serde_json::to_vec(&json!({ "rows": read.rows, "cursor": read.position, "more": false, "types": self.types(), "skipped": read.skipped })).map_err(|e| Failure::new(FailureTag::Permanent, e.to_string()))
    }
}
