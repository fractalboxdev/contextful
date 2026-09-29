//! The `s3` object source: one bucket, read by a fixed key or a prefix listing, each object
//! decompressed under a ceiling and decoded through the shared decoders
//! (`connector.source.object-address`, `connector.source.prefix-listing`).
//!
//! Requests are signed with the S3 client the bucket adapter signs with, as Signature
//! Version 4 query signatures, and sent through the mediated client, so the bucket is one
//! more allowlisted vendor host (`connector.source.object-transport`).

use crate::decode::{decode, gunzip, workbook, Format};
use crate::http::ConfigError;
use contextful_core::connector::attach::{endpoint, is_loopback_host, scrub, Allowlist};
use contextful_core::connector::reference::{check_material, Hydrated, Part, SecretName, Template};
use contextful_core::connector::ConnectorError;
use contextful_core::run::ports::{Cancellation, PullRequest, Row, Source};
use contextful_core::run::{Failure, FailureTag, RunError};
use contextful_core::store::object::{ObjectError, ObjectStore};
use contextful_outbound::client::{classify, Client};
use contextful_outbound::Resolver;
use rusty_s3::actions::{ListObjectsV2, S3Action};
use rusty_s3::{Bucket, Credentials, UrlStyle};
use serde_json::{Map, Value};
use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;
use url::Url;

/// The source's registered name.
pub const NAME: &str = "s3";
/// Decompressed bytes of one object: 256 MiB (`connector.source.object-expansion`).
pub const EXPANSION_CEILING: u64 = 256 * 1024 * 1024;
/// The endpoint a source declaring none reads.
pub const DEFAULT_ENDPOINT: &str = "https://s3.amazonaws.com";
/// The signing region a source declaring none signs for.
pub const DEFAULT_REGION: &str = "us-east-1";
/// Life of one signed URL; each request is signed immediately before it is sent.
const SIGNATURE_TTL: Duration = Duration::from_secs(300);
/// Keys one listing page asks for; a backend may answer fewer.
const LIST_PAGE: usize = 1000;

/// The configuration keys the object source reads (`run.declare.config-key`).
pub const KEYS: [&str; 13] = [
    "bucket",
    "key",
    "prefix",
    "suffix",
    "pick",
    "endpoint",
    "region",
    "format",
    "records",
    "compression",
    "skip_unchanged",
    "access_key_id",
    "secret_access_key",
];

/// Which objects a read lands.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Address {
    /// One object at a fixed key.
    Key(String),
    /// Every key under `prefix` ending in `suffix`, in key order; `latest` keeps the greatest.
    Prefix { prefix: String, suffix: Option<String>, latest: bool },
}

/// How an object's bytes are stored.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Compression {
    /// Gzip where the key ends `.gz`, as stored otherwise.
    ByKey,
    Gzip,
    None,
}

/// A parsed object source configuration.
#[derive(Debug, Clone)]
pub struct ObjectConfig {
    pub bucket: String,
    pub address: Address,
    /// The endpoint objects are addressed under, path-style: an R2 account endpoint, a
    /// MinIO host, or [`DEFAULT_ENDPOINT`].
    pub endpoint: Url,
    pub region: String,
    pub format: Format,
    pub records: Option<String>,
    pub compression: Compression,
    pub skip_unchanged: bool,
    /// The access key id and secret access key references, declared together or not at all.
    pub access_key_id: Option<Template>,
    pub secret_access_key: Option<Template>,
}

fn invalid(why: String) -> ConfigError {
    RunError::Invalid(format!("the `{NAME}` source {why}")).into()
}

fn text(cfg: &Map<String, Value>, k: &str) -> Result<Option<String>, ConfigError> {
    match cfg.get(k) {
        None => Ok(None),
        Some(Value::String(s)) => Ok(Some(s.clone())),
        Some(other) => Err(invalid(format!("key `{k}` is a string, found {other}"))),
    }
}

fn flag(cfg: &Map<String, Value>, k: &str) -> Result<bool, ConfigError> {
    match cfg.get(k) {
        None => Ok(false),
        Some(Value::Bool(b)) => Ok(*b),
        Some(other) => Err(invalid(format!("key `{k}` is a boolean, found {other}"))),
    }
}

