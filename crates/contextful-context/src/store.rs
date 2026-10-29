//! A project's store root on a filesystem: its configuration, table directories,
//! schemas, committed runs, and the pointer chain.

use crate::error::{ContextError, IoPath, Result};
use crate::encrypt::{bind_key_source, MetadataFiles, ProjectEncryption};
use crate::vector::Sealing;
use contextful_core::store::declare::TableDecl;
use contextful_core::store::lay_out::{
    is_path_segment, store_root, FormatVersion, Pointer, CONFIG_FILE, RunManifest, SkippedRuns, SnapshotId, SnapshotManifest, MANIFEST_FILE, POINTER_FILE, SCHEMA_FILE, SKIPPED_RUNS_FILE,
};
use contextful_core::store::reconcile::Schema;
use contextful_core::store::resolve::TableState;
use contextful_core::store::commit_log;
use contextful_core::store::reserve::COMMIT_LOCK_WAIT_SECS;
use contextful_core::store::StoreError;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::BTreeSet;
use contextful_fs::tmp_sibling;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use contextful_core::store::object::{PartHold, PartSource};
pub use contextful_core::store::config::{ConnectorPolicy, EncryptionConfig, NodeConfig, ReplicaConfig, StoreConfig};

/// An opened store root.
#[derive(Debug, Clone)]
pub struct Store {
    root: PathBuf,
    project: crate::project::Project,
    config_node_id: Option<String>,
    replica_of: Option<String>,
    require_connector_pin: bool,
    encryption: Option<Arc<ProjectEncryption>>,
    writer: crate::redaction::Writer,
    frontier: Option<FrontierLease>,
    schema_cache: Option<Arc<SchemaCache>>,
    /// Stored rows the vintage comparison decoded, shared across clones.
    vintage_rows: Arc<std::sync::atomic::AtomicU64>,
    #[cfg(feature = "read")]
    pub(crate) erasure_verifier: Option<Arc<crate::erasure_frontier::Verifier>>,
    /// Where a read fetches a part the store does not hold (`store.pull.lazy-parts`).
    part_source: Option<Arc<dyn PartSource>>,
}

/// The part source a process registered for each store root, read by [`Store::open`] so every
/// store a command opens after its pull fetches through it (`store.pull.lazy-parts`).
static PART_SOURCES: std::sync::Mutex<Vec<(PathBuf, Arc<dyn PartSource>)>> = std::sync::Mutex::new(Vec::new());

/// Register `source` for the store at `root`, replacing any earlier one.
pub fn register_part_source(root: &Path, source: Arc<dyn PartSource>) {
    let mut sources = PART_SOURCES.lock().unwrap_or_else(|e| e.into_inner());
    sources.retain(|(r, _)| r != root);
    sources.push((root.to_path_buf(), source));
}

/// Whether this process registered a part source for the store at `root`.
pub fn part_source_registered(root: &Path) -> bool {
    registered_part_source(root).is_some()
}

fn registered_part_source(root: &Path) -> Option<Arc<dyn PartSource>> {
    PART_SOURCES.lock().unwrap_or_else(|e| e.into_inner()).iter().find(|(r, _)| r == root).map(|(_, s)| s.clone())
}

/// The `schema.json` versions a long-lived writer holds parsed, keyed by table: a read
/// whose file version still matches answers from memory (`run.land.session-schema`).
#[derive(Debug, Default)]
pub(crate) struct SchemaCache {
    held: std::sync::Mutex<std::collections::HashMap<String, (FileVersion, Schema)>>,
    reads: std::sync::atomic::AtomicUsize,
}

/// A file's version as one stat reports it: length, modification instant and file
/// identity. A replace through [`replace_file`] renames a new file into place, so it moves
/// the identity even when the length and instant repeat.
#[derive(Debug, Clone, PartialEq, Eq)]
struct FileVersion {
    len: u64,
    modified: Option<std::time::SystemTime>,
    identity: u64,
}

fn file_version(path: &Path) -> Result<Option<FileVersion>> {
    let meta = match fs::metadata(path) {
        Ok(meta) => meta,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(ContextError::Io { path: path.to_path_buf(), source: e }),
    };
    #[cfg(unix)]
    let identity = std::os::unix::fs::MetadataExt::ino(&meta);
    #[cfg(not(unix))]
    let identity = 0;
    Ok(Some(FileVersion { len: meta.len(), modified: meta.modified().ok(), identity }))
}

/// One project publication fence, acquired before table/schema/run locks. Clones keep
/// the same kernel lease; no caller constructs or transfers it to another store.
#[derive(Debug, Clone)]
pub struct FrontierLease {
    root: PathBuf,
    held: Arc<FileLock>,
}

impl Store {
    /// Open the store of `project` under `project_dir`, resolving its configuration at
    /// startup. A declared key source whose binding is absent refuses with no
    /// cleartext fallback (`store.encrypt.key-unbound`).
    pub fn open(project_dir: &Path, project: &str) -> Result<Store> {
        crate::project::check_name(project)?;
        let root = project_dir.join(store_root(project));
        let config_path = root.join(CONFIG_FILE);
        let config: StoreConfig = match fs::read_to_string(&config_path) {
            Ok(text) => toml::from_str(&text)
                .map_err(|e| ContextError::Invalid(format!("{}: {e}", config_path.display())))?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => StoreConfig::default(),
            Err(e) => return Err(ContextError::Io { path: config_path, source: e }),
        };
        let encryption = config.encryption.as_ref().map(|enc| bind_key_source(&enc.key_source).map(Arc::new)).transpose()?;
        Ok(Store {
            writer: crate::redaction::Writer::open(project_dir)?,
            project: crate::project::Project { dir:project_dir.to_path_buf(), name:project.into() },
            config_node_id: config.node.and_then(|n| n.id),
            replica_of: config.replica.map(|r| r.of),
            require_connector_pin: config.connector.is_some_and(|c| c.require_pin),
            encryption,
            frontier: None,
            schema_cache: None,
            vintage_rows: Arc::default(),
            #[cfg(feature = "read")]
            erasure_verifier: None,
            part_source: registered_part_source(&root),
            root,
        })
    }

    /// This store, its reads fetching each absent part through `source` (`store.pull.lazy-parts`).
    pub fn with_part_source(mut self, source: Arc<dyn PartSource>) -> Store {
        self.part_source = Some(source);
        self
    }

    /// [`Store::hold_parts`] for a fold's inputs, refused when they exceed the part cache
    /// (`store.fold.cached-inputs`).
    pub fn hold_inputs(&self, files: &[String]) -> Result<Option<PartHold>> {
        match &self.part_source {
            Some(source) => Ok(Some(source.hold_inputs(files)?)),
            None => Ok(None),
        }
    }

    /// Whether reads fetch each absent part through a part source (`store.pull.lazy-parts`).
    pub fn fetches_parts(&self) -> bool {
        self.part_source.is_some()
    }

