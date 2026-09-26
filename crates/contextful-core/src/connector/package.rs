//! `connector.package` and the forwarded guest table of `connector.import`: the forms an
//! artifact reference takes, its content pin, the pin requirement on a local artifact, and
//! the shape and hashing of the configuration a guest receives.

use super::ConnectorError;
use serde_json::Value;
use sha2::{Digest as _, Sha256};

/// Largest serialized guest configuration table: 64 KiB (`connector.import.config-shape`).
pub const GUEST_CONFIG_BYTES: usize = 64 * 1024;

/// A SHA-256 content pin, held as 64 lowercase hex characters.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Digest(String);

impl Digest {
    /// A pin as written: exactly 64 hex characters, either case.
    pub fn parse(raw: &str) -> Option<Digest> {
        let t = raw.trim();
        (t.len() == 64 && t.bytes().all(|b| b.is_ascii_hexdigit())).then(|| Digest(t.to_ascii_lowercase()))
    }

    /// The digest of `bytes`.
    pub fn of(bytes: &[u8]) -> Digest {
        Digest(hex(&Sha256::digest(bytes)))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for Digest {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// Where a connector's bytes come from (`connector.package.distribution-form`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Form {
    /// A compiled-in source, by its registered name.
    InTree(String),
    /// A project-local artifact path.
    Local(String),
    /// An HTTPS URL.
    Https(String),
    /// An OCI reference, `oci://<registry>/<repository>[:<tag>|@<digest>]`.
    Oci(String),
}

impl Form {
    pub fn is_remote(&self) -> bool {
        matches!(self, Form::Https(_) | Form::Oci(_))
    }
}

/// A parsed artifact reference and the pin written beside it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Artifact {
    pub form: Form,
    pub pin: Option<Digest>,
}

/// The two switches requiring a pin on a local artifact, composed by disjunction
/// (`connector.package.pin-requirement`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct PinRequirement {
    /// The store-wide policy key.
    pub store: bool,
    /// The connector manifest's own flag.
    pub manifest: bool,
}

impl PinRequirement {
    pub fn required(self) -> bool {
        self.store || self.manifest
    }
}

impl Artifact {
    /// Parse a reference and its pin. A plain-HTTP reference raises
    /// `ConnectorInsecureArtifact`; a remote one without a 64-hex pin raises
    /// `ConnectorRemoteUnpinned`.
    pub fn parse(reference: &str, pin: Option<&str>) -> Result<Artifact, ConnectorError> {
        let r = reference.trim();
        let lower = r.to_ascii_lowercase();
        if lower.starts_with("http://") {
            return Err(ConnectorError::ConnectorInsecureArtifact(format!("`{r}` is plain HTTP; an artifact travels over HTTPS or OCI")));
        }
        let form = if lower.starts_with("https://") {
            Form::Https(r.to_string())
        } else if lower.starts_with("oci://") {
            Form::Oci(r.to_string())
        } else if lower.contains("://") {
            return Err(ConnectorError::ConnectorInsecureArtifact(format!("`{r}` names a scheme other than HTTPS or OCI")));
        } else if !r.is_empty() && r.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-') {
            Form::InTree(r.to_string())
        } else {
            Form::Local(r.to_string())
        };
        let parsed = pin.map(|p| Digest::parse(p).ok_or(p));
        match (&form, parsed) {
            (f, None) if f.is_remote() => {
                Err(ConnectorError::ConnectorRemoteUnpinned(format!("remote artifact `{r}` carries no content pin")))
            }
            (f, Some(Err(p))) if f.is_remote() => {
                Err(ConnectorError::ConnectorRemoteUnpinned(format!("remote artifact `{r}` carries pin `{p}`, which is not 64 hex characters")))
            }
            (_, Some(Err(p))) => {
                Err(ConnectorError::ConnectorDigestMismatch(format!("artifact `{r}` carries pin `{p}`, which is not 64 hex characters and matches no bytes")))
            }
            (_, Some(Ok(d))) => Ok(Artifact { form, pin: Some(d) }),
            (_, None) => Ok(Artifact { form, pin: None }),
        }
    }

    /// Admit the resolved `bytes` before they reach the engine, answering their digest.
    /// A pin the bytes do not hash to raises `ConnectorDigestMismatch`; an unpinned local
    /// artifact under a set requirement raises `ConnectorLocalUnpinned` carrying the
    /// digest of the bytes found.
    pub fn admit(&self, bytes: &[u8], requirement: PinRequirement) -> Result<Digest, ConnectorError> {
        let found = Digest::of(bytes);
        match &self.pin {
            Some(pin) if *pin != found => Err(ConnectorError::ConnectorDigestMismatch(format!("the artifact is pinned to {pin} and its bytes hash to {found}"))),
            Some(_) => Ok(found),
            None if self.form.is_remote() => Err(ConnectorError::ConnectorRemoteUnpinned("a remote artifact carries no content pin".into())),
            None if matches!(self.form, Form::Local(_)) && requirement.required() => Err(ConnectorError::ConnectorLocalUnpinned(format!(
                "a pin is required on local artifacts and none is written; the bytes found hash to {found}"
            ))),
            None => Ok(found),
        }
    }
}