/// A credential key's value: one `secret://<name>` reference, whole or as a template holding
/// nothing else. A literal refuses as material in a declaration
/// (`connector.source.object-credentials`).
fn credential(key: &str, value: &str) -> Result<Template, ConfigError> {
    let material = || ConnectorError::SecretMaterialInDeclaration(format!("`{key}` holds credential material; bind it as `secret://<name>`"));
    let template = match value.strip_prefix("secret://") {
        Some(name) => {
            SecretName::parse(name)?;
            Template::parse(&format!("${{{value}}}"))?
        }
        None => check_material(key, value)?,
    };
    // A template carrying literal text beside its reference sends that text as key material.
    if !matches!(template.parts.as_slice(), [Part::Secret(_)]) {
        return Err(material().into());
    }
    Ok(template)
}

/// The format a key's extension names, a trailing `.gz` set aside.
fn format_of(name: &str) -> Option<Format> {
    let name = name.strip_suffix(".gz").unwrap_or(name);
    let ext = name.rsplit_once('.')?.1;
    match ext {
        "jsonl" | "ndjson" => Some(Format::Jsonl),
        "json" => Some(Format::Json),
        "csv" => Some(Format::Csv),
        "xlsx" => Some(Format::Workbook),
        _ => None,
    }
}

impl ObjectConfig {
    /// Parse and check a source configuration before any I/O.
    pub fn parse(config: &Value) -> Result<ObjectConfig, ConfigError> {
        let cfg = config.as_object().ok_or_else(|| invalid("config is an object".into()))?;
        if let Some(k) = cfg.keys().find(|k| !KEYS.contains(&k.as_str())) {
            return Err(RunError::PipelineUnknownConfigKey(format!("the `{NAME}` source reads no key `{k}`; it reads {}", KEYS.join(", "))).into());
        }
        let bucket = text(cfg, "bucket")?.filter(|b| !b.is_empty()).ok_or_else(|| invalid("names no `bucket`".into()))?;
        let (key, prefix) = (text(cfg, "key")?, text(cfg, "prefix")?);
        let (suffix, pick) = (text(cfg, "suffix")?, text(cfg, "pick")?);
        let address = match (key, prefix) {
            (Some(key), None) => {
                if let Some(k) = [suffix.as_ref().map(|_| "suffix"), pick.as_ref().map(|_| "pick")].into_iter().flatten().next() {
                    return Err(invalid(format!("`{k}` narrows a prefix listing and the source reads one `key`")));
                }
                if key.is_empty() {
                    return Err(invalid("`key` is empty".into()));
                }
                Address::Key(key)
            }
            (None, Some(prefix)) => {
                let latest = match pick.as_deref() {
                    None | Some("all") => false,
                    Some("latest") => true,
                    Some(other) => return Err(invalid(format!("`pick = \"{other}\"` is neither `all` nor `latest`"))),
                };
                Address::Prefix { prefix, suffix, latest }
            }
            (key, prefix) => {
                let found = if key.is_some() && prefix.is_some() { "both" } else { "neither" };
                return Err(ConnectorError::ConnectorObjectAddressRejected(format!("bucket `{bucket}` declares {found} of `key` and `prefix`; a read names exactly one")).into());
            }
        };
        let named = match &address {
            Address::Key(k) => Some(k.as_str()),
            Address::Prefix { suffix, .. } => suffix.as_deref(),
        };
        let format = match text(cfg, "format")? {
            Some(f) => Format::parse(&f)?,
            None => named.and_then(format_of).unwrap_or(Format::Json),
        };
        let records = text(cfg, "records")?;
        if records.is_some() && format != Format::Json {
            return Err(ConnectorError::ConnectorFormatKeyRejected(format!("`records` reads a JSON body and the source's format is `{}`", format.name())).into());
        }
        let compression = match text(cfg, "compression")?.as_deref() {
            None => Compression::ByKey,
            Some("gzip") => Compression::Gzip,
            Some("none") => Compression::None,
            Some(other) => return Err(invalid(format!("`compression = \"{other}\"` is neither `gzip` nor `none`"))),
        };
        let access_key_id = text(cfg, "access_key_id")?.map(|v| credential("access_key_id", &v)).transpose()?;
        let secret_access_key = text(cfg, "secret_access_key")?.map(|v| credential("secret_access_key", &v)).transpose()?;
        if access_key_id.is_some() != secret_access_key.is_some() {
            return Err(invalid("declares one of `access_key_id` and `secret_access_key`; a key pair binds both".into()));
        }
        let endpoint = endpoint(NAME, text(cfg, "endpoint")?.as_deref().unwrap_or(DEFAULT_ENDPOINT))?;
        if endpoint.query().is_some() || endpoint.fragment().is_some() {
            return Err(invalid(format!("`endpoint = \"{}\"` carries a query or fragment; objects address under a bare origin and path", scrub(&endpoint))));
        }
        // A signed URL is a bearer capability for its lifetime, so it never travels in cleartext
        // off loopback (`connector.source.object-transport`).
        if access_key_id.is_some() && endpoint.scheme() != "https" && !endpoint.host_str().is_some_and(is_loopback_host) {
            return Err(ConnectorError::SecretCleartextEndpoint(format!("the `{NAME}` source signs its requests and `{}` is cleartext", scrub(&endpoint))).into());
        }
        Bucket::new(endpoint.clone(), UrlStyle::Path, bucket.clone(), DEFAULT_REGION).map_err(|e| invalid(format!("bucket `{bucket}` at `{}`: {e}", scrub(&endpoint))))?;
        Ok(ObjectConfig {
            bucket,
            address,
            endpoint,
            region: text(cfg, "region")?.unwrap_or_else(|| DEFAULT_REGION.to_string()),
            format,
            records,
            compression,
            skip_unchanged: flag(cfg, "skip_unchanged")?,
            access_key_id,
            secret_access_key,
        })
    }