    /// Bring each of `files`, relative to the root, home and hold it while the hold lives; no
    /// hold where the store fetches no part.
    pub fn hold_parts(&self, files: &[String]) -> Result<Option<PartHold>> {
        match &self.part_source {
            Some(source) => Ok(Some(source.hold(files)?)),
            None => Ok(None),
        }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Additional writer declarations add authority to the canonical project set.
    pub fn open_declared(project_dir: &Path, project: &str, declaration: &Path) -> Result<Store> {
        let mut store = Self::open(project_dir, project)?;
        store.writer = crate::redaction::Writer::open_declared(project_dir, Some(declaration))?;
        Ok(store)
    }

    pub(crate) fn rewrite_batch(&self, table: &str, batch: &crate::land::Batch) -> Result<crate::land::Batch> {
        self.writer.batch(table, batch)
    }

    /// Canonical source-plan admission precedes source execution and verbatim recording.
    pub fn validate_writer_plan(&self, plan: &contextful_core::run::plan::Plan, normalize: Option<contextful_core::pipeline::normalize::Normalize>) -> Result<()> {
        self.writer.validate_plan(plan, normalize)
    }

    /// Recorded effects require canonical removal admission before execution.
    pub fn validate_writer_recording(&self, table: &str, normalize: Option<contextful_core::pipeline::normalize::Normalize>) -> Result<()> {
        self.writer.validate_recording(table, normalize)
    }

    pub fn recording_identity(&self, table: &str, normalize: Option<contextful_core::pipeline::normalize::Normalize>) -> Result<Option<String>> {
        self.writer.recording_identity(table, normalize)
    }
    pub fn effect_recording_identity(&self, table: &str, normalize: Option<contextful_core::pipeline::normalize::Normalize>) -> Result<String> {
        self.writer.effect_recording_identity(table, normalize)
    }

    /// Source runs admit canonical prepared recording; body-effect recording keeps its refusal.
    pub fn validate_source_plan(&self, plan: &contextful_core::run::plan::Plan, normalize: Option<contextful_core::pipeline::normalize::Normalize>) -> Result<()> {
        self.writer.validate_source_plan(plan, normalize)
    }
    pub fn validate_recording_clock(&self, table: &str, columns: &std::collections::BTreeSet<String>, normalize: Option<contextful_core::pipeline::normalize::Normalize>) -> Result<()> {
        self.writer.validate_recording_clock(table, columns, normalize)
    }

    pub fn validate_recorded_control(&self, table: &str, pull: &contextful_core::run::ports::Pull) -> Result<()> {
        self.writer.validate_recorded_control(table, pull)
    }

    pub fn prepare_recording(&self, table: &str, batch: &crate::land::Batch, normalize: Option<contextful_core::pipeline::normalize::Normalize>, load_id: &str) -> Result<crate::PreparedRecording> {
        self.writer.prepare_recording(table, batch, normalize, load_id)
    }

    pub fn admit_recording(&self, table: &str, bytes: &[u8], normalize: Option<contextful_core::pipeline::normalize::Normalize>) -> Result<crate::PreparedRecording> {
        self.writer.admit_recording(table, bytes, normalize)
    }

    /// Body records bind their stable owner and row/effect key inside canonical admission.
    pub fn prepare_effect_recording(&self, table: &str, batch: &crate::land::Batch, normalize: Option<contextful_core::pipeline::normalize::Normalize>, load_id: &str, scope: &contextful_core::run::effect::EffectScope) -> Result<crate::PreparedRecording> {
        self.writer.prepare_effect_recording(table, batch, normalize, load_id, scope)
    }

    pub fn admit_effect_recording(&self, table: &str, bytes: &[u8], normalize: Option<contextful_core::pipeline::normalize::Normalize>, scope: &contextful_core::run::effect::EffectScope) -> Result<crate::PreparedRecording> {
        self.writer.admit_effect_recording(table, bytes, normalize, scope)
    }

    pub(crate) fn rewrite_group(&self, group: contextful_core::pipeline::normalize::NormalizedGroup) -> Result<std::collections::BTreeMap<String, Vec<contextful_core::run::ports::Row>>> {
        self.writer.group(group)
    }

    pub(crate) fn canonical_audit_key(&self) -> Result<[u8; 32]> {
        crate::project::existing_audit_key(self, &self.project)
    }

    /// Bind the operator's audit-chain location and trusted issuer/node keys. A
    /// publication never supplies its own verifier trust.
    #[cfg(feature = "read")]
    pub fn with_erasure_verifier(&self, audit_dir: PathBuf, keys: Vec<contextful_policy::issue::SignerKey>) -> Result<Store> {
        if keys.is_empty() {
            return Err(contextful_core::disclosure::erase::ErasureError::ErasureTransactionIncomplete("the erasure verifier has no trusted key".into()).into());
        }
        Ok(Store { erasure_verifier: Some(Arc::new(crate::erasure_frontier::Verifier { audit_dir, keys, source:None })), ..self.clone() })
    }

    #[cfg(feature = "read")]
    pub fn with_erasure_key_source(&self, audit_dir: PathBuf, source: Arc<dyn contextful_policy::keyset::KeySource>) -> Result<Store> {
        let keys = source.keys().map_err(|error| ContextError::Invalid(error.to_string()))?;
        if keys.is_empty() { return Err(ContextError::Invalid("the erasure key source has no trusted key".into())); }
        Ok(Store { erasure_verifier:Some(Arc::new(crate::erasure_frontier::Verifier { audit_dir, keys:Vec::new(), source:Some(source) })), ..self.clone() })
    }

    /// Serialize source-frontier mutation with admitted source publication.
    pub fn lock_frontier(&self) -> Result<FrontierLease> {
        self.lock_frontier_within(std::time::Duration::from_secs(LOCK_WAIT_SECS))
    }

    pub(crate) fn lock_frontier_within(&self, wait: std::time::Duration) -> Result<FrontierLease> {
        if let Some(lease) = self.frontier_lease() {
            if lease.held.names_file()? { return Ok(lease); }
            return Err(ContextError::Invalid("the source frontier lease no longer names its lock".into()));
        }
        fs::create_dir_all(&self.root).at(&self.root)?;
        let root = fs::canonicalize(&self.root).at(&self.root)?;
        let held = FileLock::acquire(&root.join(".frontier.lock"), wait)?;
        Ok(FrontierLease { root, held: Arc::new(held) })
    }

    /// Run a mutation under the canonical frontier-before-table lock order. Nested
    /// adapter calls use the same lease rather than acquiring the kernel lock again.
    pub fn with_frontier<T>(&self, run: impl FnOnce(&Store) -> Result<T>) -> Result<T> {
        let store = self.frontier_store()?;
        run(&store)
    }

    pub(crate) fn frontier_store(&self) -> Result<Store> {
        let lease = self.lock_frontier()?;
        self.under_frontier(&lease)
    }

    pub(crate) fn under_frontier(&self, lease: &FrontierLease) -> Result<Store> {
        if fs::canonicalize(&self.root).at(&self.root)? != lease.root || !lease.held.names_file()? {
            return Err(ContextError::Invalid("the source frontier lease belongs to another store".into()));
        }
        Ok(Store { frontier: Some(lease.clone()), ..self.clone() })
    }

    pub(crate) fn frontier_lease(&self) -> Option<FrontierLease> { self.frontier.clone() }

    /// Whether this opened store holds a bound at-rest cipher.
    pub fn encrypted(&self) -> bool { self.encryption.is_some() }

    /// The opened store's opaque cipher binding for persistent adapters.
    pub fn file_cipher(&self) -> Option<Arc<dyn contextful_core::store::encrypt::FileCipher>> {
        self.encryption.as_ref().map(|keys| keys.clone() as Arc<dyn contextful_core::store::encrypt::FileCipher>)
    }

    /// How this store's sidecar files sit on disk.
    pub fn sealing(&self) -> Sealing<'_> {
        match &self.encryption {
            Some(keys) => Sealing::Sealed(keys.files()),
            None => Sealing::Plaintext,
        }
    }

