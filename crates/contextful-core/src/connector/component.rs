//! A pipeline source naming a component artifact (`connector.package.component-source`):
//! the artifact reference and its pin, the grant a session runs under, the forwarded
//! guest table and the linear-memory override, all checked before any I/O.

use super::attach::Allowlist;
use super::package::{guest_config, Artifact, Form, PinRequirement};
use super::reference::{check_material, Template};
use super::ConnectorError;
use crate::run::RunError;
use serde_json::{Map, Value};

/// The configuration keys a component source reads (`run.declare.config-key`).
pub const KEYS: [&str; 6] = ["sha256", "allow", "attach", "guest", "memory_bytes", "require_pin"];

/// Why a component declaration refuses: the pipeline's rules, or the connector's.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SourceError {
    #[error(transparent)]
    Run(RunError),
    #[error(transparent)]
    Connector(ConnectorError),
}

impl From<RunError> for SourceError {
    fn from(e: RunError) -> SourceError {
        SourceError::Run(e)
    }
}

impl From<ConnectorError> for SourceError {
    fn from(e: ConnectorError) -> SourceError {
        SourceError::Connector(e)
    }
}

/// A component source as declared.
#[derive(Debug, Clone, PartialEq)]
pub struct ComponentSource {
    pub artifact: Artifact,
    /// The hosts the guest's requests reach; empty when `allow` is absent, reaching none.
    pub allow: Allowlist,
    /// Headers the host attaches to every permitted request, each a template holding a reference.
    pub attach: Vec<(String, Template)>,
    /// The forwarded guest table (`connector.import.forwarded-config`).
    pub guest: Option<Value>,
    /// The per-connector linear-memory override, held to its ceiling where the host loads.
    pub memory_bytes: Option<u64>,
    /// The manifest flag of `connector.package.pin-requirement`.
    pub require_pin: bool,
}

fn invalid(k: &str, shape: &str, found: &Value) -> SourceError {
    RunError::Invalid(format!("component source key `{k}` is {shape}, found {found}")).into()
}

impl ComponentSource {
    /// Read the source `name` and its `config`. An in-tree name answers `None`: it names a
    /// compiled-in source or none.
    pub fn parse(name: &str, config: &Value) -> Result<Option<ComponentSource>, SourceError> {
        let pin = match config.get("sha256") {
            None => None,
            Some(Value::String(s)) => Some(s.as_str()),
            Some(other) => return Err(invalid("sha256", "a 64-hex string", other)),
        };
        let artifact = Artifact::parse(name, pin)?;
        if matches!(artifact.form, Form::InTree(_)) {
            return Ok(None);
        }
        let empty = Map::new();
        let cfg = match config {
            Value::Object(m) => m,
            Value::Null => &empty,
            other => return Err(invalid("config", "a table", other)),
        };
        if let Some(k) = cfg.keys().find(|k| !KEYS.contains(&k.as_str())) {
            return Err(RunError::PipelineUnknownConfigKey(format!("a component source reads no key `{k}`; it reads {}", KEYS.join(", "))).into());
        }
        let allow = match cfg.get("allow") {
            None => Allowlist(Vec::new()),
            Some(Value::Array(items)) => {
                let hosts = items.iter().map(|v| v.as_str().ok_or_else(|| invalid("allow", "a list of hosts", v))).collect::<Result<Vec<&str>, _>>()?;
                Allowlist::parse(&hosts)?
            }
            Some(other) => return Err(invalid("allow", "a list of hosts", other)),
        };
        let mut attach = Vec::new();
        match cfg.get("attach") {
            None => {}
            Some(Value::Object(headers)) => {
                for (header, v) in headers {
                    let v = v.as_str().ok_or_else(|| invalid(&format!("attach.{header}"), "a value template", v))?;
                    let t = check_material(&format!("attach.{header}"), v)?;
                    if !t.has_reference() {
                        return Err(ConnectorError::SecretLiteralAttachValue(format!(
                            "`attach.{header}` embeds no `${{secret://<name>}}` reference; a constant header belongs in the guest's request"
                        ))
                        .into());
                    }
                    attach.push((header.clone(), t));
                }
            }
            Some(other) => return Err(invalid("attach", "a table of header name to value template", other)),
        }
        if !attach.is_empty() {
            allow.check_bound()?;
        }
        let guest = cfg.get("guest").cloned();
        if let Some(g) = &guest {
            guest_config(g)?;
        }
        let memory_bytes = match cfg.get("memory_bytes") {
            None => None,
            Some(v) => Some(v.as_u64().ok_or_else(|| invalid("memory_bytes", "a non-negative integer", v))?),
        };
        let require_pin = match cfg.get("require_pin") {
            None => false,
            Some(Value::Bool(b)) => *b,
            Some(other) => return Err(invalid("require_pin", "a boolean", other)),
        };
        Ok(Some(ComponentSource { artifact, allow, attach, guest, memory_bytes, require_pin }))
    }

    /// The pin requirement on this artifact under the store-wide policy key `store`, composed
    /// with the manifest flag by disjunction.
    pub fn requirement(&self, store: bool) -> PinRequirement {
        PinRequirement { store, manifest: self.require_pin }
    }
}
