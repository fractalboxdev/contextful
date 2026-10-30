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
use std::collections::{BTreeMap, BTreeSet};
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
    /// The store's `config.toml` declares `[sync]` (`store.fold.manifest-kept`).
    synced: bool,
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
    /// Each valid folded-runs index this store read, by `<table>/<snapshot id>`: a snapshot's
    /// manifest and segments never change, so one validation serves every later landing.
    indexes: std::sync::Mutex<std::collections::HashMap<String, Arc<BTreeMap<String, contextful_core::store::lay_out::FoldedRow>>>>,
    /// The commit sequence high-water this store last assigned on each table, with the
    /// counter's and pointer's versions as it left them (`store.fold.session-listing`).
    seqs: std::sync::Mutex<std::collections::HashMap<String, HeldSeq>>,
    /// The tables whose last assignment found the counter and pointer as this store left them.
    sole: std::sync::Mutex<BTreeSet<String>>,
    /// How many times this store listed a table's run directories.
    listings: std::sync::atomic::AtomicUsize,
}

/// One table's assigned high-water and the file versions it holds for: while neither the
/// counter nor the pointer moves, no other writer committed, pulled or folded since.
#[derive(Debug)]
struct HeldSeq {
    counter: Option<FileVersion>,
    pointer: Option<FileVersion>,
    high: i64,
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
            synced: config.sync.is_some(),
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