    pub fn metadata_files(&self) -> MetadataFiles<'_> {
        match &self.encryption {
            Some(keys) => MetadataFiles::sealed(keys.files()),
            None => MetadataFiles::plaintext(),
        }
    }

    pub(crate) fn metadata(&self) -> MetadataFiles<'_> { self.metadata_files() }

    /// Decode a synced metadata object's canonical bytes under this store's key.
    pub fn open_metadata_bytes(&self, path: &Path, bytes: &[u8]) -> Result<Vec<u8>> {
        self.metadata_files().open_bytes(path, bytes)
    }

    /// Seal canonical metadata bytes before writing a synced object.
    pub fn seal_metadata_bytes(&self, path: &Path, bytes: &[u8]) -> Result<Vec<u8>> {
        self.metadata_files().seal_bytes(path, bytes)
    }

    pub(crate) fn parquet_key(&self) -> Option<&[u8]> {
        self.encryption.as_ref().map(|keys| keys.parquet_key().as_slice())
    }

    pub(crate) fn write_parquet(&self, path: &Path, batch: &arrow_array::RecordBatch) -> Result<()> {
        crate::parquet_io::write_with_key(path, batch, self.parquet_key())
    }

    /// [`Store::write_parquet`] with a bloom filter on each of `bloom` (`store.index.bloom-parts`).
    pub(crate) fn write_parquet_blooming(&self, path: &Path, batch: &arrow_array::RecordBatch, bloom: &[String]) -> Result<()> {
        crate::parquet_io::write_with_key_blooming(path, batch, self.parquet_key(), bloom)
    }

    pub(crate) fn read_parquet(&self, path: &Path) -> Result<Vec<arrow_array::RecordBatch>> {
        crate::parquet_io::read_with_key(path, self.parquet_key())
    }

    pub(crate) fn parquet_row_count(&self, path: &Path) -> Result<u64> {
        crate::parquet_io::row_count_with_key(path, self.parquet_key())
    }

    /// A reader over one store part, under this store's key, for a caller choosing its own
    /// row groups and projection.
    pub fn parquet_builder(&self, path: &Path) -> Result<parquet::arrow::arrow_reader::ParquetRecordBatchReaderBuilder<std::fs::File>> {
        crate::parquet_io::builder_with_key(path, self.parquet_key())
    }

    pub(crate) fn each_parquet_batch(&self, path: &Path, each: &mut dyn FnMut(arrow_array::RecordBatch) -> Result<()>) -> Result<()> {
        crate::parquet_io::each_batch_with_key(path, self.parquet_key(), each)
    }

    /// A store part's record batches, decoded one at a time, projected to `columns` where given.
    pub(crate) fn parquet_batches(&self, path: &Path, columns: Option<&[String]>) -> Result<impl Iterator<Item = Result<arrow_array::RecordBatch>>> {
        crate::parquet_io::batches_with_key(path, self.parquet_key(), columns)
    }

    /// A store part written batch by batch under the project's key, its bloom filters sized
    /// for at most `rows` rows.
    pub(crate) fn parquet_writer(&self, path: &Path, schema: std::sync::Arc<arrow_schema::Schema>, bloom: &[String], rows: u64) -> Result<crate::parquet_io::StreamWriter> {
        crate::parquet_io::StreamWriter::create(path, schema, self.parquet_key(), bloom, rows)
    }

    pub(crate) fn parquet_columns(&self, path: &Path) -> Result<Vec<String>> {
        crate::parquet_io::columns_with_key(path, self.parquet_key())
    }

    pub(crate) fn parquet_schema(&self, path: &Path) -> Result<Vec<contextful_core::store::reconcile::Column>> {
        crate::parquet_io::schema_with_key(path, self.parquet_key())
    }

    /// The canonical store this store replicates, if it is a replica.
    pub fn replica_of(&self) -> Option<&str> {
        self.replica_of.as_deref()
    }

    /// Refuse a write verb on a replica, naming the canonical store (`store.replicate.write-refused`).
    pub fn check_writable(&self, verb: &str) -> Result<()> {
        match &self.replica_of {
            Some(canonical) => Err(StoreError::ReplicaWriteRefused(format!("`{verb}` writes, and this store is a read-only replica of `{canonical}`")).into()),
            None => Ok(()),
        }
    }

    /// `[connector] require_pin` from the store's configuration, the store-wide switch of
    /// `connector.package.pin-requirement`.
    pub fn requires_connector_pin(&self) -> bool {
        self.require_connector_pin
    }

    /// The file holding the landed body named `sha256` (`store.lay-out.landed-blob`).
    pub fn blob_path(&self, sha256: &str) -> PathBuf {
        self.root.join(BLOBS_DIR).join(sha256)
    }

    /// Land `bytes` whole as `blobs/<sha256>` through a rename (`store.lay-out.landed-blob`).
    /// A body already landed under its digest stays as it stands; a `sha256` other than the
    /// bytes' digest lands nothing.
    pub fn land_blob(&self, sha256: &str, bytes: &[u8]) -> Result<()> {
        let digest = etag(bytes);
        if digest != sha256 {
            return Err(ContextError::Invalid(format!("a body named `{sha256}` digests to `{digest}`")));
        }
        let path = self.blob_path(sha256);
        let dir = self.root.join(BLOBS_DIR);
        fs::create_dir_all(&dir).at(&dir)?;
        let sealed = match self.sealing() {
            Sealing::Plaintext => bytes.to_vec(),
            Sealing::Sealed(cipher) => cipher.seal(bytes).map_err(|e| ContextError::Invalid(format!("{}: sealing: {e}", path.display())))?,
        };
        contextful_fs::create_new(&path, &sealed).at(&path)?;
        Ok(())
    }

    /// The landed body named `sha256`; `None` when the store holds none.
    pub fn blob(&self, sha256: &str) -> Result<Option<Vec<u8>>> {
        let path = self.blob_path(sha256);
        match fs::read(&path) {
            Ok(bytes) => match self.sealing() {
                Sealing::Plaintext => Ok(Some(bytes)),
                Sealing::Sealed(cipher) => cipher.open(&bytes).map(Some).map_err(|e| ContextError::Invalid(format!("{}: opening: {e}", path.display()))),
            },
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(ContextError::Io { path, source: e }),
        }
    }

    /// `[node] id` from the store's configuration.
    pub fn configured_node_id(&self) -> Option<String> {
        self.config_node_id.clone()
    }

    /// `tables/<t>/`, for a table name of path-safe segments.
    pub fn table_dir(&self, table: &str) -> Result<PathBuf> {
        check_table_name(table)?;
        crate::erasure_frontier::table_dir(self, table)
    }

    /// Resolve a logical store-relative file through the current project frontier.
    /// Public file names retain the `tables/` layout across physical replacements.
    pub fn logical_path(&self, relative: &str) -> Result<PathBuf> {
        if relative.contains('\\') || relative.split('/').any(|p| p.is_empty() || p == "." || p == ".." || p.contains(':')) {
            return Err(ContextError::Invalid("a store file requires a normalized relative path".into()));
        }
        let frontier = crate::erasure_frontier::load(self)?;
        if let Some(rest) = relative.strip_prefix("tables/") {
            if let Some((table, replacement)) = frontier.as_ref().and_then(|f| {
                f.tables.iter().filter(|(name, _)| rest.strip_prefix(name.as_str()).is_some_and(|tail| tail.starts_with('/')))
                    .max_by_key(|(name, _)| name.len())
            }) {
                let tail = &rest[table.len() + 1..];
                return Ok(self.root.join(&replacement.directory).join(tail));
            }
        }
        Ok(self.root.join(relative))
    }

    /// The validated logical publication identity, including selected directories.
    /// Equal table bytes under another erasure transaction carry another stamp.
    pub fn frontier_stamp(&self) -> Result<Option<String>> {
        crate::erasure_frontier::load(self)?.map(|frontier| {
            let mut publications = std::collections::BTreeMap::new();
            for (table, replacement) in &frontier.tables {
                publications.insert(table.clone(), crate::erasure_frontier::inventory(&self.root.join(&replacement.directory))?);
            }
            serde_json::to_vec(&(frontier, publications)).map(|bytes| etag(&bytes)).map_err(|_| ContextError::Invalid("the validated erasure frontier does not encode".into()))
        }).transpose()
    }

    /// The table's merged schema. A table no `schema.json` declares refuses
    /// (`store.lay-out.unknown-table`).
    pub fn schema(&self, table: &str) -> Result<Schema> {
        self.try_schema(table)?.ok_or_else(|| {
            StoreError::StoreUnknownTable(format!("no table `{table}`: no `schema.json` in the store declares it")).into()
        })
    }

    /// This store holding each `schema.json` it parses until the file's version moves, for
    /// one long-lived writer (`run.land.session-schema`). Clones share the cache.
    pub fn with_schema_cache(&self) -> Store {
        Store { schema_cache: Some(Arc::new(SchemaCache::default())), ..self.clone() }
    }

    /// How many times this store's schema cache parsed a `schema.json`; zero without one.
    pub fn schema_reads(&self) -> usize {
        self.schema_cache.as_ref().map_or(0, |c| c.reads.load(std::sync::atomic::Ordering::Relaxed))
    }

    /// How many rows the vintage comparison of this store and its clones decoded, counted
    /// before any `_commit_seq` filter (`store.available-time.vintage-hash`).
    pub fn vintage_rows_read(&self) -> u64 {
        self.vintage_rows.load(std::sync::atomic::Ordering::Relaxed)
    }

    pub(crate) fn count_vintage_rows(&self, rows: usize) {
        self.vintage_rows.fetch_add(rows as u64, std::sync::atomic::Ordering::Relaxed);
    }

    /// The table's merged schema, or `None` before its first batch.
    pub fn try_schema(&self, table: &str) -> Result<Option<Schema>> {
        let path = self.table_dir(table)?.join(SCHEMA_FILE);
        let Some(cache) = &self.schema_cache else { return self.read_schema(table, &path) };
        // The version is taken before the read, so a replace racing it moves the version
        // and the next read parses again.
        let Some(version) = file_version(&path)? else { return Ok(None) };
        if let Some((held, schema)) = cache.held.lock().expect("the schema cache is not poisoned").get(table) {
            if *held == version {
                return Ok(Some(schema.clone()));
            }
        }
        let schema = self.read_schema(table, &path)?;
        if let Some(schema) = &schema {
            cache.reads.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            cache.held.lock().expect("the schema cache is not poisoned").insert(table.to_string(), (version, schema.clone()));
        }
        Ok(schema)
    }

    fn read_schema(&self, table: &str, path: &Path) -> Result<Option<Schema>> {
        let Some(bytes) = self.metadata().read_optional(path)? else { return Ok(None) };
        let schema = serde_json::from_slice::<Schema>(&bytes).map_err(|e| {
            StoreError::StoreManifestUnreadable(format!("table `{table}`: file `{}`: {e}", path.display()))
        })?;
        Ok(Some(schema))
    }

    /// Lock the table's schema for a read-merge-replace, so concurrent landings each merge
    /// into the other's result.
    pub fn lock_schema(&self, table: &str) -> Result<FileLock> {
        let dir = self.table_dir(table)?;
        fs::create_dir_all(&dir).at(&dir)?;
        FileLock::acquire(&dir.join(format!("{SCHEMA_FILE}.lock")), std::time::Duration::from_secs(LOCK_WAIT_SECS))
    }

    /// Take the table's commit lock, waiting up to the commit wait and then refusing with
    /// `StoreCommitLockTimeout` (`store.reserve.commit-wait`). A landing holds it to assign
    /// `_commit_seq` and again to write its manifest (`store.reserve.commit-order`).
    pub fn lock_commit(&self, table: &str) -> Result<FileLock> {
        let dir = self.table_dir(table)?;
        fs::create_dir_all(&dir).at(&dir)?;
        let path = dir.join(COMMIT_LOCK_FILE);
        FileLock::acquire_within(&path, std::time::Duration::from_secs(COMMIT_LOCK_WAIT_SECS))?.ok_or_else(|| {
            StoreError::StoreCommitLockTimeout(format!("table `{table}`: {} stayed held past the {COMMIT_LOCK_WAIT_SECS} s commit wait", path.display())).into()
        })
    }

    /// Reserve the table's next commit sequence value under the commit lock, which releases
    /// on return: the landing writes its parts holding only the returned ticket, and
    /// [`CommitTicket::publish`] orders its manifest after every lower value
    /// (`store.reserve.commit-order`).
    pub fn reserve_commit_seq(&self, table: &str) -> Result<CommitTicket> {
        let _commit = self.lock_commit(table)?;
        let seq = self.assign_commit_seq(table)?;
        let path = self.table_dir(table)?.join(format!("{COMMIT_TICKET_PREFIX}{seq:020}.lock"));
        let lock = FileLock::try_acquire(&path)?.ok_or_else(|| ContextError::Invalid(format!("table `{table}`: commit ticket {seq} is already held")))?;
        Ok(CommitTicket { table: table.to_string(), seq, _lock: lock })
    }

    /// Assign the table's next commit sequence value, one above the greatest of the node's
    /// counter, every run manifest on disk and the current snapshot's high-water mark, and
    /// record it before any part carries it, so a failed landing leaves a gap and never a
    /// repeat (`store.reserve.commit-seq`, `store.reserve.commit-seq-seed`). The counter
    /// is node-local and a push carries no dot file; the manifests travel with the rows.
    /// The caller holds [`Store::lock_commit`].
    ///
    /// The counter is written as [`CommitCounter`] JSON (`store.reserve.commit-marker`): a
    /// build that holds the commit lock across its part write parses it as a bare integer,
    /// fails, and so refuses the table instead of publishing out of ticket order.
    pub fn assign_commit_seq(&self, table: &str) -> Result<i64> {
        let data = self.table_dir(table)?.join("data");
        let path = data.join(COMMIT_SEQ_FILE);
        let counter = match self.metadata().read_optional(&path)? {
            Some(bytes) => CommitCounter::read(&bytes, &format!("table `{table}`: file `{}`", path.display()))?,
            None => 0,
        };
        let held = counter.max(self.recorded_commit_seq(table)?);
        let next = held.checked_add(1).ok_or_else(|| ContextError::Invalid(format!("table `{table}` exhausted its commit sequence")))?;
        fs::create_dir_all(&data).at(&data)?;
        let marked = CommitCounter { format_version: FormatVersion::CURRENT, commit_seq: next };
        self.metadata().replace(&path, &serde_json::to_vec(&marked).expect("a counter serializes"))?;
        Ok(next)
    }

    /// The highest `commit_seq` a table's manifests record: every run manifest on disk,
    /// readable or not, and the snapshot the pointer names. Zero when none records one.
    fn recorded_commit_seq(&self, table: &str) -> Result<i64> {
        #[derive(Deserialize)]
        struct Recorded {
            #[serde(default)]
            commit_seq: Option<i64>,
        }
        let read = |path: PathBuf| -> Result<i64> {
            let Some(bytes) = self.metadata().read_optional(&path)? else { return Ok(0) };
            let r: Recorded = serde_json::from_slice(&bytes).map_err(|e| {
                StoreError::StoreManifestUnreadable(format!("table `{table}`: file `{}`: {e}", path.display()))
            })?;
            Ok(r.commit_seq.unwrap_or(0))
        };
        let mut high = match self.pointer(table)? {
            Some((ptr, _)) => read(self.snapshot_dir(table, &ptr.snapshot_id)?.join(MANIFEST_FILE))?,
            None => 0,
        };
        for run_dir in sorted_dirs(&self.table_dir(table)?.join(contextful_core::store::lay_out::RUNS_DIR))? {
            for node_dir in sorted_dirs(&run_dir)? {
                high = high.max(read(node_dir.join(MANIFEST_FILE))?);
            }
        }
        Ok(high)
    }

    /// Replace `schema.json` with the merged schema (`store.lay-out.schema-file`).
    pub fn write_schema(&self, table: &str, schema: &Schema) -> Result<()> {
        let dir = self.table_dir(table)?;
        fs::create_dir_all(&dir).at(&dir)?;
        let text = serde_json::to_string_pretty(schema).expect("a schema serializes");
        if let Some(cache) = &self.schema_cache {
            cache.held.lock().expect("the schema cache is not poisoned").remove(table);
        }
        self.metadata().replace(&dir.join(SCHEMA_FILE), text.as_bytes())
    }

    /// Every table a `schema.json` declares, sorted.
    pub fn tables(&self) -> Result<Vec<String>> {
        let frontier = crate::erasure_frontier::load(self)?;
        let base = self.root.join("tables");
        let mut out = Vec::new();
        let mut stack = vec![base.clone()];
        while let Some(dir) = stack.pop() {
            let entries = match fs::read_dir(&dir) {
                Ok(e) => e,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
                Err(e) => return Err(ContextError::Io { path: dir, source: e }),
            };
            for entry in entries {
                let path = entry.at(&dir)?.path();
                if path.join(SCHEMA_FILE).is_file() {
                    let rel = path.strip_prefix(&base).expect("under the tables root");
                    out.push(rel.to_string_lossy().replace('\\', "/"));
                }
                // A table name carries `/`, so a table directory still holds tables below it.
                if path.is_dir() && path.file_name().is_some_and(|n| n != "data" && n != "requests") {
                    stack.push(path);
                }
            }
        }
        if let Some(frontier) = frontier { out.extend(frontier.tables.into_keys()); }
        out.sort();
        out.dedup();
        Ok(out)
    }

    /// Every committed run of a table: each `data/runs/<run>/<node>/` holding a
    /// `_manifest.json`. A directory without one is in flight and joins nothing
    /// (`store.lay-out.uncommitted-run`); a manifest that fails to parse, or whose
    /// `run_id`, `node_id` or `table` disagrees with its path, refuses the table
    /// (`store.lay-out.manifest-unreadable`).
    /// How many `<run>/<node>` directories of `table` hold a `_manifest.json`, from a
    /// listing alone: no manifest is read or parsed.
    pub fn manifest_count(&self, table: &str) -> Result<usize> {
        let runs_dir = self.table_dir(table)?.join(contextful_core::store::lay_out::RUNS_DIR);
        let mut count = 0;
        for run_dir in sorted_dirs(&runs_dir)? {
            for node_dir in sorted_dirs(&run_dir)? {
                if node_dir.join(MANIFEST_FILE).is_file() {
                    count += 1;
                }
            }
        }
        Ok(count)
    }

    pub fn committed_runs(&self, table: &str) -> Result<Vec<RunManifest>> {
        Ok(self.committed_runs_held(table)?.0)
    }

    /// Bring home each folded run manifest of `table` a lazy pull left in the bucket, those
    /// of `run_id` alone where given, through the part source (`store.pull.folded-runs-fetched`).
    /// Without a source the read refuses (`store.pull.folded-runs-unreachable`), and a manifest
    /// the source leaves absent refuses as collected (`store.pull.folded-run-collected`).
    fn fetch_skipped(&self, table: &str, run_id: Option<&str>) -> Result<()> {
        let path = self.root.join(SKIPPED_RUNS_FILE);
        let skipped: SkippedRuns = match fs::read(&path) {
            Ok(bytes) => serde_json::from_slice(&bytes).map_err(|e| StoreError::StoreManifestUnreadable(format!("file `{}`: {e}", path.display())))?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(e) => return Err(ContextError::Io { path, source: e }),
        };
        let absent: Vec<String> = skipped
            .of_table(table)
            .filter(|rel| run_id.is_none_or(|run| rel.split('/').rev().nth(2) == Some(run)))
            .filter(|rel| !self.root.join(rel.as_str()).exists())
            .cloned()
            .collect();
        let Some(first) = absent.first() else { return Ok(()) };
        let Some(source) = &self.part_source else {
            return Err(StoreError::SyncFoldedRunsUnfetched(format!(
                "table `{table}`: `{first}` and {} more folded run manifests stay in the bucket, and this store reads through no part source; open it where `[sync] lazy_parts` registers one",
                absent.len() - 1
            ))
            .into());
        };
        let _hold = source.hold(&absent)?;
        if let Some(gone) = absent.iter().find(|rel| !self.root.join(rel.as_str()).exists()) {
            return Err(StoreError::SyncPartCollected(format!("folded run manifest `{gone}` is gone from the bucket; the next pull stops naming it")).into());
        }
        Ok(())
    }

    /// The table's committed runs, and the lowest `commit_seq` among its grouped manifests
    /// still awaiting their group marker: a run at or above it is readable before a run
    /// below it, so a reader following commit order holds back from there
    /// (`read.subscribe.commit-order`).
    pub fn committed_runs_held(&self, table: &str) -> Result<(Vec<RunManifest>, Option<i64>)> {
        self.fetch_skipped(table, None)?;
        self.local_runs_held(table)
    }

    /// [`Store::committed_runs_held`] over the manifests on disk alone, which an unbounded read
    /// resolves against: every run a lazy pull skipped is folded into the pointer's snapshot.
    fn local_runs_held(&self, table: &str) -> Result<(Vec<RunManifest>, Option<i64>)> {
        let runs_dir = self.table_dir(table)?.join(contextful_core::store::lay_out::RUNS_DIR);
        let mut out = Vec::new();
        let mut held: Option<i64> = None;
        // One read of each (pipeline, node) commit log per call.
        let mut logs = CommitLogs::new();
        for run_dir in sorted_dirs(&runs_dir)? {
            for node_dir in sorted_dirs(&run_dir)? {
                out.extend(self.committed_at(table, &run_dir, &node_dir, &mut logs, &mut held)?);
            }
        }
        Ok((out, held))
    }

    /// The rows run `run_id` of `table` committed, read from its own `data/runs/<run_id>/`
    /// directory and its parts' footers alone; `None` while no node's manifest there reads as
    /// committed under the rules of [`Store::committed_runs`].
    pub fn committed_rows(&self, table: &str, run_id: &str) -> Result<Option<u64>> {
        if run_id.is_empty() || run_id.contains(['/', '\\']) || run_id == "." || run_id == ".." {
            return Err(ContextError::Invalid(format!("run id `{run_id}` names no run directory")));
        }
        self.fetch_skipped(table, Some(run_id))?;
        let run_dir = self.table_dir(table)?.join(contextful_core::store::lay_out::RUNS_DIR).join(run_id);
        let (mut logs, mut held) = (CommitLogs::new(), None);
        let mut rows: Option<u64> = None;
        for node_dir in sorted_dirs(&run_dir)? {
            let Some(m) = self.committed_at(table, &run_dir, &node_dir, &mut logs, &mut held)? else { continue };
            let mut total = rows.unwrap_or(0);
            for part in &m.parts {
                let path = self.logical_path(&format!("tables/{table}/{}/{}/{}/{}", contextful_core::store::lay_out::RUNS_DIR, m.run_id, m.node_id, part.name))?;
                total = total.saturating_add(self.parquet_row_count(&path)?);
            }
            rows = Some(total);
        }
        Ok(rows)
    }

    /// The manifest of `<run_dir>/<node_dir>` when it reads as committed: grouped manifests
    /// still awaiting their group marker lower `held` to their `commit_seq`.
    fn committed_at(&self, table: &str, run_dir: &Path, node_dir: &Path, logs: &mut CommitLogs, held: &mut Option<i64>) -> Result<Option<RunManifest>> {
        let path = node_dir.join(MANIFEST_FILE);
        let Some(bytes) = self.metadata().read_optional(&path)? else { return Ok(None) };
        let unreadable = |why: String| StoreError::StoreManifestUnreadable(format!("table `{table}`: file `{}`: {why}", path.display()));
        if let Some(v) = FormatVersion::declared(&bytes) {
            v.admit(&format!("table `{table}`: file `{}`", path.display()))?;
        }
        let m: RunManifest = serde_json::from_slice(&bytes).map_err(|e| unreadable(e.to_string()))?;
        let (run_seg, node_seg) = (file_name(run_dir), file_name(node_dir));
        if m.run_id != run_seg || m.node_id != node_seg || m.table != table {
            return Err(unreadable(format!(
                "names run `{}` on node `{}` of table `{}`, but sits at `{run_seg}/{node_seg}`",
                m.run_id, m.node_id, m.table
            ))
            .into());
        }
        if let Some(root) = &m.group_root {
            let marker = self.table_dir(root)?.join(contextful_core::store::lay_out::RUNS_DIR).join(&m.run_id).join(&m.node_id).join("_group.json");
            let bytes = match fs::read(&marker) {
                Ok(bytes) => bytes,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                    if let Some(seq) = m.commit_seq {
                        *held = Some(held.map_or(seq, |h| h.min(seq)));
                    }
                    return Ok(None);
                }
                Err(e) => return Err(ContextError::Io { path: marker, source: e }),
            };
            let tables: Vec<String> = serde_json::from_slice(&bytes).map_err(|e| unreadable(format!("group marker: {e}")))?;
            if !tables.iter().any(|name| name == table) {
                return Ok(None);
            }
        }
        // A run written under the commit-log protocol is readable once its node's log
        // records it under its fence; a manifest without the mark reads as committed.
        if let (true, Some(fence), Some(pipeline)) = (m.logged, m.fence, m.pipeline_id.as_deref()) {
            let key = (pipeline.to_string(), m.node_id.clone());
            if !logs.contains_key(&key) {
                let log = crate::commit_log::read(self, pipeline, &m.node_id)?;
                logs.insert(key.clone(), log);
            }
            if !commit_log::committed(&logs[&key], table, &m.run_id, fence) {
                return Ok(None);
            }
        }
        Ok(Some(m))
    }

    /// The pointer and the ETag of its bytes, or `None` before the first fold.
    pub fn pointer(&self, table: &str) -> Result<Option<(Pointer, String)>> {
        let path = self.table_dir(table)?.join(POINTER_FILE);
        let disk = match fs::read(&path) {
            Ok(b) => b,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(ContextError::Io { path, source: e }),
        };
        let bytes = self.metadata().open_bytes(&path, &disk)?;
        let p: Pointer = serde_json::from_slice(&bytes).map_err(|e| {
            StoreError::StoreManifestUnreadable(format!("table `{table}`: file `{}`: {e}", path.display()))
        })?;
        Ok(Some((p, etag(&disk))))
    }

    /// The pointer's ETag: the SHA-256 of its bytes, or `absent`.
    pub fn pointer_etag(&self, table: &str) -> Result<String> {
        let path = self.table_dir(table)?.join(POINTER_FILE);
        match fs::read(&path) {
            Ok(b) => Ok(etag(&b)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(ABSENT_ETAG.into()),
            Err(e) => Err(ContextError::Io { path, source: e }),
        }
    }

    pub fn snapshot_dir(&self, table: &str, id: &SnapshotId) -> Result<PathBuf> {
        Ok(self.table_dir(table)?.join(contextful_core::store::lay_out::SNAPSHOTS_DIR).join(id.to_string()))
    }

    /// The snapshot the pointer names and its `parent` chain, newest first, and whether
    /// the chain ends at a parent retention has collected. A reachable manifest that
    /// fails to parse refuses the table, as does one whose `parent` returns to a
    /// snapshot the walk already passed.
    pub fn chain(&self, table: &str) -> Result<(Vec<SnapshotManifest>, bool)> {
        let Some((ptr, _)) = self.pointer(table)? else { return Ok((Vec::new(), false)) };
        self.chain_from(table, ptr.snapshot_id)
    }

    /// The snapshot `start` and its `parent` chain, newest first, read as [`Store::chain`]
    /// reads the pointer's.
    pub fn chain_from(&self, table: &str, start: SnapshotId) -> Result<(Vec<SnapshotManifest>, bool)> {
        let mut chain = Vec::new();
        let mut seen = std::collections::BTreeSet::new();
        let mut next = Some(start);
        while let Some(id) = next {
            if !seen.insert(id.to_string()) {
                return Err(StoreError::StoreManifestUnreadable(format!(
                    "table `{table}`: snapshot {id} is its own ancestor; the parent chain does not terminate"
                ))
                .into());
            }
            let path = self.snapshot_dir(table, &id)?.join(MANIFEST_FILE);
            let bytes = match self.metadata().read_optional(&path)? {
                Some(b) => b,
                None if !chain.is_empty() => return Ok((chain, true)),
                None => return Err(ContextError::Invalid(format!("{}: missing snapshot manifest", path.display()))),
            };
            if let Some(v) = FormatVersion::declared(&bytes) {
                v.admit(&format!("table `{table}`: file `{}`", path.display()))?;
            }
            let m: SnapshotManifest = serde_json::from_slice(&bytes).map_err(|e| {
                StoreError::StoreManifestUnreadable(format!("table `{table}`: file `{}`: {e}", path.display()))
            })?;
            next = m.parent.clone();
            chain.push(m);
        }
        Ok((chain, false))
    }

    /// Everything an unbounded read or a fold resolves against, for one table: the run
    /// manifests on disk, without fetching any a lazy pull skipped.
    pub fn state(&self, decl: &TableDecl) -> Result<TableState> {
        let table = decl.name.as_str();
        let runs = self.local_runs_held(table)?.0;
        let (chain, parent_collected) = self.chain(table)?;
        let present: BTreeSet<String> = runs.iter().map(RunManifest::key).collect();
        let run_collected = chain.iter().flat_map(|s| &s.includes_runs).any(|r| !present.contains(r.as_str()));
        Ok(TableState {
            table: table.to_string(),
            write_mode: decl.write_mode(),
            runs,
            chain,
            history_collected: parent_collected || run_collected,
        })
    }

    /// [`Store::state`] for a read bounded by `as_of`: one resolving before the pointer's
    /// snapshot first fetches the folded runs a lazy pull skipped (`store.pull.folded-runs-fetched`).
    pub fn state_at(&self, decl: &TableDecl, as_of: Option<contextful_core::store::bound_time::Bound>) -> Result<TableState> {
        let state = self.state(decl)?;
        match as_of {
            Some(b) if state.chain.first().is_none_or(|s| !b.admits(s.created_at)) => {
                self.fetch_skipped(&decl.name, None)?;
                self.state(decl)
            }
            _ => Ok(state),
        }
    }
}

