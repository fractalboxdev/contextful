//! The `s3` object source: one bucket, read by a fixed key or a prefix listing, each object
//! decompressed under a ceiling and decoded through the shared decoders
//! (`connector.source.object-address`, `connector.source.prefix-listing`).
//!
//! A signed read presigns each request through [`Presign`], which the binary implements over
//! the store's S3 bucket adapter, and sends it through the mediated client, so one S3 client
//! signs every bucket request and the bucket is one more allowlisted vendor host
//! (`connector.source.object-transport`). This package links no S3 client.

use crate::decode::{decode_with_encoding, gunzip, workbook, Format};
use crate::http::ConfigError;
use contextful_core::connector::attach::{endpoint, is_loopback_host, scrub, Allowlist};
use contextful_core::connector::reference::{check_material, Hydrated, Part, SecretName, Template};
use contextful_core::connector::ConnectorError;
use contextful_core::run::ports::{Cancellation, PullRequest, Row, Source};
use contextful_core::run::{Failure, FailureTag, RunError};
use contextful_core::store::object::{ObjectError, ObjectStore};
use contextful_outbound::client::{classify, Client};
use contextful_outbound::Resolver;
use serde_json::{Map, Value};
use std::collections::BTreeMap;
use std::sync::Arc;
use url::Url;

/// The source's registered name.
pub const NAME: &str = "s3";
/// Decompressed bytes of one object: 256 MiB (`connector.source.object-expansion`).
pub const EXPANSION_CEILING: u64 = 256 * 1024 * 1024;
/// The endpoint a source declaring none reads.
pub const DEFAULT_ENDPOINT: &str = "https://s3.amazonaws.com";
/// The signing region a source declaring none signs for.
pub const DEFAULT_REGION: &str = "us-east-1";

