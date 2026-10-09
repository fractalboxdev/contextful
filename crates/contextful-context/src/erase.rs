//! Erasure stages every retained row version under the canonical project fence.

use crate::{ContextError, Result, Store};
use crate::error::IoPath;
use crate::erasure_frontier::{relative_path, Certificate, Frontier, RetiredDirectory, TableReplacement, CERTIFICATE_FILE, FRONTIER_FILE};
use arrow_array::{BooleanArray, RecordBatch};
use arrow_select::{concat::concat_batches, filter::filter_record_batch};
use contextful_core::disclosure::erase::{column_key_set, select_column_keys, select_keys, select_subject, ErasureError, RetainedRows};
use contextful_core::ports::{Clock, SigningPort};
use contextful_core::store::declare::TableDecl;
use contextful_core::store::lay_out::{SnapshotManifest, MANIFEST_FILE, SNAPSHOTS_DIR};
use contextful_core::store::index::{IndexEntry, IndexKind};
use contextful_core::{AuthorityError, issue::SignatureEncoding};
use contextful_policy::audit::{self, AuditLog};
#[cfg(all(test, feature = "read"))]
#[path = "erase_recovery_tests.rs"]
mod recovery_tests;
use contextful_policy::enforce::erase::ForgetAdmission;
use contextful_policy::issue::SignerKey;
use contextful_policy::verify::AdmittedAuthority;
use serde_json::json;
use std::{collections::BTreeMap, path::{Path, PathBuf}, sync::Arc};

pub enum EraseSelector<'a> {
    Subject(&'a str),
    KeySet { subject_hash: &'a str, keys: &'a RetainedRows },
    ColumnKeySet { subject_hash: &'a str, column: &'a str, keys: &'a [serde_json::Value] },
}

pub struct EraseRequest<'a> {
    pub declarations: &'a [TableDecl],
    pub tables: &'a [String],
    pub selector: EraseSelector<'a>,
    pub admission: &'a ForgetAdmission<'a>,
    pub signer: Option<Arc<dyn SigningPort + Send + Sync>>,
    pub audit_dir: &'a Path,
    pub audit_key: &'a [u8],
    pub boundary: &'a dyn Fn(&AdmittedAuthority) -> std::result::Result<(), AuthorityError>,
    pub clock: &'a dyn Clock,
}

pub struct Erased {
    pub store: Store,
    pub transaction_id: String,
    pub subject_hash: String,
    pub affected_counts: BTreeMap<String, usize>,
}

struct ConfiguredSigner(Arc<dyn SigningPort + Send + Sync>);
impl SigningPort for ConfiguredSigner {
    fn encoding(&self) -> SignatureEncoding { self.0.encoding() }
    fn public_key(&self) -> Vec<u8> { self.0.public_key() }
    fn sign(&self, message: &[u8]) -> std::result::Result<Vec<u8>, AuthorityError> { self.0.sign(message) }
}

fn unsupported(reason: &str) -> ContextError { ErasureError::ErasureScopeUnsupported(reason.into()).into() }
fn incomplete(reason: &str) -> ContextError { ErasureError::ErasureTransactionIncomplete(reason.into()).into() }

fn key_set_hash(subject_hash: &str) -> Result<String> {
    let digest = subject_hash.strip_prefix("hmac-sha256:").ok_or_else(|| unsupported("a key set has no opaque subject hash"))?;
    if digest.len() != 64 || !digest.bytes().all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c)) { return Err(unsupported("a key set has an invalid opaque subject hash")); }
    Ok(subject_hash.to_string())
}

fn key_set_scope(tables: &[String], keys: &RetainedRows) -> Result<()> {
    let requested = tables.iter().collect::<std::collections::BTreeSet<_>>();
    if requested.len() != tables.len() || requested != keys.keys().collect() { return Err(unsupported("the key set disagrees with the requested tables")); }
    Ok(())
}

