//! Erasure stages every retained row version under the canonical project fence.

use crate::{ContextError, Result, Store};
use crate::error::IoPath;
use crate::erasure_frontier::{relative_path, Certificate, Frontier, RetiredDirectory, TableReplacement, CERTIFICATE_FILE, FRONTIER_FILE};
use arrow_array::{BooleanArray, RecordBatch};
use arrow_select::{concat::concat_batches, filter::filter_record_batch};
use contextful_core::disclosure::erase::{column_key_set_declared, select_column_keys_declared, select_dead_letters, select_keys, select_subject, select_tenant, tenant_columns, ErasureError, ErasureSelection, RetainedRows, TableTypes};
use contextful_core::ports::{Clock, SigningPort};
use contextful_core::store::declare::{DeclarationSet, TableDecl};
use contextful_core::store::lay_out::{SnapshotManifest, MANIFEST_FILE, SNAPSHOTS_DIR};
use contextful_core::store::index::{IndexEntry, IndexKind};
use contextful_core::{AuthorityError, issue::SignatureEncoding};
use contextful_policy::audit::{self, AuditLog};
#[cfg(all(test, feature = "read"))]
#[path = "erase_recovery_tests.rs"]
mod recovery_tests;
use contextful_policy::enforce::erase::ForgetAdmission;
use contextful_policy::issue::SignerKey;
use contextful_policy::receipt::{Coverage, ReceiptClaim, TenantReceipt, RECEIPT_VERSION};
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

/// Subject and key-set selectors enter the same admitted publication adapter.
pub fn erase(store: &Store, request: EraseRequest<'_>) -> Result<Erased> {
    erase_declared(store, &DeclarationSet::from_tables(request.declarations), request)
}

/// Erases through a canonical pack without dropping its separately declared column identities.
pub fn erase_declared(store: &Store, declarations: &DeclarationSet, request: EraseRequest<'_>) -> Result<Erased> {
    if request.declarations != declarations.tables() { return Err(unsupported("the request disagrees with the canonical declaration pack")); }
    revalidate(&request)?;
    let signer = request.signer.as_ref().ok_or_else(|| incomplete("no erasure signing port is configured"))?;
    if request.audit_key.is_empty() { return Err(incomplete("no project audit key is configured")); }
    let all: Vec<&str> = request.declarations.iter().map(|decl| decl.name.as_str()).collect();
    ForgetAdmission::admit(request.admission.authority(), &all)?;
    let guard = || revalidate(&request);
    let select = |rows: &RetainedRows| -> Result<(ErasureSelection, String)> {
        Ok(match &request.selector {
            EraseSelector::Subject(subject) => (select_subject(request.declarations, rows, request.tables, subject)?,
                audit::query_digest(request.audit_key, &format!("contextful.erasure.subject\n{subject}"))),
            EraseSelector::KeySet { subject_hash, keys } => {
                key_set_scope(request.tables, keys)?;
                (select_keys(request.declarations, rows, keys)?, key_set_hash(subject_hash)?)
            }
            EraseSelector::ColumnKeySet { subject_hash, column, keys } => {
                key_set_scope(request.tables, &column_key_set_declared(declarations, column, keys)?)?;
                (select_column_keys_declared(declarations, rows, column, keys)?, key_set_hash(subject_hash)?)
            }
        })
    };
    let admit_letters = |letters: &[&str]| -> Result<()> {
        if !letters.is_empty() { ForgetAdmission::admit(request.admission.authority(), letters)?; }
        Ok(())
    };
    publish(store, &Plan { declarations: request.declarations, signer, audit_dir: request.audit_dir, audit_key: request.audit_key, clock: request.clock, guard: &guard, admit_letters: &admit_letters, select: &select, allow_empty: false })
}

/// A store owner's tenant purge; the caller presents no capability credential.
pub struct PurgeRequest<'a> {
    pub declarations: &'a [TableDecl],
    pub tenant: &'a str,
    pub signer: Option<Arc<dyn SigningPort + Send + Sync>>,
    pub audit_dir: &'a Path,
    pub audit_key: &'a [u8],
    pub clock: &'a dyn Clock,
}

pub struct Purged {
    pub store: Store,
    pub receipt: TenantReceipt,
}

const RECEIPT_OPERATION: &str = "tenant-receipt";