    /// The credential templates the source hydrates per read.
    pub fn credentials(&self) -> impl Iterator<Item = &Template> {
        self.access_key_id.iter().chain(self.secret_access_key.iter())
    }

    /// The name an object refuses under: `s3://<bucket>/<key>`.
    pub fn input(&self, key: &str) -> String {
        format!("s3://{}/{key}", self.bucket)
    }

    fn gzipped(&self, key: &str) -> bool {
        match self.compression {
            Compression::Gzip => true,
            Compression::None => false,
            Compression::ByKey => key.ends_with(".gz"),
        }
    }
}

/// The objects one source reads: a sorted listing, each key with the ETag the listing
/// reports where it reports one, and a fetch returning the bytes and their ETag.
pub trait Objects: Send + Sync {
    fn list(&self, prefix: &str) -> Result<Vec<(String, Option<String>)>, Failure>;
    /// The object's ETag without its bytes, where the backend answers one.
    fn etag(&self, key: &str) -> Result<Option<String>, Failure>;
    fn get(&self, key: &str) -> Result<Option<(Vec<u8>, String)>, Failure>;
}

/// An [`ObjectStore`] read as [`Objects`]; its listing reports no ETag.
struct Port {
    store: Arc<dyn ObjectStore>,
    bucket: String,
}

impl Port {
    fn failure(&self, key: &str, e: ObjectError) -> Failure {
        let input = format!("s3://{}/{key}", self.bucket);
        match e {
            ObjectError::Forbidden(m) => Failure::new(FailureTag::AuthExpired, format!("`{input}`: the bucket refused the credential: {m}")),
            ObjectError::Unsupported(m) => Failure::deterministic(FailureTag::Permanent, format!("`{input}`: {m}")),
            ObjectError::Transport(m) => Failure::new(FailureTag::Transient, format!("`{input}`: {m}")),
        }
    }
}

impl Objects for Port {
    fn list(&self, prefix: &str) -> Result<Vec<(String, Option<String>)>, Failure> {
        Ok(self.store.list(prefix).map_err(|e| self.failure(prefix, e))?.into_iter().map(|k| (k, None)).collect())
    }
    fn etag(&self, _: &str) -> Result<Option<String>, Failure> {
        Ok(None)
    }
    fn get(&self, key: &str) -> Result<Option<(Vec<u8>, String)>, Failure> {
        self.store.get(key).map_err(|e| self.failure(key, e))
    }
}

/// A bucket on an S3-compatible endpoint, each request signed and sent through the mediated
/// client. It reads; it never writes.
pub struct SignedBucket {
    bucket: Bucket,
    credentials: Option<Credentials>,
    client: Arc<Client>,
}