fn files(root: &Path) -> Result<Vec<PathBuf>> {
    fn visit(root: &Path, list: &mut Vec<PathBuf>) -> Result<()> {
        for entry in std::fs::read_dir(root).at(root)? {
            let entry = entry.at(root)?;
            let kind = entry.file_type().at(entry.path())?;
            if kind.is_symlink() { return Err(unsupported("erasure refuses substituted table content")); }
            if kind.is_dir() { visit(&entry.path(), list)?; }
            else if kind.is_file() { list.push(entry.path()); }
            else { return Err(unsupported("erasure requires regular retained table files")); }
        }
        Ok(())
    }
    let mut list = Vec::new(); visit(root, &mut list)?; list.sort(); Ok(list)
}

fn revalidate(request: &EraseRequest<'_>) -> Result<()> {
    (request.boundary)(request.admission.authority()).map_err(|error| ContextError::Invalid(error.to_string()))
}

/// A missing signer or live Forget boundary refuses before any table content reads.
pub fn erase_subject(store: &Store, request: EraseRequest<'_>) -> Result<Erased> {
    if !matches!(request.selector, EraseSelector::Subject(_)) { return Err(unsupported("subject erasure requires a subject selector")); }
    erase(store, request)
}

/// Subject and key-set selectors enter the same admitted publication adapter.
pub fn erase(store: &Store, request: EraseRequest<'_>) -> Result<Erased> {
    revalidate(&request)?;
    let signer = request.signer.as_ref().ok_or_else(|| incomplete("no erasure signing port is configured"))?;
    if request.audit_key.is_empty() { return Err(incomplete("no project audit key is configured")); }
    let all: Vec<&str> = request.declarations.iter().map(|decl| decl.name.as_str()).collect();
    ForgetAdmission::admit(request.admission.authority(), &all)?;
    let key = SignerKey::of(signer.as_ref());
    crate::erasure_frontier::check_signer(store, request.audit_dir, &key)?;
    let configured = store.clone();
    configured.with_frontier(|bound| {
        revalidate(&request)?;
        if request.audit_key != bound.canonical_audit_key()? { return Err(incomplete("the supplied audit key disagrees with the canonical project key")); }
        let mut batches: BTreeMap<String, Vec<(PathBuf, Vec<RecordBatch>)>> = BTreeMap::new();
        let mut rows = RetainedRows::new();
        let mut directories = BTreeMap::new();
        for decl in request.declarations {
            let directory = bound.table_dir(&decl.name)?;
            let mut retained = Vec::new();
            let mut parts = Vec::new();
            for path in files(&directory)? {
                if path.extension().is_some_and(|extension| extension == "parquet") {
                    let content = bound.read_parquet(&path)?;
                    for batch in &content {
                        let schema = batch.schema();
                        let columns: Vec<&str> = schema.fields().iter().map(|field| field.name().as_str()).collect();
                        retained.extend(crate::rows::batch_rows(batch, &columns)?);
                    }
                    parts.push((path.strip_prefix(&directory).map_err(|_| incomplete("retained part escapes its table"))?.to_path_buf(), content));
                }
            }
            directories.insert(decl.name.clone(), directory);
            rows.insert(decl.name.clone(), retained);
            batches.insert(decl.name.clone(), parts);
        }
        let (selected, subject_hash) = match &request.selector {
            EraseSelector::Subject(subject) => (select_subject(request.declarations, &rows, request.tables, subject)?,
                audit::query_digest(request.audit_key, &format!("contextful.erasure.subject\n{subject}"))),
            EraseSelector::KeySet { subject_hash, keys } => {
                key_set_scope(request.tables, keys)?;
                (select_keys(request.declarations, &rows, keys)?, key_set_hash(subject_hash)?)
            }
            EraseSelector::ColumnKeySet { subject_hash, column, keys } => {
                key_set_scope(request.tables, &column_key_set(request.declarations, column, keys)?)?;
                (select_column_keys(request.declarations, &rows, column, keys)?, key_set_hash(subject_hash)?)
            }
        };
        let mut nonce = [0; 32]; getrandom::fill(&mut nonce).map_err(|_| incomplete("erasure transaction identity is unavailable"))?;
        let transaction_id = crate::store::etag(&nonce);
        let staging = bound.root().join("_erasure/staging").join(&transaction_id);
        let committed = bound.root().join("_erasure/committed").join(&transaction_id);
        let working = bound.root().join("_erasure/working").join(&transaction_id);
        std::fs::create_dir_all(staging.join("tables")).at(&staging)?;
        let old_frontier = crate::erasure_frontier::load(bound)?;
        let mut targets: Vec<&str> = selected.affected_tables().collect();
        if let Some(frontier) = &old_frontier { targets.extend(frontier.tables.keys().map(String::as_str)); }
        targets.sort(); targets.dedup();
        if targets.is_empty() { std::fs::remove_dir_all(&staging).at(&staging)?; return Err(unsupported("erasure selects no retained row versions")); }
        let mut hashes = BTreeMap::new();
        let mut retired = BTreeMap::new();
        let mut inventories = BTreeMap::new();
        let mut counts = BTreeMap::new();
        let store_id = crate::project::store_id(bound.root()).map_err(|_| incomplete("the erasure store identity is unavailable"))?;
        for name in &targets {
            let decl = request.declarations.iter().find(|decl| decl.name == *name).ok_or_else(|| unsupported("a selected frontier table has no declaration"))?;
            let source = &directories[*name];
            let old_inventory = crate::erasure_frontier::retirement_inventory(source)?;
            let old_directory = source.strip_prefix(bound.root()).map_err(|_| incomplete("the retired directory escapes its store"))?.components().map(|component| component.as_os_str().to_str().ok_or_else(|| incomplete("the retired path is not text"))).collect::<Result<Vec<_>>>()?.join("/");
            let old_inventory = crate::erasure_frontier::opaque_retirement_inventory(bound, &transaction_id, &old_directory, old_inventory)?;
            let mut old = vec![RetiredDirectory { version:3, directory:old_directory, inventory_sha256:crate::store::etag(&serde_json::to_vec(&old_inventory).map_err(|_| incomplete("the retired inventory does not encode"))?), files:Some(old_inventory) }];
            if let Some(previous) = old_frontier.as_ref().and_then(|frontier| frontier.tables.get(*name)) {
                let baseline = bound.root().join(&previous.baseline_directory);
                let inventory = crate::erasure_frontier::retirement_inventory(&baseline)?;
                let inventory = crate::erasure_frontier::opaque_retirement_inventory(bound, &transaction_id, &previous.baseline_directory, inventory)?;
                old.push(RetiredDirectory { version:3, directory:previous.baseline_directory.clone(), inventory_sha256:crate::store::etag(&serde_json::to_vec(&inventory).map_err(|_| incomplete("the prior baseline inventory does not encode"))?), files:Some(inventory) });
            }
            retired.insert((*name).to_string(), old);
            let destination = staging.join("tables").join(name);
            std::fs::create_dir_all(&destination).at(&destination)?;
            for path in files(source)? {
                if path.file_name().is_some_and(|name| name == CERTIFICATE_FILE) { continue; }
                let relative = path.strip_prefix(source).map_err(|_| incomplete("a retained file escapes its table"))?;
                let copied = destination.join(relative);
                std::fs::create_dir_all(copied.parent().ok_or_else(|| incomplete("a retained file has no parent"))?).at(&copied)?;
                std::fs::copy(&path, &copied).at(&copied)?;
            }
            let mask = selected.removes(name);
            counts.insert((*name).to_string(), mask.iter().filter(|removed| **removed).count());
            let mut offset = 0;
            for (relative, content) in &batches[*name] {
                let mut rewritten = Vec::new();
                for batch in content {
                    let end = offset + batch.num_rows();
                    let slice = mask.get(offset..end).ok_or_else(|| incomplete("retained rows disagree with their erasure selection"))?;
                    let keep = BooleanArray::from(slice.iter().map(|removed| !removed).collect::<Vec<_>>());
                    rewritten.push(filter_record_batch(batch, &keep).map_err(|_| incomplete("retained batch cannot be rewritten"))?);
                    offset = end;
                }
                let combined = match rewritten.first() {
                    Some(first) => concat_batches(&first.schema(), &rewritten).map_err(|_| incomplete("retained batches disagree on their schema"))?,
                    None => empty_part(bound, &source.join(relative))?,
                };
                bound.write_parquet(&destination.join(relative), &combined)?;
            }
            rebuild_snapshots(bound, decl, &destination)?;
            let mut references = crate::erasure_frontier::erased_references(bound, name)?;
            for (row, removed) in rows[*name].iter().zip(mask) {
                if !removed { continue; }
                let run = row.get(contextful_core::store::reserve::RUN_ID).and_then(serde_json::Value::as_str)
                    .ok_or_else(|| incomplete("an erased row has no run identity"))?;
                let seq = row.get(contextful_core::store::reserve::ROW_SEQ).and_then(serde_json::Value::as_i64)
                    .ok_or_else(|| incomplete("an erased row has no row sequence"))?;
                if run.is_empty() || seq < 0 { return Err(incomplete("an erased row has an invalid reference")); }
                references.insert((run.to_string(), seq));
            }
            let index = crate::erasure_frontier::ErasedReferences { version:1, store_id:store_id.clone(), table:(*name).to_string(), references };
            bound.metadata().write(&destination.join(crate::erasure_frontier::REFERENCES_FILE), &serde_json::to_vec(&index).map_err(|_| incomplete("the erased-reference index does not encode"))?)?;
            let inventory = crate::erasure_frontier::inventory(&destination)?;
            hashes.insert((*name).to_string(), crate::store::etag(&serde_json::to_vec(&inventory).map_err(|_| incomplete("replacement inventory does not encode"))?));
            inventories.insert((*name).to_string(), inventory);
        }
        revalidate(&request)?;
        let log = AuditLog::anchor(request.audit_dir, ConfiguredSigner(signer.clone())).map_err(|_| incomplete("the canonical audit chain cannot open under the erasure signer"))?;
        let entry = log.append(json!({"operation":"erasure","store_id":store_id,"transaction_id":transaction_id,"subject_hash":subject_hash,"affected_counts":counts,"replacement_hashes":hashes,"retired_directories":retired,"executed_at":request.clock.now().to_rfc3339()}))
            .map_err(|_| incomplete("the row-free erasure audit entry did not persist"))?;
        log.export().map_err(|_| incomplete("the erasure audit evidence did not sign"))?;
        drop(log);
        let mut replacements = BTreeMap::new();
        for name in &targets {
            let certificate = Certificate { version:1, transaction_id:transaction_id.clone(), table:(*name).into(), audit_hash:entry.entry_hash.clone(), files:inventories.remove(*name).ok_or_else(|| incomplete("replacement inventory is absent"))? };
            let bytes = serde_json::to_vec(&certificate).map_err(|_| incomplete("replacement certificate does not encode"))?;
            let path = staging.join("tables").join(name).join(CERTIFICATE_FILE);
            bound.metadata().write(&path, &bytes)?;
            replacements.insert((*name).into(), TableReplacement { directory:format!("_erasure/working/{transaction_id}/tables/{name}"), baseline_directory:format!("_erasure/committed/{transaction_id}/tables/{name}"), certificate_sha256:crate::store::etag(&bytes) });
        }
        sync_tree(&staging)?;
        std::fs::create_dir_all(committed.parent().ok_or_else(|| incomplete("the committed transaction has no parent"))?).at(&committed)?;
        std::fs::rename(&staging, &committed).at(&committed)?;
        contextful_fs::open_dir_for_sync(committed.parent().ok_or_else(|| incomplete("the committed transaction has no parent"))?).at(&committed)?.sync_all().at(&committed)?;
        for name in &targets {
            let baseline = committed.join("tables").join(name);
            let current = working.join("tables").join(name);
            for source in files(&baseline)? {
                if source.file_name().is_some_and(|name| name == CERTIFICATE_FILE) { continue; }
                let relative = source.strip_prefix(&baseline).map_err(|_| incomplete("a baseline file escapes its table"))?;
                let destination = current.join(relative);
                std::fs::create_dir_all(destination.parent().ok_or_else(|| incomplete("a working file has no parent"))?).at(&destination)?;
                std::fs::copy(&source, &destination).at(&destination)?;
            }
            if crate::erasure_frontier::inventory(&baseline)? != crate::erasure_frontier::inventory(&current)? { return Err(incomplete("a working copy disagrees with its signed baseline")); }
        }
        sync_tree(&working)?;
        for old in retired.values().flatten() {
            let path = bound.root().join(&old.directory);
            let current = crate::erasure_frontier::retirement_inventory(&path)?;
            let current = crate::erasure_frontier::opaque_retirement_inventory(bound, &transaction_id, &old.directory, current)?;
            if crate::store::etag(&serde_json::to_vec(&current).map_err(|_| incomplete("the retired inventory does not encode"))?) != old.inventory_sha256 {
                return Err(incomplete("the retired inventory changed before publication"));
            }
        }
        revalidate(&request)?;
        let frontier = Frontier { version:1, transaction_id:transaction_id.clone(), audit_hash:entry.entry_hash, audit_seq:entry.seq, tables:replacements };
        let bytes = serde_json::to_vec(&frontier).map_err(|_| incomplete("the project frontier does not encode"))?;
        bound.metadata().replace(&bound.root().join(FRONTIER_FILE), &bytes)?;
        crate::erasure_frontier::load(bound)?;
        recover_committed_erasure(bound)?;
        Ok(Erased { store:configured.clone(), transaction_id, subject_hash, affected_counts:counts })
    })
}