/// The configuration keys the object source reads (`run.declare.config-key`).
pub const KEYS: [&str; 14] = [
    "bucket",
    "key",
    "prefix",
    "suffix",
    "pick",
    "endpoint",
    "region",
    "format",
    "encoding",
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
    /// The character encoding of a CSV object; absent means UTF-8.
    pub encoding: Option<String>,
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
        if records.is_some() && !matches!(format, Format::Json | Format::JsonObject) {
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
        Ok(ObjectConfig {
            bucket,
            address,
            endpoint,
            region: text(cfg, "region")?.unwrap_or_else(|| DEFAULT_REGION.to_string()),
            format,
            encoding,
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

/// The failure an object-store error on `input` raises: a refused credential expires the
/// read's authority, an unsupported request is permanent, and transport is transient.
fn object_failure(input: &str, e: ObjectError) -> Failure {
    match e {
        ObjectError::Forbidden(m) => Failure::new(FailureTag::AuthExpired, format!("`{input}`: the bucket refused the credential: {m}")),
        ObjectError::Unsupported(m) => Failure::deterministic(FailureTag::Permanent, format!("`{input}`: {m}")),
        ObjectError::Transport(m) => Failure::new(FailureTag::Transient, format!("`{input}`: {m}")),
    }
}

/// An [`ObjectStore`] read as [`Objects`]; its listing reports no ETag.
struct Port {
    store: Arc<dyn ObjectStore>,
    bucket: String,
}

impl Port {
    fn failure(&self, key: &str, e: ObjectError) -> Failure {
        object_failure(&format!("s3://{}/{key}", self.bucket), e)
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

/// The S3 protocol a signed read needs, performing no I/O: presigned request URLs and the
/// listing parser.
pub trait Presign: Send + Sync {
    fn get(&self, key: &str) -> String;
    fn head(&self, key: &str) -> String;
    fn list(&self, prefix: &str, token: Option<&str>) -> String;
    /// One listing page's keys with their ETags, and the token continuing it.
    fn page(&self, body: &str) -> Result<(Vec<(String, String)>, Option<String>), String>;
}

/// Opens the presigner for a source's bucket, with the key pair hydrated for one read.
pub type Presigner = Arc<dyn Fn(&ObjectConfig, Option<(Hydrated, Hydrated)>) -> Result<Box<dyn Presign>, Failure> + Send + Sync>;

/// A bucket on an S3-compatible endpoint, each request presigned and sent through the
/// mediated client, whose allowlist holds the endpoint host alone. It reads; it never writes.
struct SignedBucket {
    presign: Box<dyn Presign>,
    bucket: String,
    client: Client,
}

impl SignedBucket {
    fn open(config: &ObjectConfig, presign: Box<dyn Presign>) -> Result<SignedBucket, Failure> {
        let allow = Allowlist::parse(&[config.endpoint.host_str().unwrap_or_default()]).map_err(|e| Failure::deterministic(FailureTag::Config, e.to_string()))?;
        Ok(SignedBucket { presign, bucket: config.bucket.clone(), client: Client::new(allow, config.endpoint.clone()) })
    }

    fn input(&self, key: &str) -> String {
        format!("s3://{}/{key}", self.bucket)
    }

    fn send(&self, method: &str, url: &str, key: &str) -> Result<contextful_outbound::Response, Failure> {
        let url = Url::parse(url).map_err(|e| Failure::deterministic(FailureTag::Config, format!("`{}`: {e}", self.input(key))))?;
        let resp = self.client.send(method, &url, &[], None)?;
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
            let resp = self.send("GET", &self.presign.list(prefix, token.as_deref()), prefix)?;
            if resp.status == 404 {
                return Err(Failure::deterministic(FailureTag::Permanent, format!("`{}`: no bucket answers", self.input(prefix))));
            }
            let (page, next) = self.presign.page(&String::from_utf8_lossy(&resp.body)).map_err(|e| Failure::new(FailureTag::Transient, format!("`{}`: {e}", self.input(prefix))))?;
            out.extend(page.into_iter().map(|(k, t)| (k, Some(t))));
            match next {
                Some(next) if token.as_ref() != Some(&next) => token = Some(next),
                _ => return Ok(out),
            }
        }
        Err(Failure::new(FailureTag::Permanent, format!("the listing of `{}` reached {} pages", self.input(prefix), crate::http::PAGE_CAP)))
    }

    fn etag(&self, key: &str) -> Result<Option<String>, Failure> {
        let resp = self.send("HEAD", &self.presign.head(key), key)?;
        if resp.status == 404 {
            return Ok(None);
        }
        self.tagged(&resp, key).map(Some)
    }

    fn get(&self, key: &str) -> Result<Option<(Vec<u8>, String)>, Failure> {
        let resp = self.send("GET", &self.presign.get(key), key)?;
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

    /// A source reading its configured endpoint through the mediated client, each request
    /// presigned by `presigner` with the credential references hydrated per read
    /// (`connector.source.object-credentials`).
    pub fn signed(config: ObjectConfig, resolver: Arc<Resolver>, presigner: Presigner) -> ObjectSource {
        let c = config.clone();
        let open: Open = Box::new(move || {
            let credentials = match (&c.access_key_id, &c.secret_access_key) {
                (Some(id), Some(secret)) => Some((resolver.render(id)?, resolver.render(secret)?)),
                _ => None,
            };
            Ok(Box::new(SignedBucket::open(&c, presigner(&c, credentials)?)?) as Box<dyn Objects>)
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
        self.read_with_completion(position, cancel).map(|(rows, cursor, _)| (rows, cursor))
    }

    /// The source examined its complete inventory only when no object was skipped.
    fn read_with_completion(&self, position: Option<&Value>, cancel: &dyn Cancellation) -> Result<(Vec<Row>, Option<Value>, bool), Failure> {
        let skip = self.config.skip_unchanged;
        let held: BTreeMap<String, String> =
            position.filter(|_| skip).and_then(|p| p.get("objects")).and_then(|o| serde_json::from_value(o.clone()).ok()).unwrap_or_default();
        let objects = (self.open)()?;
        let mut etags = BTreeMap::new();
        let mut rows = Vec::new();
        let mut skipped_input = false;
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
                    skipped_input = true;
                    continue;
                }
            }
            let (bytes, etag) = objects.get(&key)?.ok_or_else(|| Failure::deterministic(FailureTag::Permanent, format!("`{input}`: no object holds the key")))?;
            let unchanged = held.get(&key) == Some(&etag);
            etags.insert(key.clone(), etag);
            if skip && unchanged {
                skipped_input = true;
                continue;
            }
            let body = if self.config.gzipped(&key) { gunzip(&bytes, EXPANSION_CEILING, &input)? } else { bytes };
            let decoded = match self.config.format {
                Format::Workbook => workbook::rows(&body, None, 0, &input)?,
                format => decode_with_encoding(format, &body, self.config.records.as_deref(), &input, self.config.encoding.as_deref())?.0,
            };
            rows.extend(decoded);
        }
        Ok((rows, skip.then(|| serde_json::json!({ "objects": etags })), !skipped_input))
    }
}

impl Source for ObjectSource {
    /// One pull reads every addressed object; the position rides the pull as its cursor.
    fn pull(&mut self, request: &PullRequest, cancel: &dyn Cancellation) -> Result<Vec<u8>, Failure> {
        let (rows, cursor, snapshot_complete) = self.read_with_completion(request.position.as_ref(), cancel)?;
        serde_json::to_vec(&serde_json::json!({ "rows": rows, "cursor": cursor, "more": false, "snapshot_complete": snapshot_complete }))
            .map_err(|e| Failure::new(FailureTag::Permanent, e.to_string()))
    }
}
