//! The `s3` object source: one bucket, read by a fixed key or a prefix listing, each object
//! decompressed under a ceiling and decoded through the shared decoders
//! (`connector.source.object-address`, `connector.source.prefix-listing`).
//!
//! An object a decoder cannot read, or one past the expansion ceiling, lands a dead letter
//! naming its key, the reason and its digest, and the rest of the read lands
//! (`connector.source.object-unreadable`).
//!
//! A signed read presigns each request through [`Presign`], which the binary implements over
//! the store's S3 bucket adapter, and sends it through the mediated client, so one S3 client
//! signs every bucket request and the bucket is one more allowlisted vendor host
//! (`connector.source.object-transport`). This package links no S3 client.

use crate::body::{strip_ext, BodyKeys, Compression};
use crate::decode::{decode_delimited, workbook, Format};
use crate::http::ConfigError;
use contextful_core::connector::attach::{endpoint, is_loopback_host, scrub, Allowlist};
use contextful_core::connector::reference::{check_material, Hydrated, Part, SecretName, Template};
use contextful_core::connector::ConnectorError;
use contextful_core::run::ports::{Cancellation, DeadLetter, PullRequest, Row, Source};
use contextful_core::run::{Failure, FailureTag, RunError};
use contextful_core::store::object::{ObjectError, ObjectStore};
use contextful_outbound::client::{classify, Client};
use contextful_outbound::Resolver;
use serde_json::{Map, Value};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;
use url::Url;

/// The source's registered name.
pub const NAME: &str = "s3";
/// The endpoint a source declaring none reads.
pub const DEFAULT_ENDPOINT: &str = "https://s3.amazonaws.com";
/// The signing region a source declaring none signs for.
pub const DEFAULT_REGION: &str = "us-east-1";

/// The configuration keys the object source reads (`run.declare.config-key`).
pub const KEYS: [&str; 18] = [
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
    "member",
    "delimiter",
    "columns",
    "skip_unchanged",
    "replay_dead_letters",
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
    /// Compression, zip member and delimited-text keys.
    pub body: BodyKeys,
    pub skip_unchanged: bool,
    /// Re-read every object the position holds dead-lettered, whatever its ETag
    /// (`connector.source.dead-letter-replay`).
    pub replay_dead_letters: bool,
    /// The access key id and secret access key references, declared together or not at all.
    pub access_key_id: Option<Template>,
    pub secret_access_key: Option<Template>,
}

fn invalid(why: String) -> ConfigError {
    RunError::Invalid(format!("the `{NAME}` source {why}")).into()
}

