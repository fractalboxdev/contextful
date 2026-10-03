//! The control document's refusals (`surface.edit`): it holds references to credentials and
//! to registered connectors, never the credential or the artifact itself.

use super::SurfaceError;
use crate::connector::reference::{check_material, Template};
use crate::connector::ConnectorError;

/// Keys whose value is credential material wherever they sit in the document.
const CREDENTIAL_KEYS: [&str; 12] = [
    "password",
    "passwd",
    "secret",
    "token",
    "api_key",
    "apikey",
    "access_key_id",
    "secret_access_key",
    "session_token",
    "client_secret",
    "refresh_token",
    "private_key",
];

/// Suffixes that make a key a credential key.
const CREDENTIAL_SUFFIXES: [&str; 4] = ["_password", "_secret", "_token", "_api_key"];

/// Keys that carry an artifact's bytes rather than a reference to a registered connector.
const UPLOAD_KEYS: [&str; 4] = ["wasm_bytes", "artifact_bytes", "component_bytes", "upload"];

/// The WebAssembly magic `\0asm`, as it opens a base64 body.
const WASM_BASE64: &str = "AGFzbQ";

/// Hold every value of `document` to references: a credential value raises
/// `SecretMaterialInDocument` (`surface.edit.secret-in-document`), and an artifact carried
/// in the document raises `ConnectorUploadRefused` (`surface.edit.connector-upload`). The
/// error names the key path, never the value.
pub fn check_document(document: &toml::Value) -> Result<(), SurfaceError> {
    walk(document, &mut Vec::new())
}

fn walk(value: &toml::Value, path: &mut Vec<String>) -> Result<(), SurfaceError> {
    match value {
        toml::Value::Table(t) => {
            for (k, v) in t {
                path.push(k.clone());
                walk(v, path)?;
                path.pop();
            }
            Ok(())
        }
        toml::Value::Array(items) => {
            if is_wasm_bytes(items) {
                return Err(upload(path));
            }
            for (i, v) in items.iter().enumerate() {
                path.push(i.to_string());
                walk(v, path)?;
                path.pop();
            }
            Ok(())
        }
        toml::Value::String(s) => check_string(path, s),
        _ => Ok(()),
    }
}

fn shown(path: &[String]) -> String {
    path.join(".")
}

fn upload(path: &[String]) -> SurfaceError {
    SurfaceError::ConnectorUploadRefused(format!(
        "`{}` carries a connector artifact; the document names a registered connector by id and version, and publishing an artifact runs through the signed registry path",
        shown(path)
    ))
}

fn material(path: &[String]) -> SurfaceError {
    SurfaceError::SecretMaterialInDocument(format!(
        "`{}` holds credential material; the document holds a reference such as `${{secret://<name>}}`, and the secret backend holds the value",
        shown(path)
    ))
}

fn is_credential_key(key: &str) -> bool {
    // Header spellings fold onto config spellings: `X-Api-Token` reads as `x_api_token`.
    let k = key.to_ascii_lowercase().replace('-', "_");
    CREDENTIAL_KEYS.contains(&k.as_str()) || CREDENTIAL_SUFFIXES.iter().any(|s| k.ends_with(s))
}

/// A value naming where material lives rather than holding it.
fn is_reference(value: &str) -> bool {
    let v = value.trim();
    is_locator(v) || Template::parse(v).is_ok_and(|t| t.has_reference())
}

/// A whole value spelled `secret://<name>`, `env://<name>` or `env:<NAME>`.
fn is_locator(v: &str) -> bool {
    v.starts_with("secret://")
        || v.starts_with("env://")
        || v.strip_prefix("env:").is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_'))
}

fn is_wasm_bytes(items: &[toml::Value]) -> bool {
    let magic = [0i64, 0x61, 0x73, 0x6d];
    items.len() >= magic.len() && items.iter().zip(magic).all(|(v, m)| v.as_integer() == Some(m))
}

fn check_string(path: &[String], value: &str) -> Result<(), SurfaceError> {
    let key = path.iter().rev().find(|k| k.parse::<usize>().is_err()).map(String::as_str).unwrap_or_default();
    let trimmed = value.trim();
    if UPLOAD_KEYS.contains(&key) || trimmed.starts_with("data:") || trimmed.starts_with(WASM_BASE64) {
        return Err(upload(path));
    }
    // A whole-value locator names where material lives; the literal scan below reads a
    // template's text around its placeholders.
    if is_locator(trimmed) {
        return Ok(());
    }
    if is_credential_key(key) && !trimmed.is_empty() && !is_reference(trimmed) {
        return Err(material(path));
    }
    match check_material(key, value) {
        Err(ConnectorError::SecretMaterialInDeclaration(_)) => Err(material(path)),
        _ => Ok(()),
    }
}