impl SignedBucket {
    /// Open `config`'s bucket with `credentials`, hydrated for this read.
    pub fn open(config: &ObjectConfig, credentials: Option<(Hydrated, Hydrated)>) -> Result<SignedBucket, Failure> {
        let deny = |m: String| Failure::deterministic(FailureTag::Config, m);
        let bucket = Bucket::new(config.endpoint.clone(), UrlStyle::Path, config.bucket.clone(), config.region.clone()).map_err(|e| deny(format!("`{}`: {e}", scrub(&config.endpoint))))?;
        let allow = Allowlist::parse(&[config.endpoint.host_str().unwrap_or_default()]).map_err(|e| deny(e.to_string()))?;
        let credentials = credentials.map(|(id, secret)| Credentials::new(id.reveal(), secret.reveal()));
        Ok(SignedBucket { bucket, credentials, client: Arc::new(Client::new(allow, config.endpoint.clone())) })
    }

    fn input(&self, key: &str) -> String {
        format!("s3://{}/{key}", self.bucket.name())
    }

    fn send(&self, method: &str, url: &Url, key: &str) -> Result<contextful_outbound::Response, Failure> {
        let resp = self.client.send(method, url, &[], None)?;
        match resp.status {
            200..=299 | 404 => Ok(resp),
            status => {
                let retry_after = resp.header("retry-after").and_then(|v| v.trim().parse().ok());
                Err(classify(status, retry_after, &self.input(key)))
            }
        }
    }

    fn tagged(&self, resp: &contextful_outbound::Response, key: &str) -> Result<String, Failure> {
        resp.header("etag").map(str::to_string).ok_or_else(|| Failure::new(FailureTag::Transient, format!("`{}`: the answer carries no ETag", self.input(key))))
    }
}

impl Objects for SignedBucket {
    fn list(&self, prefix: &str) -> Result<Vec<(String, Option<String>)>, Failure> {
        let mut out = Vec::new();
        let mut token: Option<String> = None;
        for _ in 0..crate::http::PAGE_CAP {
            let mut action = self.bucket.list_objects_v2(self.credentials.as_ref());
            action.with_prefix(prefix.to_string());
            action.with_max_keys(LIST_PAGE);
            if let Some(t) = &token {
                action.with_continuation_token(t.clone());
            }
            let resp = self.send("GET", &action.sign(SIGNATURE_TTL), prefix)?;
            if resp.status == 404 {
                return Err(Failure::deterministic(FailureTag::Permanent, format!("`{}`: no bucket answers", self.input(prefix))));
            }
            let text = String::from_utf8_lossy(&resp.body);
            let page = ListObjectsV2::parse_response(&text).map_err(|e| Failure::new(FailureTag::Transient, format!("`{}`: the listing does not parse: {e}", self.input(prefix))))?;
            out.extend(page.contents.into_iter().map(|c| (c.key, Some(c.etag))));
            match page.next_continuation_token {
                Some(next) if !next.is_empty() && token.as_ref() != Some(&next) => token = Some(next),
                _ => return Ok(out),
            }
        }
        Err(Failure::new(FailureTag::Permanent, format!("the listing of `{}` reached {} pages", self.input(prefix), crate::http::PAGE_CAP)))
    }

    fn etag(&self, key: &str) -> Result<Option<String>, Failure> {
        let url = self.bucket.head_object(self.credentials.as_ref(), key).sign(SIGNATURE_TTL);
        let resp = self.send("HEAD", &url, key)?;
        if resp.status == 404 {
            return Ok(None);
        }
        self.tagged(&resp, key).map(Some)
    }

    fn get(&self, key: &str) -> Result<Option<(Vec<u8>, String)>, Failure> {
        let url = self.bucket.get_object(self.credentials.as_ref(), key).sign(SIGNATURE_TTL);
        let resp = self.send("GET", &url, key)?;
        if resp.status == 404 {
            return Ok(None);
        }
        let tag = self.tagged(&resp, key)?;
        Ok(Some((resp.body, tag)))
    }
}

/// Opens the objects a read reaches; a signed bucket hydrates its credentials on each call.
type Open = Box<dyn Fn() -> Result<Box<dyn Objects>, Failure> + Send>;

/// One object source bound to its bucket.
pub struct ObjectSource {
    pub config: ObjectConfig,
    open: Open,
}