/// Check a guest configuration value ahead of any I/O and answer its serialization: a
/// value that is not a table, a serialization over [`GUEST_CONFIG_BYTES`], or a
/// credential or environment reference anywhere inside raises `ConnectorConfigRejected`.
pub fn guest_config(value: &Value) -> Result<String, ConnectorError> {
    let reject = |why: String| Err(ConnectorError::ConnectorConfigRejected(why));
    if !value.is_object() {
        return reject(format!("the guest configuration is a table, found {}", kind(value)));
    }
    if let Some(path) = reference_in(value, String::new()) {
        return reject(format!("the guest configuration carries a credential or environment reference at `{path}`; a credential attaches through [attach]"));
    }
    let text = serde_json::to_string(value).map_err(|e| ConnectorError::ConnectorConfigRejected(e.to_string()))?;
    if text.len() > GUEST_CONFIG_BYTES {
        return reject(format!("the guest configuration serializes to {} bytes, over {GUEST_CONFIG_BYTES}", text.len()));
    }
    Ok(text)
}

fn kind(v: &Value) -> &'static str {
    match v {
        Value::Null => "null",
        Value::Bool(_) => "a boolean",
        Value::Number(_) => "a number",
        Value::String(_) => "a string",
        Value::Array(_) => "an array",
        Value::Object(_) => "a table",
    }
}

fn is_reference(s: &str) -> bool {
    let l = s.to_ascii_lowercase();
    l.contains("secret://") || l.contains("env://") || l.contains("${")
}

/// The path of the first key or string value holding a reference.
fn reference_in(v: &Value, path: String) -> Option<String> {
    match v {
        Value::String(s) if is_reference(s) => Some(if path.is_empty() { "<root>".into() } else { path }),
        Value::Array(items) => items.iter().enumerate().find_map(|(i, x)| reference_in(x, format!("{path}[{i}]"))),
        Value::Object(map) => map.iter().find_map(|(k, x)| {
            let at = if path.is_empty() { k.clone() } else { format!("{path}.{k}") };
            if is_reference(k) {
                Some(at)
            } else {
                reference_in(x, at)
            }
        }),
        _ => None,
    }
}

/// The connector's content hash: the artifact digest verbatim with no table forwarded,
/// and otherwise the digest over the artifact digest and the table's serialization
/// (`connector.import.config-hashing`). The table hashes in canonical form, so key order
/// in the declaration moves no hash.
pub fn content_hash(artifact: &Digest, config: Option<&Value>) -> String {
    match config {
        None => artifact.as_str().to_string(),
        Some(table) => {
            let mut h = Sha256::new();
            h.update(artifact.as_str().as_bytes());
            h.update([0u8]);
            h.update(canonical(table).as_bytes());
            hex(&h.finalize())
        }
    }
}

/// JSON text with every table's keys in byte order, whatever order the map holds them.
fn canonical(v: &Value) -> String {
    match v {
        Value::Object(map) => {
            let mut keys: Vec<&String> = map.keys().collect();
            keys.sort();
            let body: Vec<String> =
                keys.into_iter().map(|k| format!("{}:{}", Value::String(k.clone()), canonical(&map[k]))).collect();
            format!("{{{}}}", body.join(","))
        }
        Value::Array(items) => format!("[{}]", items.iter().map(canonical).collect::<Vec<_>>().join(",")),
        other => other.to_string(),
    }
}
