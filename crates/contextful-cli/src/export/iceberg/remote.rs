//! The catalog and object-store requests of one Iceberg export. Each leaves through a
//! mediated client whose allowlist holds the declared catalog or storage host alone
//! (`run.export-iceberg.egress`); object requests are SigV4-presigned and sent as plain URLs.

use anyhow::{bail, Context, Result};
use contextful_core::connector::attach::Allowlist;
use contextful_core::connector::reference::Hydrated;
use contextful_core::export::iceberg::IcebergTarget;
use contextful_core::export::ExportError;
use contextful_outbound::{Client, HeaderValue, Resolver};
use iceberg::spec::TableMetadata;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;
use url::Url;

/// The wall clock one object transfer may take: a data file holds up to 128 MiB.
const OBJECT_TIMEOUT: Duration = Duration::from_secs(900);
/// The largest object a transfer reads back: a manifest or manifest list.
const OBJECT_BODY_BYTES: u64 = 256 * 1024 * 1024;
/// How long a presigned URL stays valid.
const PRESIGN: Duration = Duration::from_secs(3600);

/// A table as the catalog answered it, with the storage configuration it vends.
pub(super) struct Loaded {
    pub metadata: TableMetadata,
    pub config: HashMap<String, String>,
}

/// What one commit request came to.
pub(super) enum Commit {
    Taken,
    /// `409`: another snapshot reached `main` first.
    Conflict(String),
    /// A definite refusal: nothing was applied.
    Refused(String),
    /// A `5xx` or no answer: the catalog may have applied it (`run.export-iceberg.commit-unknown`).
    Unknown(String),
}

fn client(url: &Url) -> Result<Client> {
    let host = url.host_str().unwrap_or_default();
    Ok(Client::new(Allowlist::parse(&[host])?, url.clone()))
}

/// The REST catalog of one export.
pub(super) struct Catalog {
    client: Client,
    /// `<catalog>/v1/<prefix>/`.
    base: Url,
    bearer: Arc<Hydrated>,
    namespace: String,
    table: String,
}

impl Catalog {
    /// Open the catalog: hydrate the token and read `/v1/config` for the warehouse prefix.
    pub fn open(target: &IcebergTarget, resolver: &Resolver) -> Result<Catalog> {
        let token = resolver.render(&target.token).map_err(|f| anyhow::anyhow!(f.message))?;
        let bearer = Arc::new(Hydrated::new(format!("Bearer {}", token.reveal())));
        let root = target.catalog.as_str().trim_end_matches('/').to_string();
        let mut config = Url::parse(&format!("{root}/v1/config"))?;
        config.query_pairs_mut().append_pair("warehouse", &target.warehouse);
        let client = client(&target.catalog)?;
        let mut catalog = Catalog { client, base: Url::parse(&format!("{root}/v1/"))?, bearer, namespace: target.namespace.clone(), table: target.table_name.clone() };
        let answer = catalog.request("GET", &config, None)?;
        if answer.0 != 200 {
            bail!("the catalog answered {} to its configuration request: {}", answer.0, answer.1);
        }
        let overrides = &answer.1["overrides"]["prefix"];
        if let Some(prefix) = overrides.as_str().or(answer.1["defaults"]["prefix"].as_str()).filter(|p| !p.is_empty()) {
            catalog.base = catalog.base.join(&format!("{prefix}/"))?;
        }
        Ok(catalog)
    }

    fn request(&self, method: &str, url: &Url, body: Option<&Value>) -> Result<(u16, Value)> {
        let response = self.exchange(method, url, body).map_err(|f| anyhow::anyhow!("the catalog request `{method} {}` failed: {f}", url.path()))?;
        Ok((response.0, response.1))
    }

    fn exchange(&self, method: &str, url: &Url, body: Option<&Value>) -> Result<(u16, Value), String> {
        // A catalog vends table credentials only to a client asking for them.
        let mut headers = vec![
            ("Authorization".to_string(), HeaderValue::Sensitive(self.bearer.clone())),
            ("X-Iceberg-Access-Delegation".to_string(), HeaderValue::Plain("vended-credentials".to_string())),
        ];
        let bytes = body.map(|b| b.to_string().into_bytes());
        if bytes.is_some() {
            headers.push(("Content-Type".to_string(), HeaderValue::Plain("application/json".to_string())));
        }
        let response = self.client.send(method, url, &headers, bytes.as_deref()).map_err(|f| f.message)?;
        let value = serde_json::from_slice(&response.body).unwrap_or(Value::Null);
        Ok((response.status, value))
    }

