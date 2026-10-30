//! The connector manifest (`connector.package.manifest-keys`): a closed key set, the host
//! access it declares (`connector.declare-capability.declared-grant`), and its one build
//! input (`connector.package.build-source`).

use super::attach::Allowlist;
use super::probe::ScopeProbe;
use super::package::Digest;
use super::rotate::AuthPolicy;
use super::ConnectorError;
use serde::Deserialize;
use std::path::{Component, Path, PathBuf};

/// The manifest as written; every table refuses a key it does not read.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Raw {
    name: Option<String>,
    version: Option<String>,
    world: Option<String>,
    wasm: Option<String>,
    wasm_sha256: Option<String>,
    #[serde(default)]
    require_wasm_pin: bool,
    build: Option<RawBuild>,
    #[serde(default)]
    capabilities: RawCapabilities,
    dedup_window_secs: Option<u64>,
    auth: Option<RawAuth>,
}

/// `[auth]`: the OAuth refresh policy the host runs for the connector
/// (`connector.rotate.auth-policy`). Every key is required.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawAuth {
    /// The token endpoint each refresh posts to, TLS or loopback.
    token_endpoint: String,
    /// The OAuth client identifier.
    client_id: String,
    /// The client secret, one `${secret://<name>}` reference.
    client_secret: String,
    /// The refresh token, one `${secret://<name>}` reference whose adapter stores each replacement.
    refresh_token: String,
    /// The access token, one `${secret://<name>}` reference attach templates name and the host alone answers.
    access_token: String,
    /// The scope each refresh requests.
    scope: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawBuild {
    source: String,
}

/// `env` and `limiter` are absent: a load binds neither, so declaring one refuses rather
/// than reading as granted (P1).
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawCapabilities {
    #[serde(default)]
    allow_hosts: Vec<String>,
    #[serde(default)]
    clock: bool,
    scope_probe: Option<RawProbe>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawProbe {
    endpoint: String,
    scopes_header: String,
    #[serde(default)]
    expect: Vec<String>,
}

/// The host access a manifest declares, decided at load.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Declared {
    /// `[capabilities] allow_hosts`; empty when the connector reaches no host.
    pub hosts: Allowlist,
    /// `[capabilities] clock`: the wall-clock grant.
    pub clock: bool,
    pub scope_probe: Option<ScopeProbe>,
}

/// The one build input: a directory relative to the manifest holding the guest crate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Build {
    pub source: PathBuf,
}

/// A parsed connector manifest.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Manifest {
    pub name: Option<String>,
    pub version: Option<String>,
    pub world: Option<String>,
    /// The artifact path, relative to the manifest, `connector pin` writes.
    pub wasm: Option<String>,
    /// The artifact's content pin as written by `connector pin`; [`Manifest::pin`] reads it at load.
    pub wasm_sha256: Option<String>,
    /// The per-connector switch of `connector.package.pin-requirement`.
    pub require_wasm_pin: bool,
    pub build: Option<Build>,
    pub declared: Declared,
    /// The seconds the connector's destination deduplicates a repeated idempotency key
    /// (`connector.declare-capability.dedup-window`).
    pub dedup_window_secs: Option<u64>,
    /// The `[auth]` table's OAuth refresh policy (`connector.rotate.auth-policy`).
    pub auth: Option<AuthPolicy>,
}

fn rejected(key: impl std::fmt::Display, why: impl std::fmt::Display) -> ConnectorError {
    ConnectorError::ConnectorManifestRejected(format!("`{key}` {why}"))
}

/// The dotted key a deserialization error names: the field it rejects, below the path it
/// reached.
fn failing_key(e: &serde_path_to_error::Error<toml::de::Error>) -> String {
    let path = e.path().to_string();
    let message = e.inner().message();
    let unknown = message.strip_prefix("unknown field `").and_then(|rest| rest.split_once('`')).map(|(field, _)| field);
    let missing = message.strip_prefix("missing field `").and_then(|rest| rest.split_once('`')).map(|(field, _)| field);
    match unknown.or(missing) {
        Some(field) if path == "." || path.is_empty() => field.to_string(),
        Some(field) if !path.ends_with(field) => format!("{path}.{field}"),
        _ => path,
    }
}

impl Manifest {
    /// Parse a manifest. A key no table reads, a value off its shape, and a `build.source`
    /// leaving the manifest directory raise `ConnectorManifestRejected` naming the key.
    pub fn parse(text: &str) -> Result<Manifest, ConnectorError> {
        let de = toml::Deserializer::new(text);
        let raw: Raw = serde_path_to_error::deserialize(de).map_err(|e| {
            let key = failing_key(&e);
            rejected(key, e.inner().message().trim())
        })?;
        let build = raw.build.map(|b| build_source(&b.source)).transpose()?;
        let caps = raw.capabilities;
        let hosts = if caps.allow_hosts.is_empty() { Allowlist(Vec::new()) } else { Allowlist::parse(&caps.allow_hosts)? };
        let scope_probe = caps.scope_probe.map(|p| ScopeProbe::new(&p.endpoint, &p.scopes_header, &p.expect)).transpose()?;
        let auth = raw.auth.map(|a| AuthPolicy::new(&a.token_endpoint, &a.client_id, &a.client_secret, &a.refresh_token, &a.access_token, &a.scope)).transpose()?;
        Ok(Manifest {
            name: raw.name,
            version: raw.version,
            world: raw.world,
            wasm: raw.wasm,
            wasm_sha256: raw.wasm_sha256,
            require_wasm_pin: raw.require_wasm_pin,
            build,
            declared: Declared { hosts, clock: caps.clock, scope_probe },
            dedup_window_secs: raw.dedup_window_secs,
            auth,
        })
    }

    /// Parse the outcome of reading the manifest at `path`; the adapter performs the read,
    /// and an unreadable file refuses as a malformed one.
    pub fn from_read(path: &Path, read: std::io::Result<String>) -> Result<Manifest, ConnectorError> {
        let text = read.map_err(|e| rejected("manifest", format_args!("at `{}` is unreadable: {e}", path.display())))?;
        Manifest::parse(&text)
    }

    /// The artifact pin a load admits against: `wasm_sha256`, read at load so a template's
    /// placeholder parses cleanly and refuses here.
    pub fn pin(&self) -> Result<Option<Digest>, ConnectorError> {
        match &self.wasm_sha256 {
            None => Ok(None),
            Some(hex) => Digest::parse(hex).map(Some).ok_or_else(|| rejected("wasm_sha256", format_args!("`{hex}` is not 64 hex characters"))),
        }
    }

    /// The build source resolved against the directory holding `manifest_path`.
    pub fn build_dir(&self, manifest_path: &Path) -> Option<PathBuf> {
        let build = self.build.as_ref()?;
        Some(manifest_path.parent().unwrap_or_else(|| Path::new("")).join(&build.source))
    }
}

/// A build source is a nonempty path of plain components below the manifest directory.
fn build_source(raw: &str) -> Result<Build, ConnectorError> {
    let path = Path::new(raw);
    if raw.is_empty() || path.components().any(|part| !matches!(part, Component::Normal(_))) {
        return Err(rejected("build.source", format_args!("`{raw}` leaves the manifest directory")));
    }
    Ok(Build { source: path.to_path_buf() })
}
