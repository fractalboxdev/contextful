//! `connector.resolve`: the provider port every credential backend sits behind.

use super::reference::{check_material, is_credential_key, is_reference, Hydrated, SecretName};
use super::ConnectorError;
use crate::run::Failure;
use crate::time::Instant;

/// The profile of a build selecting no profile bundle (`topology.package.version-profile`).
pub const DEVELOPMENT_PROFILE: &str = "development";

/// Refuse a plaintext credential written inline in the manifest `text` read from `file`,
/// unless `profile` is [`DEVELOPMENT_PROFILE`] (`connector.resolve.inline-outside-development`):
/// a non-empty string under a credential key holding no reference raises
/// `SecretInlineCredential`, naming the file and key path, never the value. A credential-shaped
/// value refuses first as material (`connector.reference.material-in-a-declaration`), a value
/// holding a `${…}` placeholder is left to the template parser, and a manifest that does not
/// parse to the command reading it.
pub fn check_inline(profile: &str, file: &str, text: &str) -> Result<(), ConnectorError> {
    if profile == DEVELOPMENT_PROFILE {
        return Ok(());
    }
    let value: serde_json::Value = if file.ends_with(".json") {
        match serde_json::from_str(text) {
            Ok(v) => v,
            Err(_) => return Ok(()),
        }
    } else {
        match toml::from_str::<toml::Value>(text).ok().and_then(|v| serde_json::to_value(v).ok()) {
            Some(v) => v,
            None => return Ok(()),
        }
    };
    let Some((path, found)) = inline(&value, &mut Vec::new()) else { return Ok(()) };
    if let Err(ConnectorError::SecretMaterialInDeclaration(_)) = check_material(&path, found) {
        return Err(ConnectorError::SecretMaterialInDeclaration(format!("{file}: `{path}` holds credential material; bind it as `${{secret://<name>}}`")));
    }
    Err(ConnectorError::SecretInlineCredential(format!(
        "{file}: `{path}` holds a plaintext credential, which the `{profile}` build refuses; bind it as `secret://<name>`, `${{secret://<name>}}` or `env://NAME`"
    )))
}

/// The key path and value of the first inline credential under `value`.
fn inline<'a>(value: &'a serde_json::Value, path: &mut Vec<String>) -> Option<(String, &'a str)> {
    match value {
        serde_json::Value::Object(map) => map.iter().find_map(|(k, v)| {
            path.push(k.clone());
            let found = inline(v, path);
            path.pop();
            found
        }),
        serde_json::Value::Array(items) => items.iter().enumerate().find_map(|(i, v)| {
            path.push(i.to_string());
            let found = inline(v, path);
            path.pop();
            found
        }),
        serde_json::Value::String(s) => {
            let key = path.iter().rev().find(|k| k.parse::<usize>().is_err())?;
            let v = s.trim();
            (is_credential_key(key) && !v.is_empty() && !v.contains("${") && !is_reference(v)).then(|| (path.join("."), v))
        }
        _ => None,
    }
}

/// What a provider answers for a name: the material, an expiry for a lease, and a version
/// for a versioned store.
#[derive(Debug, PartialEq, Eq)]
pub struct Answer {
    pub value: Hydrated,
    pub expires_at: Option<Instant>,
    /// The store's version of the value, for an adapter that versions it; a rotation's put
    /// is conditioned on it (`connector.rotate.oauth-credential`).
    pub version: Option<String>,
}

/// One adapter answering a reference with material (`connector.resolve.provider-port`).
pub trait Provider: Send + Sync {
    /// The adapter's name, as the run's provider attribution records it.
    fn name(&self) -> &str;
    /// The material for `name`, `None` when this adapter holds none.
    fn answer(&self, name: &SecretName) -> Result<Option<Answer>, Failure>;
    /// Whether this adapter takes part in template hydration.
    fn serves_templates(&self) -> bool {
        true
    }
}
