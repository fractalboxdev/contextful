//! The connector manifest (`connector.package.manifest-keys`): a closed key set, the host
//! access it declares (`connector.declare-capability.declared-grant`), and its one build
//! input (`connector.package.build-source`).

use super::attach::Allowlist;
use super::meter::LimiterDeclaration;
use super::probe::ScopeProbe;
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
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawBuild {
    source: String,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawCapabilities {
    #[serde(default)]
    allow_hosts: Vec<String>,
    #[serde(default)]
    env: Vec<String>,
    #[serde(default)]
    clock: bool,
    scope_probe: Option<RawProbe>,
    limiter: Option<RawLimiter>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawProbe {
    endpoint: String,
    scopes_header: String,
    #[serde(default)]
    expect: Vec<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawLimiter {
    quota: String,
    class: String,
    #[serde(default)]
    usage_headers: Vec<String>,
}

/// The host access a manifest declares, decided at load.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Declared {
    /// `[capabilities] allow_hosts`; empty when the connector reaches no host.
    pub hosts: Allowlist,
    /// `[capabilities] env`: names an operator binds, never ambient access.
    pub env: Vec<String>,
    /// `[capabilities] clock`: the wall-clock grant.
    pub clock: bool,
    pub scope_probe: Option<ScopeProbe>,
    pub limiter: Option<LimiterDeclaration>,
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
    pub wasm_sha256: Option<String>,
    pub require_wasm_pin: bool,
    pub build: Option<Build>,
    pub declared: Declared,
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
        let limiter = caps.limiter.map(|l| LimiterDeclaration::new(&l.quota, &l.class, &l.usage_headers)).transpose()?;
        Ok(Manifest {
            name: raw.name,
            version: raw.version,
            world: raw.world,
            wasm: raw.wasm,
            wasm_sha256: raw.wasm_sha256,
            require_wasm_pin: raw.require_wasm_pin,
            build,
            declared: Declared { hosts, env: caps.env, clock: caps.clock, scope_probe, limiter },
        })
    }

    /// Read and parse the manifest at `path`; an unreadable file refuses as a malformed one.
    pub fn load(path: &Path) -> Result<Manifest, ConnectorError> {
        let text = std::fs::read_to_string(path).map_err(|e| rejected("manifest", format_args!("at `{}` is unreadable: {e}", path.display())))?;
        Manifest::parse(&text)
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