fn text(cfg: &Map<String, Value>, k: &str) -> Result<Option<String>, ConfigError> {
    crate::body::text(NAME, cfg, k)
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

/// The format a key's extension names, compared without case, a trailing `.gz` or `.zip`
/// set aside (`connector.source.body-format`).
fn format_of(name: &str) -> Option<Format> {
    let name = strip_ext(name, ".gz").or_else(|| strip_ext(name, ".zip")).unwrap_or(name);
    let ext = name.rsplit_once('.')?.1.to_ascii_lowercase();
    match ext.as_str() {
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
        let body = BodyKeys::parse(NAME, cfg, format, Compression::ByKey)?;
        // A named key or suffix that is no zip leaves `member` nothing to select.
        if let (Some(_), Some(n)) = (&body.member, named) {
            if body.compression_of(n) != Compression::Zip {
                return Err(invalid(format!("declares `member`, which selects inside a zip archive, and `{n}` is stored as no zip")));
            }
        }
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
            body,
            skip_unchanged: flag(cfg, "skip_unchanged")?,
            replay_dead_letters: flag(cfg, "replay_dead_letters")?,
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
        ObjectError::Transport(m) | ObjectError::Unavailable(m) => Failure::new(FailureTag::Transient, format!("`{input}`: {m}")),
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
    /// and the position after them. An unreadable object lands a dead letter in place of rows.
    pub fn read(&self, position: Option<&Value>, cancel: &dyn Cancellation) -> Result<(Vec<Row>, Option<Value>), Failure> {
        self.read_whole(position, cancel).map(|read| (read.rows, read.cursor))
    }

    /// The rows one object's bytes decode to.
    fn decode(&self, key: &str, bytes: &[u8], input: &str) -> Result<Vec<Row>, Failure> {
        let body = self.config.body.decompress(key, bytes, input)?;
        Ok(match self.config.format {
            Format::Workbook => workbook::rows(&body, None, 0, input)?,
            format => decode_delimited(format, &body, self.config.records.as_deref(), input, self.config.encoding.as_deref(), &self.config.body.delimited)?.0,
        })
    }

    fn read_whole(&self, position: Option<&Value>, cancel: &dyn Cancellation) -> Result<ObjectRead, Failure> {
        let skip = self.config.skip_unchanged;
        let held: BTreeMap<String, String> =
            position.filter(|_| skip).and_then(|p| p.get("objects")).and_then(|o| serde_json::from_value(o.clone()).ok()).unwrap_or_default();
        let held_dead: BTreeSet<String> =
            position.filter(|_| skip).and_then(|p| p.get("dead_letters")).and_then(|o| serde_json::from_value(o.clone()).ok()).unwrap_or_default();
        let objects = (self.open)()?;
        let mut etags = BTreeMap::new();
        let mut dead = BTreeSet::new();
        let mut rows = Vec::new();
        let mut letters = Vec::new();
        let mut refusals = Vec::new();
        // Objects landed or held unchanged; a read whose every object is unreadable refuses
        // (`connector.source.unreadable-all`).
        let mut readable = 0usize;
        let mut skipped_input = false;
        for (key, listed) in self.keys(objects.as_ref())? {
            if cancel.requested() {
                return Err(Failure::canceled("stopped between objects"));
            }
            let input = self.config.input(&key);
            let was_dead = held_dead.contains(&key);
            let replay = self.config.replay_dead_letters && was_dead;
            if let Some(tag) = held.get(&key).filter(|_| skip && !replay) {
                // An unchanged ETag costs a listing entry or a head request, never a download.
                let current = match listed {
                    Some(t) => Some(t),
                    None if matches!(self.config.address, Address::Key(_)) => objects.etag(&key)?,
                    None => None,
                };
                if current.as_ref() == Some(tag) {
                    etags.insert(key.clone(), tag.clone());
                    if was_dead {
                        dead.insert(key);
                    } else {
                        readable += 1;
                    }
                    skipped_input = true;
                    continue;
                }
            }
            let (bytes, etag) = objects.get(&key)?.ok_or_else(|| Failure::deterministic(FailureTag::Permanent, format!("`{input}`: no object holds the key")))?;
            let unchanged = held.get(&key) == Some(&etag);
            etags.insert(key.clone(), etag);
            if skip && unchanged && !replay {
                if was_dead {
                    dead.insert(key);
                } else {
                    readable += 1;
                }
                skipped_input = true;
                continue;
            }
            match self.decode(&key, &bytes, &input) {
                Ok(decoded) => {
                    readable += 1;
                    rows.extend(decoded);
                }
                // A deterministic refusal over the object's own bytes is the object's fault, never
                // the read's: it lands a dead letter and the read continues.
                Err(f) if f.deterministic && f.tag == FailureTag::Permanent => {
                    letters.push(DeadLetter { key: key.clone(), reason: f.message.clone(), sha256: hex(&Sha256::digest(&bytes)) });
                    refusals.push(f);
                    dead.insert(key);
                }
                Err(f) => return Err(f),
            }
        }
        if readable == 0 && !refusals.is_empty() {
            return Err(unreadable_all(refusals));
        }
        let mut cursor = serde_json::json!({ "objects": etags });
        if !dead.is_empty() {
            cursor["dead_letters"] = serde_json::json!(dead);
        }
        let complete = !skipped_input && letters.is_empty();
        Ok(ObjectRead { rows, cursor: skip.then_some(cursor), complete, letters })
    }
}

/// One read of the addressed objects.
struct ObjectRead {
    rows: Vec<Row>,
    cursor: Option<Value>,
    /// Every addressed object landed; an unchanged or dead-lettered object leaves it false.
    complete: bool,
    letters: Vec<DeadLetter>,
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// The refusal of a read whose every object is unreadable: the one object's own refusal, or
/// one naming every object and its reason.
fn unreadable_all(mut refusals: Vec<Failure>) -> Failure {
    if refusals.len() == 1 {
        return refusals.remove(0);
    }
    let reasons: Vec<String> = refusals.iter().map(|f| f.message.clone()).collect();
    Failure::deterministic(FailureTag::Permanent, RunError::PipelineUnreadableInput(format!("every one of the {} objects the read addresses is unreadable: {}", refusals.len(), reasons.join("; "))).to_string())
}

impl Source for ObjectSource {
    /// One pull reads every addressed object; the position rides the pull as its cursor.
    fn pull(&mut self, request: &PullRequest, cancel: &dyn Cancellation) -> Result<Vec<u8>, Failure> {
        let read = self.read_whole(request.position.as_ref(), cancel)?;
        let mut pull = serde_json::json!({ "rows": read.rows, "cursor": read.cursor, "more": false, "snapshot_complete": read.complete });
        // Each dead-lettered object counts on the run record (`run.record.skipped-count`).
        if !read.letters.is_empty() {
            pull["skipped"] = serde_json::json!(read.letters.len());
            pull["dead_letters"] = serde_json::json!(read.letters);
        }
        serde_json::to_vec(&pull)
            .map_err(|e| Failure::new(FailureTag::Permanent, e.to_string()))
    }
}