impl ObjectSource {
    /// A source reading `store` through the object-store port.
    pub fn new(config: ObjectConfig, store: Arc<dyn ObjectStore>) -> ObjectSource {
        let bucket = config.bucket.clone();
        ObjectSource { config, open: Box::new(move || Ok(Box::new(Port { store: store.clone(), bucket: bucket.clone() }) as Box<dyn Objects>)) }
    }

    /// A source reading its configured endpoint through the mediated client, its credential
    /// references hydrated per read (`connector.source.object-credentials`).
    pub fn signed(config: ObjectConfig, resolver: Arc<Resolver>) -> ObjectSource {
        let c = config.clone();
        let open: Open = Box::new(move || {
            let credentials = match (&c.access_key_id, &c.secret_access_key) {
                (Some(id), Some(secret)) => Some((resolver.render(id)?, resolver.render(secret)?)),
                _ => None,
            };
            Ok(Box::new(SignedBucket::open(&c, credentials)?) as Box<dyn Objects>)
        });
        ObjectSource { config, open }
    }

    /// The keys one read reaches, in key order, each with a listed ETag where one is reported.
    fn keys(&self, objects: &dyn Objects) -> Result<Vec<(String, Option<String>)>, Failure> {
        match &self.config.address {
            Address::Key(k) => Ok(vec![(k.clone(), None)]),
            Address::Prefix { prefix, suffix, latest } => {
                let mut keys = objects.list(prefix)?;
                keys.retain(|(k, _)| k.starts_with(prefix.as_str()) && suffix.as_deref().is_none_or(|s| k.ends_with(s)));
                keys.sort();
                keys.dedup_by(|a, b| a.0 == b.0);
                if *latest {
                    keys = keys.pop().into_iter().collect();
                }
                Ok(keys)
            }
        }
    }

    /// Read every addressed object from `position`: the rows of each object whose ETag moved,
    /// and the position after them. A failure on any object lands none.
    pub fn read(&self, position: Option<&Value>, cancel: &dyn Cancellation) -> Result<(Vec<Row>, Option<Value>), Failure> {
        let skip = self.config.skip_unchanged;
        let held: BTreeMap<String, String> =
            position.filter(|_| skip).and_then(|p| p.get("objects")).and_then(|o| serde_json::from_value(o.clone()).ok()).unwrap_or_default();
        let objects = (self.open)()?;
        let mut etags = BTreeMap::new();
        let mut rows = Vec::new();
        for (key, listed) in self.keys(objects.as_ref())? {
            if cancel.requested() {
                return Err(Failure::canceled("stopped between objects"));
            }
            let input = self.config.input(&key);
            if let Some(tag) = held.get(&key).filter(|_| skip) {
                // An unchanged ETag costs a listing entry or a head request, never a download.
                let current = match listed {
                    Some(t) => Some(t),
                    None if matches!(self.config.address, Address::Key(_)) => objects.etag(&key)?,
                    None => None,
                };
                if current.as_ref() == Some(tag) {
                    etags.insert(key, tag.clone());
                    continue;
                }
            }
            let (bytes, etag) = objects.get(&key)?.ok_or_else(|| Failure::deterministic(FailureTag::Permanent, format!("`{input}`: no object holds the key")))?;
            let unchanged = held.get(&key) == Some(&etag);
            etags.insert(key.clone(), etag);
            if skip && unchanged {
                continue;
            }
            let body = if self.config.gzipped(&key) { gunzip(&bytes, EXPANSION_CEILING, &input)? } else { bytes };
            let decoded = match self.config.format {
                Format::Workbook => workbook::rows(&body, None, 0, &input)?,
                format => decode(format, &body, self.config.records.as_deref(), &input)?.0,
            };
            rows.extend(decoded);
        }
        Ok((rows, skip.then(|| serde_json::json!({ "objects": etags }))))
    }
}

impl Source for ObjectSource {
    /// One pull reads every addressed object; the position rides the pull as its cursor.
    fn pull(&mut self, request: &PullRequest, cancel: &dyn Cancellation) -> Result<Vec<u8>, Failure> {
        let (rows, cursor) = self.read(request.position.as_ref(), cancel)?;
        serde_json::to_vec(&serde_json::json!({ "rows": rows, "cursor": cursor, "more": false })).map_err(|e| Failure::new(FailureTag::Permanent, e.to_string()))
    }
}