/// Rewrite every row whose bare outermost partition column holds the tenant's exact bytes
/// out of every table through one signed publication, then sign a fresh salted receipt
/// and record its digest on the audit chain. The receipt's leak and coverage checks run
/// on the prospective claim before the rewrite, so a refusal leaves every row in place.
pub fn purge_tenant(store: &Store, request: PurgeRequest<'_>) -> Result<Purged> {
    let signer = request.signer.as_ref().ok_or_else(|| incomplete("no purge signing port is configured"))?;
    if request.audit_key.is_empty() { return Err(incomplete("no project audit key is configured")); }
    let mut types = TableTypes::new();
    for decl in request.declarations {
        if let Some(schema) = store.try_schema(&decl.name)? {
            types.insert(decl.name.clone(), schema.columns.into_iter().map(|column| (column.name, column.ty)).collect());
        }
    }
    let columns = tenant_columns(request.declarations, &types)?;
    let mut nonce = [0; 32]; getrandom::fill(&mut nonce).map_err(|_| incomplete("the receipt request identity is unavailable"))?;
    let request_id = crate::store::etag(&nonce);
    let store_id = crate::project::store_id(store.root()).map_err(|_| incomplete("the purge store identity is unavailable"))?;
    let coverage = Coverage::of_version(RECEIPT_VERSION).ok_or_else(|| incomplete("the receipt version declares no coverage"))?;
    let issued_at = request.clock.now().to_rfc3339();
    let prospective = ReceiptClaim {
        request_id: request_id.clone(),
        transaction_id: String::new(),
        store_id: store_id.clone(),
        affected_counts: request.declarations.iter().map(|decl| (decl.name.clone(), 0)).collect(),
        coverage: coverage.clone(),
        issued_at: issued_at.clone(),
        prior_request_id: None,
    };
    TenantReceipt::screen(&prospective, request.tenant, &ConfiguredSigner(signer.clone()))?;
    let tenant_digest = audit::query_digest(request.audit_key, &format!("contextful.erasure.tenant\n{}", request.tenant));
    let prior_request_id = prior_request(request.audit_dir, &tenant_digest)?;
    let select = |rows: &RetainedRows| -> Result<(ErasureSelection, String)> {
        Ok((select_tenant(request.declarations, rows, &columns, request.tenant)?, tenant_digest.clone()))
    };
    let guard = || Ok(());
    let admit_letters = |_: &[&str]| Ok(());
    let erased = publish(store, &Plan { declarations: request.declarations, signer, audit_dir: request.audit_dir, audit_key: request.audit_key, clock: request.clock, guard: &guard, admit_letters: &admit_letters, select: &select, allow_empty: true })?;
    // A tenant holding no row publishes no rewrite; its receipt counts zero in every declared table.
    let affected_counts = if erased.transaction_id.is_empty() { prospective.affected_counts } else { erased.affected_counts.clone() };
    let claim = ReceiptClaim {
        request_id,
        transaction_id: erased.transaction_id.clone(),
        store_id,
        affected_counts,
        coverage,
        issued_at,
        prior_request_id,
    };
    let receipt = TenantReceipt::sign(claim, request.tenant, &ConfiguredSigner(signer.clone()))?;
    receipt.check_fresh(&issued_receipts(request.audit_dir)?)?;
    let log = AuditLog::anchor(request.audit_dir, ConfiguredSigner(signer.clone())).map_err(|_| incomplete("the canonical audit chain cannot open under the purge signer"))?;
    log.append(json!({"operation":RECEIPT_OPERATION,"request_id":receipt.request_id,"transaction_id":receipt.transaction_id,"receipt_sha256":receipt.digest(),"tenant_digest":tenant_digest}))
        .map_err(|_| incomplete("the receipt audit entry did not persist"))?;
    log.export().map_err(|_| incomplete("the receipt audit evidence did not sign"))?;
    Ok(Purged { store: erased.store, receipt })
}

/// The `request_id` of the latest receipt entry the audit chain records for a keyed tenant digest.
fn prior_request(audit_dir: &Path, tenant_digest: &str) -> Result<Option<String>> {
    let entries = match audit::entries(audit_dir) {
        Ok(entries) => entries,
        Err(_) if !audit_dir.exists() => return Ok(None),
        Err(_) => return Err(incomplete("the audit chain holding issued receipts is unreadable")),
    };
    Ok(entries.iter().rev()
        .filter(|entry| entry.attributes.get("operation").and_then(serde_json::Value::as_str) == Some(RECEIPT_OPERATION))
        .filter(|entry| entry.attributes.get("tenant_digest").and_then(serde_json::Value::as_str) == Some(tenant_digest))
        .find_map(|entry| entry.attributes.get("request_id").and_then(serde_json::Value::as_str).map(str::to_string)))
}

/// The digest of every receipt the audit chain records, oldest first.
pub fn issued_receipts(audit_dir: &Path) -> Result<Vec<String>> {
    let entries = match audit::entries(audit_dir) {
        Ok(entries) => entries,
        Err(_) if !audit_dir.exists() => return Ok(Vec::new()),
        Err(_) => return Err(incomplete("the audit chain holding issued receipts is unreadable")),
    };
    Ok(entries.iter().filter(|entry| entry.attributes.get("operation").and_then(serde_json::Value::as_str) == Some(RECEIPT_OPERATION))
        .filter_map(|entry| entry.attributes.get("receipt_sha256").and_then(serde_json::Value::as_str).map(str::to_string)).collect())
}