pub const ABSENT_ETAG: &str = "absent";

/// The lock a table's commits serialize on, beside `schema.json`.
const COMMIT_LOCK_FILE: &str = "commit.lock";

/// The last commit sequence value a table assigned, under `data/`. The leading `.` keeps
/// it node-local: a push uploads no dot file.
const COMMIT_SEQ_FILE: &str = ".commit_seq";

/// A table's node-local commit counter (`store.reserve.commit-marker`).
#[derive(Debug, Serialize, Deserialize)]
struct CommitCounter {
    format_version: FormatVersion,
    commit_seq: i64,
}

impl CommitCounter {
    /// The value `bytes` record: a bare integer, which a build locking across its part write
    /// leaves, or the JSON form, whose newer major refuses as `store.lay-out.format-newer`.
    fn read(bytes: &[u8], file: &str) -> Result<i64> {
        let unreadable = |why: String| ContextError::from(StoreError::StoreManifestUnreadable(format!("{file}: {why}")));
        let text = std::str::from_utf8(bytes).map_err(|e| unreadable(e.to_string()))?;
        if let Ok(bare) = text.trim().parse::<i64>() {
            return Ok(bare);
        }
        if let Some(v) = FormatVersion::declared(bytes) {
            v.admit(file)?;
        }
        let counter: CommitCounter = serde_json::from_str(text).map_err(|e| unreadable(e.to_string()))?;
        Ok(counter.commit_seq)
    }
}