/// Completes an already signed erasure's physical collection without selecting
/// additional rows. Every retirement binding validates before the first deletion.
/// This trusted store-owner API relies on previously authenticated signed intent;
/// external adapters use `recover_admitted_erasure` for fresh effect admission.
pub fn recover_committed_erasure(store: &Store) -> Result<()> {
    recover_collection(store, |_, _, _| Ok(()))
}

/// Resumes collection under verified Forget authority for its complete owned scope.
/// No selector, signing port or caller-selected table subset enters this adapter.
pub fn recover_admitted_erasure(
    store: &Store,
    authority: &AdmittedAuthority,
    boundary: &dyn Fn(&AdmittedAuthority) -> std::result::Result<(), AuthorityError>,
) -> Result<Option<String>> {
    let mut transaction = None;
    recover_collection(store, |bound, frontier, staged| {
        let names = match (frontier, staged.is_empty()) {
            (Some(frontier), true) => frontier.tables.keys().cloned().collect(),
            _ => {
                let names = bound.tables()?;
                validate_staged_origins(staged, &names)?;
                names
            }
        };
        let tables: Vec<&str> = names.iter().map(String::as_str).collect();
        let admitted = ForgetAdmission::admit(authority, &tables)?;
        boundary(admitted.authority()).map_err(|error| ContextError::Invalid(error.to_string()))?;
        bound.canonical_audit_key()?;
        transaction = frontier.map(|frontier| frontier.transaction_id.clone());
        Ok(())
    })?;
    Ok(transaction)
}