    /// The project this store belongs to.
    pub fn project_name(&self) -> &str {
        &self.project.name
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

    /// Whether the store's `config.toml` declares `[sync]`, so a collection keeps each
    /// superseded snapshot's `_manifest.json` (`store.fold.manifest-kept`).
    pub fn synced(&self) -> bool {
        self.synced
    }

    /// Whether a collection reduced snapshot `m` of `table` to its `_manifest.json`
    /// (`store.fold.manifest-kept`): the bucket's `_collected.json` stands beside it, or, on a
    /// store reading through no part source, a part it names is absent. A pull bringing the
    /// parts home answers `false` again.
    pub fn snapshot_collected(&self, table: &str, m: &SnapshotManifest) -> Result<bool> {
        if self.snapshot_dir(table, &m.snapshot_id)?.join(contextful_core::store::lay_out::COLLECTED_FILE).exists() {
            return Ok(true);
        }
        if self.part_source.is_some() {
            return Ok(false);
        }
        let table_dir = self.table_dir(table)?;
        Ok(m.part_files().iter().any(|p| !table_dir.join(p).exists()))
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

    /// How many times this store listed a table's run directories; zero without a schema cache.
    pub fn run_listings(&self) -> usize {
        self.schema_cache.as_ref().map_or(0, |c| c.listings.load(std::sync::atomic::Ordering::Relaxed))
    }

    /// Whether this store's last `_commit_seq` assignment on `table` found the counter and the
    /// pointer as its previous assignment left them, so no other writer committed, pulled or
    /// folded between the two (`store.fold.session-listing`). False without a schema cache.
    pub(crate) fn sole_assignment(&self, table: &str) -> bool {
        self.schema_cache.as_ref().is_some_and(|c| c.sole.lock().expect("the schema cache is not poisoned").contains(table))
    }

    /// The `<run>` directories under `table`'s `data/runs/`, sorted, counted toward
    /// [`Store::run_listings`].
    fn run_dirs(&self, table: &str) -> Result<Vec<PathBuf>> {
        if let Some(cache) = &self.schema_cache {
            cache.listings.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        }
        sorted_dirs(&self.table_dir(table)?.join(contextful_core::store::lay_out::RUNS_DIR))
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
    ///
    /// A store holding a schema cache keeps the high-water it assigned beside the counter's
    /// and pointer's versions, and reads no manifest while both stand as it left them: every
    /// other writer's commit and every pulled run manifest or pointer rewrites the counter
    /// under the commit lock (`store.reserve.pull-reseed`), and every pass replaces the
    /// pointer (`store.fold.session-listing`).
    pub fn assign_commit_seq(&self, table: &str) -> Result<i64> {
        let dir = self.table_dir(table)?;
        let path = dir.join("data").join(COMMIT_SEQ_FILE);
        let versions = match &self.schema_cache {
            Some(_) => Some((file_version(&path)?, file_version(&dir.join(POINTER_FILE))?)),
            None => None,
        };
        let counter = self.commit_counter(table, &path)?;
        let held_high = self.schema_cache.as_ref().zip(versions.as_ref()).and_then(|(cache, (counter, pointer))| {
            let seqs = cache.seqs.lock().expect("the schema cache is not poisoned");
            seqs.get(table).filter(|h| h.counter == *counter && h.pointer == *pointer).map(|h| h.high)
        });
        let held = match held_high {
            Some(high) => counter.max(high),
            None => counter.max(self.recorded_commit_seq(table)?),
        };
        let next = held.checked_add(1).ok_or_else(|| ContextError::Invalid(format!("table `{table}` exhausted its commit sequence")))?;
        self.write_commit_counter(&path, next)?;
        if let (Some(cache), Some((_, pointer))) = (&self.schema_cache, versions) {
            let counter = file_version(&path)?;
            cache.seqs.lock().expect("the schema cache is not poisoned").insert(table.to_string(), HeldSeq { counter, pointer, high: next });
            let mut sole = cache.sole.lock().expect("the schema cache is not poisoned");
            if held_high.is_some() { sole.insert(table.to_string()) } else { sole.remove(table) };
        }
        Ok(next)
    }

    /// Take `table`'s commit lock and raise its counter to at least the `commit_seq` that the
    /// manifest bytes `stored`, bound for `path`, record (`store.reserve.pull-reseed`). The
    /// counter is rewritten even where it already stands above, so an assignment holding a
    /// high-water reads the manifests again. The caller writes the manifest, or the pointer
    /// naming it, before dropping the returned lock.
    pub fn raise_commit_seq(&self, table: &str, path: &Path, stored: &[u8]) -> Result<FileLock> {
        let seq = parse_commit_seq(table, path, &self.open_metadata_bytes(path, stored)?)?;
        let commit = self.lock_commit(table)?;
        let counter = self.table_dir(table)?.join("data").join(COMMIT_SEQ_FILE);
        let held = self.commit_counter(table, &counter)?.max(seq);
        self.write_commit_counter(&counter, held)?;
        Ok(commit)
    }

    /// The value the counter at `path` records, zero where none is written.
    fn commit_counter(&self, table: &str, path: &Path) -> Result<i64> {
        match self.metadata().read_optional(path)? {
            Some(bytes) => CommitCounter::read(&bytes, &format!("table `{table}`: file `{}`", path.display())),
            None => Ok(0),
        }
    }

    fn write_commit_counter(&self, path: &Path, seq: i64) -> Result<()> {
        let data = path.parent().expect("the counter sits in its table's data directory");
        fs::create_dir_all(data).at(data)?;
        let marked = CommitCounter { format_version: FormatVersion::CURRENT, commit_seq: seq };
        self.metadata().replace(path, &serde_json::to_vec(&marked).expect("a counter serializes"))
    }

    /// The highest `commit_seq` a table's manifests record: every run manifest on disk,
    /// readable or not, and the snapshot the pointer names. Zero when none records one.
    fn recorded_commit_seq(&self, table: &str) -> Result<i64> {
        let read = |path: PathBuf| -> Result<i64> {
            let Some(bytes) = self.metadata().read_optional(&path)? else { return Ok(0) };
            parse_commit_seq(table, &path, &bytes)
        };
        let mut high = match self.pointer(table)? {
            Some((ptr, _)) => read(self.snapshot_dir(table, &ptr.snapshot_id)?.join(MANIFEST_FILE))?,
            None => 0,
        };
        for run_dir in self.run_dirs(table)? {
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

    /// How many `<run>/<node>` directories of `table` hold a `_manifest.json`, from a
    /// listing alone: no manifest is read or parsed.
    pub fn manifest_count(&self, table: &str) -> Result<usize> {
        let mut count = 0;
        for run_dir in self.run_dirs(table)? {
            for node_dir in sorted_dirs(&run_dir)? {
                if node_dir.join(MANIFEST_FILE).is_file() {
                    count += 1;
                }
            }
        }
        Ok(count)
    }

    /// Every committed run of a table: each `data/runs/<run>/<node>/` holding a
    /// `_manifest.json`. A directory without one is in flight and joins nothing
    /// (`store.lay-out.uncommitted-run`); a manifest that fails to parse, or whose
    /// `run_id`, `node_id` or `table` disagrees with its path, refuses the table
    /// (`store.lay-out.manifest-unreadable`).
    pub fn committed_runs(&self, table: &str) -> Result<Vec<RunManifest>> {
        Ok(self.committed_runs_held(table)?.0)
    }

    /// Bring home each folded run manifest of `table` a lazy pull left in the bucket, those
    /// of `run_id` alone where given, through the part source (`store.pull.folded-runs-fetched`).
    /// Without a source the read refuses (`store.pull.folded-runs-unreachable`), and a manifest
    /// the source leaves absent refuses as collected (`store.pull.folded-run-collected`), save
    /// under an unbounded `listing`, which leaves out each the source answers as collected.
    fn fetch_skipped(&self, table: &str, run_id: Option<&str>, leave: Leave) -> Result<()> {
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
        // An unbounded listing leaves out a manifest a collection took; a bounded read leaves out one
        // whose run a kept snapshot folds, and the resolution refuses a bound that needs it
        // (`store.pull.folded-run-collected`, `store.bound-time.as-of-unretained`).
        let folded: BTreeSet<String> = match leave {
            Leave::Folded => self.folded_runs(table, |_| true)?.into_iter().map(|(k, _)| k).collect(),
            _ => BTreeSet::new(),
        };
        let leaves = |rel: &str| match leave {
            Leave::Collected => true,
            Leave::Folded => {
                // `tables/<t>/data/runs/<run>/<node>/_manifest.json` names run `<run>/<node>`.
                let segs: Vec<&str> = rel.split('/').collect();
                segs.len() >= 3 && folded.contains(&format!("{}/{}", segs[segs.len() - 3], segs[segs.len() - 2]))
            }
            Leave::Nothing => false,
        };
        let mut collected: BTreeSet<&str> = BTreeSet::new();
        let _hold = match source.hold(&absent) {
            Ok(hold) => Some(hold),
            Err(StoreError::SyncPartCollected(_)) if leave != Leave::Nothing => {
                for rel in absent.iter().filter(|rel| !self.root.join(rel.as_str()).exists()) {
                    match source.hold(std::slice::from_ref(rel)) {
                        Ok(_) => {}
                        Err(StoreError::SyncPartCollected(_)) if leaves(rel) => {
                            collected.insert(rel.as_str());
                        }
                        Err(e) => return Err(e.into()),
                    }
                }
                None
            }
            Err(e) => return Err(e.into()),
        };
        if let Some(gone) = absent.iter().find(|rel| !collected.contains(rel.as_str()) && !self.root.join(rel.as_str()).exists()) {
            return Err(StoreError::SyncPartCollected(format!("folded run manifest `{gone}` is gone from the bucket; the next pull stops naming it")).into());
        }
        Ok(())
    }

    /// The table's committed runs, and the lowest `commit_seq` among its grouped manifests
    /// still awaiting their group marker: a run at or above it is readable before a run
    /// below it, so a reader following commit order holds back from there
    /// (`read.subscribe.commit-order`).
    pub fn committed_runs_held(&self, table: &str) -> Result<(Vec<RunManifest>, Option<i64>)> {
        self.fetch_skipped(table, None, Leave::Collected)?;
        self.local_runs_held(table)
    }

    /// [`Store::committed_runs_held`] over the manifests on disk alone, which an unbounded read
    /// resolves against: every run a lazy pull skipped is folded into the pointer's snapshot.
    fn local_runs_held(&self, table: &str) -> Result<(Vec<RunManifest>, Option<i64>)> {
        let mut out = Vec::new();
        let mut held: Option<i64> = None;
        // One read of each (pipeline, node) commit log per call.
        let mut logs = CommitLogs::new();
        for run_dir in self.run_dirs(table)? {
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
        self.fetch_skipped(table, Some(run_id), Leave::Nothing)?;
        let run_dir = self.table_dir(table)?.join(contextful_core::store::lay_out::RUNS_DIR).join(run_id);
        let (mut logs, mut held) = (CommitLogs::new(), None);
        let mut rows: Option<u64> = None;
        for node_dir in sorted_dirs(&run_dir)? {
            let Some(m) = self.committed_at(table, &run_dir, &node_dir, &mut logs, &mut held)? else { continue };
            let mut total = rows.unwrap_or(0);
            // A lazy store brings the parts home before reading their footers.
            let rels: Vec<String> = m.parts.iter().map(|part| format!("tables/{table}/{}/{}/{}/{}", contextful_core::store::lay_out::RUNS_DIR, m.run_id, m.node_id, part.name)).collect();
            let _hold = self.hold_parts(&rels)?;
            for rel in &rels {
                total = total.saturating_add(self.parquet_row_count(&self.logical_path(rel)?)?);
            }
            rows = Some(total);
        }
        if rows.is_some() {
            return Ok(rows);
        }
        // A run whose manifest a collection removed commits on through its folded-runs row
        // (`store.lay-out.folded-committed`).
        let prefix = format!("{run_id}/");
        let folded = self.folded_runs(table, |key| key.starts_with(&prefix))?;
        if folded.is_empty() || folded.iter().any(|(_, row)| row.as_ref().and_then(|r| r.rows).is_none()) {
            return Ok(None);
        }
        Ok(Some(folded.iter().filter_map(|(_, row)| row.as_ref().and_then(|r| r.rows)).fold(0u64, u64::saturating_add)))
    }

    /// The runs `table`'s pointer folds whose key `wanted` admits, each with its folded-runs
    /// row where the chain's valid index lists it, `None` where only a walked snapshot's
    /// `includes_runs` names it (`store.lay-out.folded-index-walk`).
    pub(crate) fn folded_runs(&self, table: &str, wanted: impl Fn(&str) -> bool) -> Result<Vec<(String, Option<contextful_core::store::lay_out::FoldedRow>)>> {
        let (chain, _, index) = self.chain_for(table, None)?;
        let walked = index.as_ref().map_or(chain.len(), |ix| ix.at);
        let mut out: Vec<(String, Option<contextful_core::store::lay_out::FoldedRow>)> =
            chain[..walked].iter().flat_map(|s| s.includes_runs.iter()).filter(|k| wanted(k)).map(|k| (k.clone(), None)).collect();
        if let Some(ix) = index {
            out.extend(ix.rows.iter().filter(|(k, _)| wanted(k)).map(|(k, r)| (k.clone(), Some(r.clone()))));
        }
        Ok(out)
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

    /// The snapshot the pointer names, reading none of its ancestors.
    pub fn current_snapshot(&self, table: &str) -> Result<Option<SnapshotManifest>> {
        let Some((ptr, _)) = self.pointer(table)? else { return Ok(None) };
        let path = self.snapshot_dir(table, &ptr.snapshot_id)?.join(MANIFEST_FILE);
        let Some(bytes) = self.metadata().read_optional(&path)? else {
            return Err(ContextError::Invalid(format!("{}: missing snapshot manifest", path.display())));
        };
        if let Some(v) = FormatVersion::declared(&bytes) {
            v.admit(&format!("table `{table}`: file `{}`", path.display()))?;
        }
        let m = serde_json::from_slice(&bytes).map_err(|e| StoreError::StoreManifestUnreadable(format!("table `{table}`: file `{}`: {e}", path.display())))?;
        Ok(Some(m))
    }

    /// The snapshot `start` and its `parent` chain, newest first, read as [`Store::chain`]
    /// reads the pointer's.
    /// A collected snapshot's manifest a lazy pull skipped comes home through the part source
    /// first; without a source the walk refuses (`store.pull.husks-fetched`).
    pub fn chain_from(&self, table: &str, start: SnapshotId) -> Result<(Vec<SnapshotManifest>, bool)> {
        self.chain_walk(table, start, true, None)
    }

    /// The pointer's chain read as [`Store::chain`] reads it, ending at snapshot `stop` where the
    /// walk reaches it: a skipped manifest below `stop` stays in the bucket (`store.pull.husks-staleness`).
    pub fn chain_down_to(&self, table: &str, stop: Option<&SnapshotId>) -> Result<(Vec<SnapshotManifest>, bool)> {
        let Some((ptr, _)) = self.pointer(table)? else { return Ok((Vec::new(), false)) };
        self.chain_walk(table, ptr.snapshot_id, true, stop)
    }

    /// The pointer's chain as far as its manifests are on disk, fetching none a lazy pull
    /// skipped: the flag says it ends at an absent parent. Local retention reads it, since a
    /// skipped snapshot holds nothing on this disk (`store.pull.husks-fetched`).
    pub fn chain_on_disk(&self, table: &str) -> Result<(Vec<SnapshotManifest>, bool)> {
        let Some((ptr, _)) = self.pointer(table)? else { return Ok((Vec::new(), false)) };
        self.chain_walk(table, ptr.snapshot_id, false, None)
    }

    /// The ids of `table`'s collected snapshots whose manifests a lazy pull skipped
    /// (`store.pull.husks-skipped`).
    pub(crate) fn skipped_snapshots(&self, table: &str) -> Result<BTreeSet<String>> {
        let record = self.root.join(SKIPPED_RUNS_FILE);
        let skipped: SkippedRuns = match fs::read(&record) {
            Ok(bytes) => serde_json::from_slice(&bytes).map_err(|e| StoreError::StoreManifestUnreadable(format!("file `{}`: {e}", record.display())))?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(BTreeSet::new()),
            Err(e) => return Err(ContextError::Io { path: record, source: e }),
        };
        let prefix = format!("tables/{table}/{}/", contextful_core::store::lay_out::SNAPSHOTS_DIR);
        Ok(skipped
            .manifests
            .iter()
            .filter_map(|rel| rel.strip_prefix(&prefix)?.strip_suffix(&format!("/{MANIFEST_FILE}")).map(str::to_string))
            .filter(|id| !id.contains('/'))
            .collect())
    }

    fn chain_walk(&self, table: &str, start: SnapshotId, fetch: bool, stop: Option<&SnapshotId>) -> Result<(Vec<SnapshotManifest>, bool)> {
        let mut chain = Vec::new();
        let mut seen = std::collections::BTreeSet::new();
        let mut fetched = None;
        let mut next = Some(start);
        while let Some(id) = next {
            if !seen.insert(id.to_string()) {
                return Err(StoreError::StoreManifestUnreadable(format!(
                    "table `{table}`: snapshot {id} is its own ancestor; the parent chain does not terminate"
                ))
                .into());
            }
            let path = self.snapshot_dir(table, &id)?.join(MANIFEST_FILE);
            if fetch {
                self.fetch_skipped_snapshot(table, &path, &mut fetched)?;
            }
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
            next = if stop == Some(&id) { None } else { m.parent.clone() };
            chain.push(m);
        }
        Ok((chain, false))
    }

    /// The pointer's chain as a read under `bound` needs it, and the folded-runs index it
    /// stops at (`store.lay-out.folded-index-valid`): newest first, ending at the first
    /// snapshot whose index reads valid, or past it at the first created at or before `bound`.
    /// Without a valid index the walk reads the whole chain. The flag says the chain ends at a
    /// parent no longer on disk.
    fn chain_for(&self, table: &str, bound: Option<contextful_core::store::bound_time::Bound>) -> Result<(Vec<SnapshotManifest>, bool, Option<contextful_core::store::resolve::FoldedSet>)> {
        self.walk_chain(table, Vec::new(), None, bound, None)
    }

    /// Each run the chain from snapshot `start` folds, by [`RunManifest::key`]: the
    /// `includes_runs` walked down to the first valid index, then the index's rows
    /// (`store.lay-out.folded-index-walk`).
    pub fn folded_keys_from(&self, table: &str, start: SnapshotId) -> Result<BTreeSet<String>> {
        let (chain, _, index) = self.walk_chain(table, Vec::new(), None, None, Some(start))?;
        let walked = index.as_ref().map_or(chain.len(), |ix| ix.at);
        let mut out: BTreeSet<String> = chain[..walked].iter().flat_map(|s| s.includes_runs.iter().cloned()).collect();
        if let Some(ix) = index {
            out.extend(ix.rows.keys().cloned());
        }
        Ok(out)
    }

    /// Whether snapshot `id`'s manifest is on disk and carries a valid folded-runs index
    /// (`store.lay-out.folded-index-valid`).
    pub fn folded_index_valid(&self, table: &str, id: &SnapshotId) -> Result<bool> {
        let path = self.snapshot_dir(table, id)?.join(MANIFEST_FILE);
        let Some(bytes) = self.metadata().read_optional(&path)? else { return Ok(false) };
        let Ok(m) = serde_json::from_slice::<SnapshotManifest>(&bytes) else { return Ok(false) };
        Ok(self.folded_index(table, &m).is_some())
    }

    /// Bring the collected snapshot manifest at `path` home through the part source when a lazy
    /// pull skipped it, with every other the table's record names, in one fetch
    /// (`store.pull.husks-fetched`); a store without a source refuses
    /// (`store.pull.folded-runs-unreachable`). A manifest the record does not name fetches
    /// nothing. `fetched` holds the walk's one read of the record.
    fn fetch_skipped_snapshot(&self, table: &str, path: &Path, fetched: &mut Option<BTreeSet<String>>) -> Result<()> {
        if path.exists() {
            return Ok(());
        }
        let Ok(rel) = path.strip_prefix(&self.root) else { return Ok(()) };
        let rel = rel.to_string_lossy().replace('\\', "/");
        let first = fetched.is_none();
        let skipped = match fetched {
            Some(s) => s,
            None => fetched.insert(self.skipped_snapshots(table)?.into_iter().map(|id| format!("tables/{table}/{}/{id}/{MANIFEST_FILE}", contextful_core::store::lay_out::SNAPSHOTS_DIR)).collect()),
        };
        if !skipped.contains(&rel) {
            return Ok(());
        }
        let Some(source) = &self.part_source else {
            return Err(StoreError::SyncFoldedRunsUnfetched(format!(
                "table `{table}`: collected snapshot manifest `{rel}` stays in the bucket, and this store reads through no part source; open it where `[sync] lazy_parts` registers one"
            ))
            .into());
        };
        let absent: Vec<String> = match first {
            true => skipped.iter().filter(|r| !self.root.join(r.as_str()).exists()).cloned().collect(),
            false => vec![rel],
        };
        let _hold = source.hold(&absent)?;
        Ok(())
    }

    /// [`Store::chain_for`] continued past `chain`'s last snapshot, or from the pointer when
    /// `chain` is empty, keeping `index` where one is already read: each index validates once.
    fn walk_chain(
        &self,
        table: &str,
        mut chain: Vec<SnapshotManifest>,
        mut index: Option<contextful_core::store::resolve::FoldedSet>,
        bound: Option<contextful_core::store::bound_time::Bound>,
        start: Option<SnapshotId>,
    ) -> Result<(Vec<SnapshotManifest>, bool, Option<contextful_core::store::resolve::FoldedSet>)> {
        let mut seen: std::collections::BTreeSet<String> = chain.iter().map(|s| s.snapshot_id.to_string()).collect();
        let mut fetched = None;
        let mut next = match (chain.last(), start) {
            (Some(last), _) => last.parent.clone(),
            (None, Some(start)) => Some(start),
            (None, None) => self.pointer(table)?.map(|(ptr, _)| ptr.snapshot_id),
        };
        while let Some(id) = next {
            if !seen.insert(id.to_string()) {
                return Err(StoreError::StoreManifestUnreadable(format!(
                    "table `{table}`: snapshot {id} is its own ancestor; the parent chain does not terminate"
                ))
                .into());
            }
            let path = self.snapshot_dir(table, &id)?.join(MANIFEST_FILE);
            // A collected snapshot a lazy pull skipped answers nothing a valid index above it does
            // not; a walk without one fetches it (`store.pull.husks-fetched`).
            if index.is_none() {
                self.fetch_skipped_snapshot(table, &path, &mut fetched)?;
            }
            let bytes = match self.metadata().read_optional(&path)? {
                Some(b) => b,
                None if !chain.is_empty() => return Ok((chain, true, index)),
                None => return Err(ContextError::Invalid(format!("{}: missing snapshot manifest", path.display()))),
            };
            if let Some(v) = FormatVersion::declared(&bytes) {
                v.admit(&format!("table `{table}`: file `{}`", path.display()))?;
            }
            let m: SnapshotManifest = serde_json::from_slice(&bytes).map_err(|e| {
                StoreError::StoreManifestUnreadable(format!("table `{table}`: file `{}`: {e}", path.display()))
            })?;
            next = m.parent.clone();
            if index.is_none() {
                if let Some(rows) = self.folded_index(table, &m) {
                    index = Some(contextful_core::store::resolve::FoldedSet::new(chain.len(), rows));
                }
            }
            let reached = bound.is_none_or(|b| b.admits(m.created_at));
            chain.push(m);
            if index.is_some() && reached {
                break;
            }
        }
        Ok((chain, false, index))
    }

    /// The rows of `m`'s folded-runs index when it reads valid (`store.lay-out.folded-index-valid`),
    /// validated once per snapshot on a store holding a schema cache.
    fn folded_index(&self, table: &str, m: &SnapshotManifest) -> Option<Arc<BTreeMap<String, contextful_core::store::lay_out::FoldedRow>>> {
        let Some(cache) = &self.schema_cache else { return crate::fold::folded::read(self, table, m).map(Arc::new) };
        let key = format!("{table}/{}", m.snapshot_id);
        if let Some(rows) = cache.indexes.lock().expect("the schema cache is not poisoned").get(&key) {
            return Some(rows.clone());
        }
        let rows = Arc::new(crate::fold::folded::read(self, table, m)?);
        cache.indexes.lock().expect("the schema cache is not poisoned").insert(key, rows.clone());
        Some(rows)
    }

    /// Everything an unbounded read or a fold resolves against, for one table: the run
    /// manifests on disk, without fetching any a lazy pull skipped.
    pub fn state(&self, decl: &TableDecl) -> Result<TableState> {
        self.state_bounded(decl, None)
    }

    /// [`Store::state`], its chain read as far as a read under `bound` needs it.
    fn state_bounded(&self, decl: &TableDecl, bound: Option<contextful_core::store::bound_time::Bound>) -> Result<TableState> {
        let (chain, parent_collected, index) = self.chain_for(&decl.name, bound)?;
        self.table_state(decl, chain, parent_collected, index)
    }

    /// The table's state over a chain already read, with the run manifests on disk now.
    fn table_state(
        &self,
        decl: &TableDecl,
        chain: Vec<SnapshotManifest>,
        parent_collected: bool,
        index: Option<contextful_core::store::resolve::FoldedSet>,
    ) -> Result<TableState> {
        let table = decl.name.as_str();
        let runs = self.local_runs_held(table)?.0;
        let mut state = TableState {
            table: table.to_string(),
            write_mode: decl.write_mode(),
            runs,
            chain,
            history_collected: parent_collected,
            collected: BTreeSet::new(),
            index,
        };
        if !state.chain.is_empty() {
            let present: BTreeSet<String> = state.runs.iter().map(RunManifest::key).collect();
            state.history_collected |= state.folded(0).iter().any(|r| !present.contains(*r));
        }
        Ok(state)
    }

    /// [`Store::state`] for a read bounded by `as_of`: one resolving before the pointer's
    /// snapshot first fetches the folded runs a lazy pull skipped (`store.pull.folded-runs-fetched`).
    pub fn state_at(&self, decl: &TableDecl, as_of: Option<contextful_core::store::bound_time::Bound>) -> Result<TableState> {
        let table = decl.name.as_str();
        let (mut chain, mut parent_collected, mut index) = self.chain_for(table, None)?;
        let Some(b) = as_of.filter(|b| chain.first().is_none_or(|s| !b.admits(s.created_at))) else {
            return self.table_state(decl, chain, parent_collected, index);
        };
        self.fetch_skipped(table, None, Leave::Folded)?;
        // The unbounded walk stops at the head's valid index; the bound may need snapshots past it.
        if index.is_some() && !parent_collected && chain.last().is_some_and(|s| !b.admits(s.created_at)) {
            (chain, parent_collected, index) = self.walk_chain(table, chain, index, as_of, None)?;
        }
        let mut state = self.table_state(decl, chain, parent_collected, index)?;
        // Collected snapshots sit behind every retained one: from the first, the rest are.
        let mut beyond = false;
        for s in state.chain.iter().skip(1) {
            beyond = beyond || self.snapshot_collected(table, s)?;
            if beyond {
                state.collected.insert(s.snapshot_id.to_string());
            }
        }
        Ok(state)
    }
}

pub const ABSENT_ETAG: &str = "absent";

/// Which skipped run manifests a fetch leaves out when the part source answers them collected.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Leave {
    /// Every one: an unbounded listing.
    Collected,
    /// Those whose run a kept snapshot of the chain folds: a bounded read.
    Folded,
    /// None: a read of one run.
    Nothing,
}

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

/// The `commit_seq` the opened run or snapshot manifest `bytes` of `path` records, zero where
/// it records none.
fn parse_commit_seq(table: &str, path: &Path, bytes: &[u8]) -> Result<i64> {
    #[derive(Deserialize)]
    struct Recorded {
        #[serde(default)]
        commit_seq: Option<i64>,
    }
    let r: Recorded = serde_json::from_slice(bytes)
        .map_err(|e| StoreError::StoreManifestUnreadable(format!("table `{table}`: file `{}`: {e}", path.display())))?;
    Ok(r.commit_seq.unwrap_or(0))
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