    fn table_url(&self) -> Result<Url> {
        Ok(self.base.join(&format!("namespaces/{}/tables/{}", self.namespace, self.table))?)
    }

    /// The table's current metadata, or `None` when the catalog holds no such table.
    pub fn load(&self) -> Result<Option<Loaded>> {
        match self.request("GET", &self.table_url()?, None)? {
            (200, body) => Ok(Some(loaded(body)?)),
            (404, _) => Ok(None),
            (status, body) => bail!("the catalog answered {status} loading `{}.{}`: {body}", self.namespace, self.table),
        }
    }

    /// Create the namespace when absent, then the table at `schema` and `properties`.
    pub fn create(&self, schema: &iceberg::spec::Schema, properties: &HashMap<String, String>) -> Result<Loaded> {
        let namespace = self.base.join(&format!("namespaces/{}", self.namespace))?;
        if self.request("GET", &namespace, None)?.0 == 404 {
            let created = self.request("POST", &self.base.join("namespaces")?, Some(&json!({ "namespace": [self.namespace], "properties": {} })))?;
            if !matches!(created.0, 200 | 409) {
                bail!("the catalog answered {} creating namespace `{}`: {}", created.0, self.namespace, created.1);
            }
        }
        let body = json!({ "name": self.table, "schema": schema, "properties": properties, "stage-create": false });
        match self.request("POST", &self.base.join(&format!("namespaces/{}/tables", self.namespace))?, Some(&body))? {
            (200, body) => loaded(body),
            (status, body) => bail!("the catalog answered {status} creating `{}.{}`: {body}", self.namespace, self.table),
        }
    }

    /// Drop a table this export created and published nothing into.
    pub fn drop_table(&self) -> Result<()> {
        match self.request("DELETE", &self.table_url()?, None)?.0 {
            200..=299 | 404 => Ok(()),
            status => bail!("the catalog answered {status} dropping `{}.{}`", self.namespace, self.table),
        }
    }

    /// Send one `updateTable` request.
    pub fn commit(&self, body: &Value) -> Result<Commit> {
        // The REST specification's definite refusals take no effect; every other status, a
        // proxy's 408 or 429 among them, leaves the outcome open.
        Ok(match self.exchange("POST", &self.table_url()?, Some(body)) {
            Ok((200..=299, _)) => Commit::Taken,
            Ok((409, body)) => Commit::Conflict(body.to_string()),
            Ok((status @ (400 | 401 | 403 | 404 | 406 | 419 | 422), body)) => Commit::Refused(format!("the catalog answered {status}: {body}")),
            Ok((status, body)) => Commit::Unknown(format!("the catalog answered {status}: {body}")),
            Err(message) => Commit::Unknown(message),
        })
    }
}

fn strings(value: &Value) -> HashMap<String, String> {
    value.as_object().map(|c| c.iter().filter_map(|(k, v)| v.as_str().map(|v| (k.clone(), v.to_string()))).collect()).unwrap_or_default()
}

/// The table's metadata and its storage configuration: the `config` map, overlaid by the
/// `storage-credentials` entry whose prefix is the longest one the table location carries.
fn loaded(body: Value) -> Result<Loaded> {
    let metadata: TableMetadata = serde_json::from_value(body["metadata"].clone()).context("reading the catalog's table metadata")?;
    let mut config = strings(&body["config"]);
    let vended = body["storage-credentials"].as_array().into_iter().flatten().filter(|c| c["prefix"].as_str().is_some_and(|p| metadata.location().starts_with(p)));
    if let Some(best) = vended.max_by_key(|c| c["prefix"].as_str().map_or(0, str::len)) {
        config.extend(strings(&best["config"]));
    }
    Ok(Loaded { metadata, config })
}

/// The object store the table's files live in, at the declared storage host.
pub(super) struct Storage {
    client: Client,
    endpoint: Url,
    region: String,
    credentials: rusty_s3::Credentials,
}