/// One signed publication: the selection a caller supplies, staged, signed, committed and
/// collected under the canonical frontier.
struct Plan<'a> {
    declarations: &'a [TableDecl],
    signer: &'a Arc<dyn SigningPort + Send + Sync>,
    audit_dir: &'a Path,
    audit_key: &'a [u8],
    clock: &'a dyn Clock,
    guard: &'a dyn Fn() -> Result<()>,
    /// Admits the dead-letter tables beside declared tables that the publication rewrites
    /// with its selection (`disclosure.erase.dead-letters`).
    admit_letters: &'a dyn Fn(&[&str]) -> Result<()>,
    select: &'a dyn Fn(&RetainedRows) -> Result<(ErasureSelection, String)>,
    /// An empty selection returns an unpublished outcome with an empty transaction id,
    /// before any staging and whatever frontier the store already holds.
    allow_empty: bool,
}

fn publish(store: &Store, plan: &Plan<'_>) -> Result<Erased> {
    let signer = plan.signer;
    let key = SignerKey::of(signer.as_ref());
    crate::erasure_frontier::check_signer(store, plan.audit_dir, &key)?;
    let configured = store.clone();
    configured.with_frontier(|bound| {
        (plan.guard)()?;
        if plan.audit_key != bound.canonical_audit_key()? { return Err(incomplete("the supplied audit key disagrees with the canonical project key")); }
        let mut batches: BTreeMap<String, Vec<(PathBuf, Vec<RecordBatch>)>> = BTreeMap::new();
        let mut rows = RetainedRows::new();
        let mut directories = BTreeMap::new();
        // The dead-letter table beside each declared table, when the store holds one the
        // declarations do not, joins the publication (`disclosure.erase.dead-letters`).
        let letters: Vec<String> = bound.tables()?.into_iter()
            .filter(|name| plan.declarations.iter().all(|decl| decl.name != *name)
                && plan.declarations.iter().any(|decl| contextful_core::run::ports::dead_letter_table(&decl.name) == *name))
            .collect();
        (plan.admit_letters)(&letters.iter().map(String::as_str).collect::<Vec<_>>())?;
        let declarations: Vec<TableDecl> = plan.declarations.iter().cloned().chain(letters.iter().map(TableDecl::named)).collect();
        for decl in &declarations {
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
        let (mut selected, subject_hash) = (plan.select)(&rows)?;
        select_dead_letters(&mut selected, &rows, &letters)?;
        if plan.allow_empty && selected.affected_tables().next().is_none() {
            return Ok(Erased { store:configured.clone(), transaction_id:String::new(), subject_hash, affected_counts:BTreeMap::new() });
        }
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
        if targets.is_empty() {
            std::fs::remove_dir_all(&staging).at(&staging)?;
            return Err(unsupported("erasure selects no retained row versions"));
        }
        let mut hashes = BTreeMap::new();
        let mut retired = BTreeMap::new();
        let mut inventories = BTreeMap::new();
        let mut counts = BTreeMap::new();
        let store_id = crate::project::store_id(bound.root()).map_err(|_| incomplete("the erasure store identity is unavailable"))?;
        for name in &targets {
            let decl = declarations.iter().find(|decl| decl.name == *name).ok_or_else(|| unsupported("a selected frontier table has no declaration"))?;
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
        (plan.guard)()?;
        let log = AuditLog::anchor(plan.audit_dir, ConfiguredSigner(signer.clone())).map_err(|_| incomplete("the canonical audit chain cannot open under the erasure signer"))?;
        let entry = log.append(json!({"operation":"erasure","store_id":store_id,"transaction_id":transaction_id,"subject_hash":subject_hash,"affected_counts":counts,"replacement_hashes":hashes,"retired_directories":retired,"executed_at":plan.clock.now().to_rfc3339()}))
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
        (plan.guard)()?;
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

/// The name a rewritten snapshot's one part takes until every retained snapshot is read.
const ERASURE_PART: &str = ".erasure-part.parquet";

fn rebuild_snapshots(store: &Store, decl: &TableDecl, directory: &Path) -> Result<()> {
    let snapshots = directory.join(SNAPSHOTS_DIR);
    // Every retained snapshot reads its rows, a part carried by reference at its origin,
    // and writes them whole under a staged name before any part file leaves, since another
    // snapshot may still name the file (`store.fold.carry-by-reference`).
    let target = crate::parquet_io::arrow_schema(&store.schema(&decl.name)?);
    let mut rewritten = Vec::new();
    for path in files(directory)? {
        if path.file_name().is_none_or(|name| name != MANIFEST_FILE) { continue; }
        if path.parent().and_then(Path::parent) != Some(snapshots.as_path()) { continue; }
        let bytes = store.metadata().read(&path)?;
        let mut manifest: SnapshotManifest = serde_json::from_slice(&bytes).map_err(|_| incomplete("a retained snapshot manifest does not decode"))?;
        if manifest.table != decl.name { return Err(incomplete("a retained snapshot names another table")); }
        let snapshot = path.parent().ok_or_else(|| incomplete("a retained snapshot has no directory"))?.to_path_buf();
        let holder = |part: &contextful_core::store::lay_out::PartEntry| -> Result<PathBuf> {
            Ok(snapshots.join(part.holder(&manifest.snapshot_id).to_string()).join(relative_path(&part.name)?))
        };
        // A carried part keeps the schema it was written under, so each batch conforms to
        // the merged schema before the parts join (`store.reconcile.fold-never-narrows`).
        let mut batches = Vec::new();
        for part in &manifest.parts {
            for batch in store.read_parquet(&holder(part)?)? { batches.push(crate::parquet_io::conform(&batch, &target)?); }
        }
        let rows = concat_batches(&target, &batches).map_err(|_| incomplete("a retained snapshot has incompatible parts"))?;
        store.write_parquet(&snapshot.join(ERASURE_PART), &rows)?;
        for index in &manifest.indexes {
            if matches!(index, IndexEntry::Unrecognized(_)) { return Err(unsupported("an unrecognized sidecar has no erasure rewrite adapter")); }
            let relative = index.path().ok_or_else(|| unsupported("a sidecar names no physical directory"))?;
            let path = snapshot.join(relative_path(relative)?);
            if path.exists() { std::fs::remove_dir_all(&path).at(&path)?; }
        }
        manifest.indexes = decl.indexes().iter().map(|index| match index.kind {
            IndexKind::Vector => crate::vector::build(&snapshot, &manifest.snapshot_id, &rows, decl, index, &store.sealing()).map(IndexEntry::Vector),
            IndexKind::Fulltext => crate::fulltext::build(&snapshot, &manifest.snapshot_id, &rows, decl, index, &store.sealing()).map(IndexEntry::Fulltext),
        }).collect::<Result<_>>()?;
        manifest.row_count = rows.num_rows() as u64;
        rewritten.push((path, snapshot, manifest));
    }
    // Then every other part file leaves, with the sidecar segments a collected snapshot's
    // directory still held: no manifest names either once each snapshot holds its rows whole.
    fn clear(directory: &Path, root: bool) -> Result<()> {
        for entry in std::fs::read_dir(directory).at(directory)? {
            let entry = entry.at(directory)?;
            let path = entry.path();
            if entry.file_type().at(&path)?.is_dir() {
                if !root && path.file_name().is_some_and(|n| n == contextful_core::store::index::INDEXES_DIR) && !directory.join(MANIFEST_FILE).exists() {
                    std::fs::remove_dir_all(&path).at(&path)?;
                    continue;
                }
                clear(&path, false)?;
                if std::fs::read_dir(&path).at(&path)?.next().is_none() { std::fs::remove_dir(&path).at(&path)?; }
            } else if path.extension().is_some_and(|e| e == "parquet") && path.file_name().is_some_and(|n| n != ERASURE_PART) {
                std::fs::remove_file(&path).at(&path)?;
            }
        }
        Ok(())
    }
    if snapshots.is_dir() { clear(&snapshots, true)?; }
    // Each snapshot then holds its one row-independent part.
    for (path, snapshot, mut manifest) in rewritten {
        let name = contextful_core::store::lay_out::part_name(0);
        std::fs::rename(snapshot.join(ERASURE_PART), snapshot.join(&name)).at(snapshot.join(&name))?;
        manifest.parts = vec![contextful_core::store::lay_out::PartEntry {
            rows: Some(manifest.row_count),
            ..contextful_core::store::lay_out::PartEntry::new(name, store.sealing().key_version())
        }];
        manifest.format_version = contextful_core::store::lay_out::FormatVersion::of_snapshot(&manifest);
        store.metadata().write(&path, &serde_json::to_vec(&manifest).map_err(|_| incomplete("the rewritten snapshot manifest does not encode"))?)?;
    }
    Ok(())
}

fn empty_part(store: &Store, path: &Path) -> Result<RecordBatch> {
    let columns = store.parquet_schema(path)?;
    let schema = contextful_core::store::reconcile::Schema { columns, ..Default::default() };
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