/// How long a landing waits for a lock another landing holds before refusing.
pub const LOCK_WAIT_SECS: u64 = 30;

/// An exclusive lock: a lock file holding an advisory lock of the operating system,
/// the filesystem's stand-in for a conditional write. The kernel releases the lock when
/// its holder exits, so a crashed holder's file blocks nobody, a live holder is never
/// mistaken for a crashed one however long it works, and no waiter takes a lock beside
/// its holder. The holder removes the file on drop, and only while it still names the
/// file the holder locked.
#[derive(Debug)]
pub struct FileLock {
    path: PathBuf,
    file: fs::File,
}

impl FileLock {
    /// Whether the lock path still names this holder's open file.
    pub fn names_file(&self) -> Result<bool> {
        names_file(&self.path, &self.file)
    }

    /// Take the lock, or `None` while a live holder has it.
    pub fn try_acquire(path: &Path) -> Result<Option<FileLock>> {
        loop {
            let file = fs::OpenOptions::new().read(true).write(true).create(true).truncate(false).open(path).at(path)?;
            match file.try_lock() {
                Ok(()) => {}
                Err(fs::TryLockError::WouldBlock) => return Ok(None),
                Err(fs::TryLockError::Error(e)) => return Err(ContextError::Io { path: path.to_path_buf(), source: e }),
            }
            // A releasing holder removes the file between this open and this lock; the lock
            // then covers a file no path names, and the next open meets the current one.
            if names_file(path, &file)? {
                return Ok(Some(FileLock { path: path.to_path_buf(), file }));
            }
        }
    }