fn validate_staged_origins(staged: &[PathBuf], tables: &[String]) -> Result<()> {
    fn visit(directory: &Path, prefix: &str, tables: &[String], origins: &mut std::collections::BTreeSet<String>) -> Result<()> {
        for entry in std::fs::read_dir(directory).at(directory)? {
            let entry = entry.at(directory)?;
            let name = entry.file_name();
            let name = name.to_str().ok_or_else(|| incomplete("a staged origin is not text"))?;
            let relative = if prefix.is_empty() { name.to_string() } else { format!("{prefix}/{name}") };
            relative_path(&relative)?;
            let kind = entry.file_type().at(entry.path())?;
            let within = tables.iter().find(|table| relative.strip_prefix(table.as_str()).is_some_and(|tail| tail.starts_with('/')));
            if kind.is_dir() && !kind.is_symlink() {
                if tables.iter().any(|table| table == &relative) { origins.insert(relative.clone()); }
                else if within.is_none() && !tables.iter().any(|table| table.strip_prefix(&relative).is_some_and(|tail| tail.starts_with('/'))) {
                    return Err(incomplete("a staged directory has no canonical table origin"));
                }
                visit(&entry.path(), &relative, tables, origins)?;
            } else if kind.is_file() && within.is_some() {
                origins.insert(within.expect("a staged file has a canonical origin").clone());
            } else {
                return Err(incomplete("a staged file has no canonical table origin"));
            }
        }
        Ok(())
    }
    for stage in staged {
        match std::fs::symlink_metadata(stage) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => (),
            _ => return Err(incomplete("the staged transaction is not owned store content")),
        }
        let entries = std::fs::read_dir(stage).at(stage)?.collect::<std::io::Result<Vec<_>>>().at(stage)?;
        if entries.len() != 1 || entries[0].file_name() != "tables" || !entries[0].file_type().at(entries[0].path())?.is_dir() {
            return Err(incomplete("the staged transaction has no unambiguous table namespace"));
        }
        let mut origins = std::collections::BTreeSet::new();
        visit(&entries[0].path(), "", tables, &mut origins)?;
        if origins.is_empty() { return Err(incomplete("the staged transaction has no canonical table origin")); }
    }
    Ok(())
}

