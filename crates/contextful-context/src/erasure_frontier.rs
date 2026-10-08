//! One project publication selects the table replacements of an erasure together.

use crate::error::{ContextError, Result};
use crate::store::{etag, Store};
use contextful_core::disclosure::erase::ErasureError;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::PathBuf;

pub(crate) const FRONTIER_FILE: &str = "_erasure_frontier.json";
pub(crate) const CERTIFICATE_FILE: &str = "_erasure_certificate.json";
#[cfg(feature = "read")]
pub(crate) const REFERENCES_FILE: &str = "_erased_references.json";

#[cfg(feature = "read")]
#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ErasedReferences {
    pub version: u32,
    pub store_id: String,
    pub table: String,
    pub references: std::collections::BTreeSet<(String, i64)>,
}

/// The index lives in the immutable certified baseline; normal working writes carry
/// no authority to add an erased verdict.
#[cfg(feature = "read")]
pub(crate) fn erased_references(store: &Store, table: &str) -> Result<std::collections::BTreeSet<(String, i64)>> {
    let Some(frontier) = load(store)? else { return Ok(Default::default()) };
    let Some(replacement) = frontier.tables.get(table) else { return Ok(Default::default()) };
    let path = store.root().join(&replacement.baseline_directory).join(REFERENCES_FILE);
    let bytes = store.metadata().read(&path)?;
    let index: ErasedReferences = serde_json::from_slice(&bytes).map_err(|_| incomplete("the erased-reference index does not decode"))?;
    let identity = crate::project::store_id(store.root()).map_err(|_| incomplete("the erased-reference store identity is absent"))?;
    if index.version != 1 || index.store_id != identity || index.table != table
        || index.references.iter().any(|(run, seq)| run.is_empty() || *seq < 0) {
        return Err(incomplete("the erased-reference index has a foreign or malformed binding"));
    }
    Ok(index.references)
}

#[cfg(feature = "read")]
pub(crate) struct Verifier {
    pub audit_dir: PathBuf,
    pub keys: Vec<contextful_policy::issue::SignerKey>,
    pub source: Option<std::sync::Arc<dyn contextful_policy::keyset::KeySource>>,
}