    /// Take the lock, waiting up to `wait` for a live holder to release it, or `None`
    /// while one still holds it. The caller decides what contention means.
    pub fn acquire_within(path: &Path, wait: std::time::Duration) -> Result<Option<FileLock>> {
        let start = std::time::Instant::now();
        loop {
            if let Some(lock) = FileLock::try_acquire(path)? {
                return Ok(Some(lock));
            }
            if start.elapsed() >= wait {
                return Ok(None);
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
    }

    /// Take the lock, waiting up to `wait`, and refuse when a live holder outlasts it.
    pub fn acquire(path: &Path, wait: std::time::Duration) -> Result<FileLock> {
        FileLock::acquire_within(path, wait)?
            .ok_or_else(|| ContextError::Invalid(format!("{} is held by another process", path.display())))
    }
}

impl Drop for FileLock {
    /// Remove the file while still holding its lock, so a waiter that opened it finds no
    /// path naming it and reopens; the lock releases when the handle closes.
    fn drop(&mut self) {
        if names_file(&self.path, &self.file).unwrap_or(false) {
            let _ = fs::remove_file(&self.path);
        }
    }
}

/// The prefix of a reserved commit sequence value's lock file beside `commit.lock`. Its
/// `.lock` suffix keeps it off every push.
const COMMIT_TICKET_PREFIX: &str = "commit-ticket-";

/// A commit sequence value a landing reserved and has not yet published: a locked file the
/// kernel releases when its holder exits, so a crashed landing's value reads as a gap.
#[derive(Debug)]
pub struct CommitTicket {
    table: String,
    seq: i64,
    _lock: FileLock,
}

impl CommitTicket {
    /// The reserved `_commit_seq` value.
    pub fn seq(&self) -> i64 {
        self.seq
    }

    /// Wait until every lower ticket of the table published or its holder died, then take the
    /// commit lock the manifest is written under, so readable runs hold a prefix of the
    /// sequence (`store.reserve.commit-order`). Past the commit wait it raises
    /// `StoreCommitLockTimeout` (`store.reserve.commit-wait`). The ticket releases when it
    /// drops, after the manifest.
    pub fn publish(&self, store: &Store) -> Result<FileLock> {
        let dir = store.table_dir(&self.table)?;
        let wait = std::time::Duration::from_secs(COMMIT_LOCK_WAIT_SECS);
        let started = std::time::Instant::now();
        loop {
            let mut held = None;
            for entry in fs::read_dir(&dir).at(&dir)? {
                let path = entry.at(&dir)?.path();
                let Some(seq) = path.file_name().and_then(|n| n.to_str()).and_then(|n| n.strip_prefix(COMMIT_TICKET_PREFIX)).and_then(|n| n.strip_suffix(".lock")).and_then(|n| n.parse::<i64>().ok()) else {
                    continue;
                };
                if seq >= self.seq {
                    continue;
                }
                // A lower ticket no live holder locks belongs to a landing that ended: taking it
                // removes the file, and its value stays a gap.
                if FileLock::try_acquire(&path)?.is_none() {
                    held = Some(path);
                    break;
                }
            }
            let Some(path) = held else {
                let remaining = wait.saturating_sub(started.elapsed());
                let lock_path = dir.join(COMMIT_LOCK_FILE);
                return FileLock::acquire_within(&lock_path, remaining)?.ok_or_else(|| {
                    StoreError::StoreCommitLockTimeout(format!("table `{}`: {} stayed held past the {COMMIT_LOCK_WAIT_SECS} s commit wait", self.table, lock_path.display())).into()
                });
            };
            if started.elapsed() >= wait {
                return Err(StoreError::StoreCommitLockTimeout(format!(
                    "table `{}`: commit sequence value {} waited past the {COMMIT_LOCK_WAIT_SECS} s commit wait for {} to publish",
                    self.table,
                    self.seq,
                    path.display()
                ))
                .into());
            }
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
    }
}

/// Whether `path` still names the file `file` opened.
fn names_file(path: &Path, file: &fs::File) -> Result<bool> {
    contextful_fs::names_file(path, file).at(path)
}

/// The store-root directory holding landed bodies (`store.lay-out.landed-blob`).
pub const BLOBS_DIR: &str = "blobs";

pub(crate) fn etag(bytes: &[u8]) -> String {
    hex(&Sha256::digest(bytes))
}

/// Lowercase hex of `bytes`.
pub(crate) fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// The commit logs one listing has read, per (pipeline, node).
type CommitLogs = std::collections::HashMap<(String, String), Vec<commit_log::CommitEntry>>;

fn file_name(p: &Path) -> String {
    p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()
}

/// Subdirectories of `dir`, sorted; a missing `dir` has none and a stray file is skipped.
pub(crate) fn sorted_dirs(dir: &Path) -> Result<Vec<PathBuf>> {
    let entries = match fs::read_dir(dir) {
        Ok(e) => e,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(ContextError::Io { path: dir.to_path_buf(), source: e }),
    };
    let mut out = Vec::new();
    for entry in entries {
        let path = entry.at(dir)?.path();
        if path.is_dir() {
            out.push(path);
        }
    }
    out.sort();
    Ok(out)
}

/// Replace `path` with `bytes` through a private temporary file and one rename, so a
/// reader meets the old file or the new one.
pub fn replace_file(path: &Path, bytes: &[u8]) -> Result<()> {
    let tmp = tmp_sibling(path);
    fs::write(&tmp, bytes).at(&tmp)?;
    fs::rename(&tmp, path).at(path)
}

/// Create `path` holding `bytes` only if nothing is there: the bytes land in a sibling
/// temporary file one exclusive create publishes, `false` where `path` exists.
pub(crate) fn create_new_file(path: &Path, bytes: &[u8]) -> Result<bool> {
    contextful_fs::create_new(path, bytes).at(path)
}

/// A table or project name: `/`-separated segments of `[A-Za-z0-9._-]`, none `.` or `..`.
fn check_segment_path(name: &str, what: &str) -> Result<()> {
    if name.split('/').all(is_path_segment) {
        Ok(())
    } else {
        Err(ContextError::Invalid(format!("{what} name `{name}` is not `/`-separated segments of [A-Za-z0-9._-]")))
    }
}

pub(crate) fn check_table_name(table: &str) -> Result<()> {
    check_segment_path(table, "table")?;
    check_table_layout(table)
}

/// Segments a table directory's own layout uses: its data and ledger trees, its files, and
/// the lock and temporary siblings written beside them (`store.lay-out.table-directory`).
const TABLE_LAYOUT_SEGMENTS: [&str; 4] = ["data", "requests", SCHEMA_FILE, POINTER_FILE];

/// Refuse a table name one of whose segments the table layout uses, so a nested table never
/// lands inside another table's tree and [`Store::tables`] lists every table it holds.
fn check_table_layout(table: &str) -> Result<()> {
    match table.split('/').find(|seg| TABLE_LAYOUT_SEGMENTS.contains(seg) || seg.starts_with('.') || seg.ends_with(".lock")) {
        Some(seg) => Err(ContextError::Invalid(format!(
            "table name `{table}` carries segment `{seg}`, which a table directory's own layout uses"
        ))),
        None => Ok(()),
    }
}