fn recover_collection(store: &Store, mut boundary: impl FnMut(&Store, Option<&Frontier>, &[PathBuf]) -> Result<()>) -> Result<()> {
    store.with_frontier(|bound| {
        let frontier = crate::erasure_frontier::load(bound)?;
        let retired = match &frontier {
            Some(frontier) => crate::erasure_frontier::retired_directories(bound, frontier)?,
            None => BTreeMap::new(),
        };
        let mut present = Vec::new();
        let erasure = bound.root().join("_erasure");
        let staging = erasure.join("staging");
        for directory in [&erasure, &staging] {
            match std::fs::symlink_metadata(directory) {
                Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => (),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => (),
                _ => return Err(incomplete("the recovery staging namespace is not owned store content")),
            }
        }
        match std::fs::read_dir(&staging) {
            Ok(entries) => for entry in entries {
                let entry = entry.at(&staging)?;
                let name = entry.file_name();
                let name = name.to_str().ok_or_else(|| incomplete("a staged transaction identity is not text"))?;
                let kind = entry.file_type().at(entry.path())?;
                if name.len() != 64 || !name.bytes().all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
                    || kind.is_symlink() || !kind.is_dir()
                    || frontier.as_ref().is_some_and(|frontier| frontier.transaction_id == name) {
                    return Err(incomplete("recovery found an ambiguous staged transaction"));
                }
                files(&entry.path())?;
                present.push(entry.path());
            },
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => (),
            Err(_) => return Err(incomplete("the recovery staging namespace is unreadable")),
        }
        let staged = present.clone();
        boundary(bound, frontier.as_ref(), &staged)?;
        for old in retired.values().flatten() {
            let path = bound.root().join(&old.directory);
            let mut component = bound.root().to_path_buf();
            let mut absent = false;
            for segment in old.directory.split('/') {
                component.push(segment);
                match std::fs::symlink_metadata(&component) {
                    Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => (),
                    Ok(_) => return Err(incomplete("a retired directory is not owned store content")),
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => { absent = true; break; },
                    Err(_) => return Err(incomplete("a retired directory is unreadable")),
                }
            }
            if absent { continue; }
            if old.version == 1 && path.join(CERTIFICATE_FILE).exists() {
                return Err(incomplete("a hash-only retirement cannot admit its unbound certificate"));
            }
            let inventory = crate::erasure_frontier::retirement_inventory(&path)?;
            let inventory = if old.version == 3 {
                crate::erasure_frontier::opaque_retirement_inventory(bound, &frontier.as_ref().ok_or_else(|| incomplete("retirement has no published frontier"))?.transaction_id, &old.directory, inventory)?
            } else { inventory };
            let admitted = match &old.files {
                Some(files) => inventory.iter().all(|(path, digest)| files.get(path) == Some(digest)),
                None => crate::store::etag(&serde_json::to_vec(&inventory).map_err(|_| incomplete("the retired inventory does not encode"))?) == old.inventory_sha256,
            };
            if !admitted { return Err(incomplete("a retired inventory changed after signed admission")); }
            present.push(path);
        }
        for path in present {
            let current = crate::erasure_frontier::load(bound)?;
            boundary(bound, current.as_ref(), &staged)?;
            std::fs::remove_dir_all(&path).at(&path)?;
            let parent = path.parent().ok_or_else(|| incomplete("a retired directory has no parent"))?;
            contextful_fs::open_dir_for_sync(parent).at(parent)?.sync_all().at(parent)?;
        }
        Ok(())
    })
}