#[cfg(feature = "read")]
impl std::fmt::Debug for Verifier {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.debug_struct("Verifier").field("audit_dir", &self.audit_dir).field("static_keys", &self.keys.len()).field("live_source", &self.source.is_some()).finish()
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Frontier {
    pub version: u32,
    pub transaction_id: String,
    pub audit_hash: String,
    #[serde(default)]
    pub audit_seq: u64,
    pub tables: BTreeMap<String, TableReplacement>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct TableReplacement {
    pub directory: String,
    pub baseline_directory: String,
    pub certificate_sha256: String,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Certificate {
    pub version: u32,
    pub transaction_id: String,
    pub table: String,
    pub audit_hash: String,
    #[serde(default)]
    pub files: BTreeMap<String, String>,
}

#[cfg(feature = "read")]
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RetiredDirectory {
    pub directory: String,
    pub inventory_sha256: String,
}

fn incomplete(why: &str) -> ContextError {
    ErasureError::ErasureTransactionIncomplete(why.to_string()).into()
}

fn digest(value: &str) -> bool {
    value.len() == 64 && value.bytes().all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c))
}

/// Absence preserves the original layout; a present record validates every selected
/// table before exposing any of them. Metadata sealing uses the Store's key adapter.
pub(crate) fn load(store: &Store) -> Result<Option<Frontier>> {
    let path = store.root().join(FRONTIER_FILE);
    let Some(bytes) = store.metadata().read_optional(&path)? else { return Ok(None) };
    let frontier: Frontier = serde_json::from_slice(&bytes).map_err(|_| incomplete("the project frontier does not decode"))?;
    if frontier.version != 1 || !digest(&frontier.transaction_id) || frontier.audit_hash.is_empty() || frontier.tables.is_empty() {
        return Err(incomplete("the project frontier has an invalid transaction binding"));
    }
    let bindings = verified_bindings(store, &frontier)?;
    for (table, replacement) in &frontier.tables {
        crate::store::check_table_name(table).map_err(|_| incomplete("the frontier names an invalid table"))?;
        let expected = format!("_erasure/committed/{}/tables/{table}", frontier.transaction_id);
        let working = format!("_erasure/working/{}/tables/{table}", frontier.transaction_id);
        if replacement.baseline_directory != expected || replacement.directory != working || !digest(&replacement.certificate_sha256) {
            return Err(incomplete("a table replacement disagrees with its transaction"));
        }
        let directory = store.root().join(&replacement.baseline_directory);
        // Each component remains inside this store; a substituted symlink never
        // redirects an admitted logical table to a foreign directory.
        let mut component = store.root().to_path_buf();
        for segment in replacement.baseline_directory.split('/') {
            component.push(segment);
            let metadata = std::fs::symlink_metadata(&component).map_err(|_| incomplete("a selected table directory is absent"))?;
            if metadata.file_type().is_symlink() || !metadata.is_dir() {
                return Err(incomplete("a selected table directory is not owned store content"));
            }
        }
        let certificate_path = directory.join(CERTIFICATE_FILE);
        let Some(canonical) = store.metadata().read_optional(&certificate_path)? else {
            return Err(incomplete("a selected table certificate is absent"));
        };
        if etag(&canonical) != replacement.certificate_sha256 {
            return Err(incomplete("a selected table certificate has changed"));
        }
        let certificate: Certificate = serde_json::from_slice(&canonical).map_err(|_| incomplete("a table certificate does not decode"))?;
        if certificate.version != 1 || certificate.transaction_id != frontier.transaction_id || certificate.table != *table || certificate.audit_hash != frontier.audit_hash {
            return Err(incomplete("a table certificate disagrees with the project frontier"));
        }
        let inventory = inventory(&directory)?;
        if inventory != certificate.files || inventory.is_empty() || bindings.get(table) != Some(&etag(&serde_json::to_vec(&inventory).map_err(|_| incomplete("replacement inventory does not encode"))?)) {
            return Err(incomplete("the signed replacement inventory disagrees with its files"));
        }
        if !directory.join(contextful_core::store::lay_out::SCHEMA_FILE).is_file() {
            return Err(incomplete("a selected table has no declared schema"));
        }
        component = store.root().to_path_buf();
        for segment in replacement.directory.split('/') {
            component.push(segment);
            let metadata = std::fs::symlink_metadata(&component).map_err(|_| incomplete("the working table directory is absent"))?;
            if metadata.file_type().is_symlink() || !metadata.is_dir() { return Err(incomplete("the working table directory is not owned store content")); }
        }
        if !component.join(contextful_core::store::lay_out::SCHEMA_FILE).is_file() { return Err(incomplete("the working table has no declared schema")); }
    }
    Ok(Some(frontier))
}

#[cfg(not(feature = "read"))]
fn verified_bindings(_store: &Store, _frontier: &Frontier) -> Result<BTreeMap<String, String>> {
    Err(incomplete("this store adapter has no canonical audit verifier"))
}

#[cfg(feature = "read")]
fn verified_bindings(store: &Store, frontier: &Frontier) -> Result<BTreeMap<String, String>> {
    let attributes = verified_attributes(store, frontier)?;
    let bindings: BTreeMap<String, String> = serde_json::from_value(attributes.get("replacement_hashes").cloned().ok_or_else(|| incomplete("the audit entry has no replacement binding"))?)
        .map_err(|_| incomplete("the audit replacement binding does not decode"))?;
    if bindings.keys().ne(frontier.tables.keys()) || bindings.values().any(|value| !digest(value)) {
        return Err(incomplete("the audit entry names another replacement set"));
    }
    Ok(bindings)
}

#[cfg(feature = "read")]
fn trusted_keys(verifier: &Verifier) -> Result<Vec<contextful_policy::issue::SignerKey>> {
    match &verifier.source {
        Some(source) => Ok(source.keys().map_err(|_| incomplete("the configured erasure verification keys are unavailable"))?.keys()
            .map(|key| contextful_policy::issue::SignerKey { algorithm: key.algorithm(), public_key: key.public_key.to_bytes() }).collect()),
        None => Ok(verifier.keys.clone()),
    }
}

#[cfg(feature = "read")]
pub(crate) fn check_signer(store: &Store, audit_dir: &std::path::Path, signer: &contextful_policy::issue::SignerKey) -> Result<()> {
    let verifier = store.erasure_verifier.as_ref().ok_or_else(|| incomplete("no erasure audit verifier is configured"))?;
    if verifier.audit_dir != audit_dir || !trusted_keys(verifier)?.iter().any(|key| key.algorithm == signer.algorithm && key.public_key == signer.public_key) {
        return Err(incomplete("the erasure signing port does not match independently configured trust"));
    }
    Ok(())
}

#[cfg(feature = "read")]
fn verified_attributes(store: &Store, frontier: &Frontier) -> Result<serde_json::Value> {
    use contextful_policy::audit;
    let verifier = store.erasure_verifier.as_ref().ok_or_else(|| incomplete("no erasure audit verifier is configured"))?;
    let keys = trusted_keys(verifier)?;
    if !keys.iter().any(|key| audit::verify_signed(&verifier.audit_dir, key).is_ok()) {
        return Err(incomplete("the erasure audit chain has no trusted signature"));
    }
    let entries = audit::entries(&verifier.audit_dir).map_err(|_| incomplete("the erasure audit chain is unreadable"))?;
    let entry = entries.iter().find(|entry| entry.seq == frontier.audit_seq && entry.entry_hash == frontier.audit_hash)
        .ok_or_else(|| incomplete("the erasure audit entry is absent"))?;
    let attributes = &entry.attributes;
    let store_id = crate::project::store_id(store.root()).map_err(|_| incomplete("the erasure store identity is unavailable"))?;
    if attributes.get("operation").and_then(serde_json::Value::as_str) != Some("erasure")
        || attributes.get("store_id").and_then(serde_json::Value::as_str) != Some(store_id.as_str())
        || attributes.get("transaction_id").and_then(serde_json::Value::as_str) != Some(frontier.transaction_id.as_str()) {
        return Err(incomplete("the erasure audit entry binds another store or transaction"));
    }
    Ok(attributes.clone())
}

#[cfg(feature = "read")]
pub(crate) fn retired_directories(store: &Store, frontier: &Frontier) -> Result<BTreeMap<String, Vec<RetiredDirectory>>> {
    let attributes = verified_attributes(store, frontier)?;
    let retired: BTreeMap<String, Vec<RetiredDirectory>> = serde_json::from_value(attributes.get("retired_directories").cloned().ok_or_else(|| incomplete("the audit entry has no retirement binding"))?)
        .map_err(|_| incomplete("the audit retirement binding does not decode"))?;
    if retired.keys().ne(frontier.tables.keys()) { return Err(incomplete("the retirement set disagrees with the published tables")); }
    for (table, directories) in &retired {
      if directories.is_empty() { return Err(incomplete("a published table has no signed retirement binding")); }
      for old in directories {
        let original = format!("tables/{table}");
        let older = old.directory.strip_prefix("_erasure/working/").or_else(|| old.directory.strip_prefix("_erasure/committed/")).and_then(|tail| tail.split_once("/tables/"));
        let valid_older = older.is_some_and(|(transaction, name)| digest(transaction) && transaction != frontier.transaction_id && name == table);
        if (old.directory != original && !valid_older) || !digest(&old.inventory_sha256) {
            return Err(incomplete("a retired directory has an invalid store-local binding"));
        }
      }
    }
    Ok(retired)
}

pub(crate) fn inventory(directory: &std::path::Path) -> Result<BTreeMap<String, String>> {
    fn visit(root: &std::path::Path, current: &std::path::Path, files: &mut BTreeMap<String, String>) -> Result<()> {
        for entry in std::fs::read_dir(current).map_err(|_| incomplete("replacement directory is unreadable"))? {
            let entry = entry.map_err(|_| incomplete("replacement directory entry is unreadable"))?;
            let path = entry.path();
            let kind = entry.file_type().map_err(|_| incomplete("replacement file type is unreadable"))?;
            if kind.is_symlink() {
                return Err(incomplete("a replacement contains a substituted symlink"));
            }
            if kind.is_dir() {
                visit(root, &path, files)?;
            } else if kind.is_file() {
                if path == root.join(CERTIFICATE_FILE) { continue; }
                let relative = path.strip_prefix(root).map_err(|_| incomplete("a replacement file escapes its directory"))?;
                let name = relative.to_str().ok_or_else(|| incomplete("a replacement path is not text"))?.replace('\\', "/");
                let bytes = std::fs::read(&path).map_err(|_| incomplete("a replacement file is unreadable"))?;
                files.insert(name, etag(&bytes));
            } else {
                return Err(incomplete("a replacement contains a non-regular file"));
            }
        }
        Ok(())
    }
    let mut files = BTreeMap::new();
    visit(directory, directory, &mut files)?;
    Ok(files)
}

pub(crate) fn table_dir(store: &Store, table: &str) -> Result<PathBuf> {
    let frontier = load(store)?;
    Ok(match frontier.as_ref().and_then(|f| f.tables.get(table)) {
        Some(replacement) => store.root().join(&replacement.directory),
        None => store.root().join("tables").join(table),
    })
}