impl Storage {
    /// The storage a table configuration names, refused as `ExportStorageUndeclared` when its
    /// endpoint's host is not the declared one (`run.export-iceberg.egress`).
    pub fn open(export: &str, target: &IcebergTarget, config: &HashMap<String, String>) -> Result<Storage> {
        let endpoint = match config.get("s3.endpoint") {
            Some(named) => {
                let url = Url::parse(named).with_context(|| format!("the table configuration's `s3.endpoint` `{named}`"))?;
                if url.host_str() != target.storage.host_str() || url.port_or_known_default() != target.storage.port_or_known_default() {
                    return Err(ExportError::ExportStorageUndeclared(format!(
                        "export `{export}`: the table configuration names storage `{}`, and the block declares `{}`; no file was written",
                        url.host_str().unwrap_or_default(),
                        target.storage.host_str().unwrap_or_default()
                    ))
                    .into());
                }
                url
            }
            None => target.storage.clone(),
        };
        let (Some(key), Some(secret)) = (config.get("s3.access-key-id"), config.get("s3.secret-access-key")) else {
            bail!("export `{export}`: the catalog vends no storage credentials for the table");
        };
        let credentials = match config.get("s3.session-token") {
            Some(token) => rusty_s3::Credentials::new_with_token(key, secret, token),
            None => rusty_s3::Credentials::new(key, secret),
        };
        let region = config.get("s3.region").cloned().unwrap_or_else(|| "auto".to_string());
        Ok(Storage { client: client(&endpoint)?, endpoint, region, credentials })
    }

    /// The bucket and key an `s3://bucket/key` location names.
    fn bucket(&self, location: &str) -> Result<(rusty_s3::Bucket, String)> {
        let url = Url::parse(location).with_context(|| format!("the object location `{location}`"))?;
        if !matches!(url.scheme(), "s3" | "s3a") {
            bail!("the object location `{location}` is not an `s3://` URI");
        }
        let name = url.host_str().unwrap_or_default().to_string();
        let bucket = rusty_s3::Bucket::new(self.endpoint.clone(), rusty_s3::UrlStyle::Path, name, self.region.clone())?;
        Ok((bucket, url.path().trim_start_matches('/').to_string()))
    }

    fn send(&self, method: &str, url: Url, body: Option<&[u8]>) -> Result<contextful_outbound::Response> {
        self.client.send_bounded(method, &url, &[], body, OBJECT_BODY_BYTES, OBJECT_TIMEOUT).map_err(|f| anyhow::anyhow!("the object request `{method}` failed: {f}"))
    }

    pub fn put(&self, location: &str, bytes: &[u8]) -> Result<()> {
        use rusty_s3::S3Action;
        let (bucket, key) = self.bucket(location)?;
        let url = bucket.put_object(Some(&self.credentials), &key).sign(PRESIGN);
        let response = self.send("PUT", url, Some(bytes))?;
        if !(200..300).contains(&response.status) {
            bail!("the object store answered {} writing `{location}`", response.status);
        }
        Ok(())
    }

    pub fn get(&self, location: &str) -> Result<Vec<u8>> {
        use rusty_s3::S3Action;
        let (bucket, key) = self.bucket(location)?;
        let url = bucket.get_object(Some(&self.credentials), &key).sign(PRESIGN);
        let response = self.send("GET", url, None)?;
        if response.status != 200 {
            bail!("the object store answered {} reading `{location}`", response.status);
        }
        Ok(response.body)
    }

    pub fn delete(&self, location: &str) -> Result<()> {
        use rusty_s3::S3Action;
        let (bucket, key) = self.bucket(location)?;
        let url = bucket.delete_object(Some(&self.credentials), &key).sign(PRESIGN);
        let response = self.send("DELETE", url, None)?;
        if !(200..300).contains(&response.status) && response.status != 404 {
            bail!("the object store answered {} deleting `{location}`", response.status);
        }
        Ok(())
    }

    /// Delete every location, reporting the first failure after trying all.
    pub fn delete_all<'a>(&self, locations: impl IntoIterator<Item = &'a String>) -> Result<()> {
        let mut first = None;
        for location in locations {
            if let Err(e) = self.delete(location) {
                first.get_or_insert(e);
            }
        }
        first.map_or(Ok(()), Err)
    }
}