fn rebuild_snapshots(store: &Store, decl: &TableDecl, directory: &Path) -> Result<()> {
    for path in files(directory)? {
        if path.file_name().is_none_or(|name| name != MANIFEST_FILE) { continue; }
        if path.parent().and_then(Path::parent) != Some(directory.join(SNAPSHOTS_DIR).as_path()) { continue; }
        let bytes = store.metadata().read(&path)?;
        let mut manifest: SnapshotManifest = serde_json::from_slice(&bytes).map_err(|_| incomplete("a retained snapshot manifest does not decode"))?;
        if manifest.table != decl.name { return Err(incomplete("a retained snapshot names another table")); }
        let snapshot = path.parent().ok_or_else(|| incomplete("a retained snapshot has no directory"))?;
        let mut batches = Vec::new();
        for part in &manifest.parts { batches.extend(store.read_parquet(&snapshot.join(relative_path(&part.name)?))?); }
        let rows = match batches.first() {
            Some(first) => concat_batches(&first.schema(), &batches).map_err(|_| incomplete("a retained snapshot has incompatible parts"))?,
            None => match manifest.parts.first() {
                Some(part) => empty_part(store, &snapshot.join(relative_path(&part.name)?))?,
                None => RecordBatch::new_empty(crate::parquet_io::arrow_schema(&store.schema(&decl.name)?)),
            },
        };
        for part in &manifest.parts {
            let old = snapshot.join(relative_path(&part.name)?);
            std::fs::remove_file(&old).at(&old)?;
        }
        fn remove_empty_directories(directory: &Path) -> Result<()> {
            for entry in std::fs::read_dir(directory).at(directory)? {
                let entry = entry.at(directory)?;
                if entry.file_type().at(entry.path())?.is_dir() {
                    remove_empty_directories(&entry.path())?;
                    if std::fs::read_dir(entry.path()).at(entry.path())?.next().is_none() { std::fs::remove_dir(entry.path()).at(entry.path())?; }
                }
            }
            Ok(())
        }
        remove_empty_directories(snapshot)?;
        let name = contextful_core::store::lay_out::part_name(0);
        store.write_parquet(&snapshot.join(&name), &rows)?;
        manifest.parts = vec![contextful_core::store::lay_out::PartEntry { name, key_version:store.sealing().key_version() }];
        for index in &manifest.indexes {
            if matches!(index, IndexEntry::Unrecognized(_)) { return Err(unsupported("an unrecognized sidecar has no erasure rewrite adapter")); }
            let relative = index.path().ok_or_else(|| unsupported("a sidecar names no physical directory"))?;
            let path = snapshot.join(relative_path(relative)?);
            if path.exists() { std::fs::remove_dir_all(&path).at(&path)?; }
        }
        manifest.indexes = decl.indexes().iter().map(|index| match index.kind {
            IndexKind::Vector => crate::vector::build(snapshot, &manifest.snapshot_id, &rows, decl, index, &store.sealing()).map(IndexEntry::Vector),
            IndexKind::Fulltext => crate::fulltext::build(snapshot, &manifest.snapshot_id, &rows, decl, index, &store.sealing()).map(IndexEntry::Fulltext),
        }).collect::<Result<_>>()?;
        manifest.row_count = rows.num_rows() as u64;
        store.metadata().write(&path, &serde_json::to_vec(&manifest).map_err(|_| incomplete("the rewritten snapshot manifest does not encode"))?)?;
    }
    Ok(())
}

fn empty_part(store: &Store, path: &Path) -> Result<RecordBatch> {
    let columns = store.parquet_schema(path)?;
    let schema = contextful_core::store::reconcile::Schema { columns };
    Ok(RecordBatch::new_empty(crate::parquet_io::arrow_schema(&schema)))
}

fn sync_tree(directory: &Path) -> Result<()> {
    for entry in std::fs::read_dir(directory).at(directory)? {
        let entry = entry.at(directory)?;
        let path = entry.path();
        let kind = entry.file_type().at(&path)?;
        if kind.is_dir() { sync_tree(&path)?; }
        else if kind.is_file() { std::fs::File::open(&path).at(&path)?.sync_all().at(&path)?; }
        else { return Err(unsupported("a staged replacement has unowned content")); }
    }
    contextful_fs::open_dir_for_sync(directory).at(directory)?.sync_all().at(directory)
}
