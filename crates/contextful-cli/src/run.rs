//! `contextful run` — the run-path surface of the command line.
//!
//! Each subcommand wires the engine to this process: the machine catalog at
//! `.contextful/context/<project>/machine.sqlite`, the journal and awakeable registry under
//! `.contextful/run/<project>/`, a command source, and the store as the destination. No
//! run rule lives here.

use crate::admit::{AdmitArgs, Author};
use crate::project::{locate, Located};
use contextful_core::grant::{describe_pipeline, list_pipelines, trace_run, Grant};
use anyhow::{bail, Context, Result};
use contextful_context::project::Project;
use clap::Subcommand;
use contextful_context::land::{commit_parts, commit_parts_group, commit_parts_with_diffs, discard_staged, publish_group, stage_part, stage_normalized_group, NormalizedStage, Batch, Position, RunContext};
use contextful_context::{commit_log, node, run_record, ContextError, Store};
use contextful_core::coordinate::Catalog;
use contextful_core::store::commit_log::{CommitEntry, Kind};
use contextful_core::ports::{Clock, FixedClock};
use contextful_core::run::cancel::Scope;
use contextful_core::run::journal::sha256_hex;
use contextful_core::run::plan::Plan;
use contextful_core::run::ports::{dead_letter_table, AwakeableStore, BlobStore, Commit, DeadLetter, Destination, JournalStore, Landed, Marker, Part, Stage};
use contextful_core::run::record::{describe_ceiling, export_ceiling, parse_bound, select_history, RunStatus, Window};
use contextful_core::run::{Failure, FailureTag, RunError};
use contextful_core::pipeline::normalize::{NormalizedGroup, Mode, Normalize};
use contextful_core::store::declare::TableDecl;
use contextful_core::store::reconcile::ColumnType;
use contextful_core::store::lay_out::SchemaDiff;
use contextful_core::store::reserve::Injection;
use contextful_core::store::StoreError;
use contextful_core::time::Instant;
use contextful_engine::awake::{AwakeError, Registry};
use contextful_engine::cancel::Keeper;
use contextful_engine::command::CommandSource;
use contextful_core::store::catalog::MACHINE_CATALOG_FILE;
use contextful_core::store::encrypt::FileCipher;
use contextful_engine::{Engine, Journal, RunSpec};
use contextful_sqlite::{MachineCatalog, SqliteRunStores};
use contextful_engine::stores::{FileAwakeableStore, FileBlobStore, FileJournalStore};
use std::cell::RefCell;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

#[derive(clap::Args)]
pub struct ProjectArgs {
    /// The project whose store root is `.contextful/context/<project>/` and whose run
    /// state is `.contextful/run/<project>/`, under the working directory; absent, the
    /// nearest `contextful.toml` upward names it.
    #[arg(long)]
    pub(crate) project: Option<String>,
    /// The evaluation instant (RFC 3339); absent reads the system clock.
    #[arg(long)]
    pub(crate) now: Option<String>,
}

#[derive(Subcommand)]
pub enum RunCmd {
    /// Start or resume a run of a plan: pull through the journal, land one commit, advance the position.
    Start {
        #[command(flatten)]
        project: ProjectArgs,
        /// The plan file; its sha256 is the reference the run pins.
        #[arg(long)]
        plan: PathBuf,
        /// The pipeline manifest holding the `[[pipeline.tables]]` declarations; absent, the
        /// project's `contextful.toml`.
        #[arg(long)]
        declaration: Option<PathBuf>,
        /// The catalog run id of this attempt; absent mints one.
        #[arg(long)]
        run_id: Option<String>,
        /// The site id of this run; replaces the manifest's `site_id` or `site_id_env`.
        #[arg(long)]
        site_id: Option<String>,
        /// The environment variable holding this run's site id; replaces the manifest's declaration.
        #[arg(long)]
        site_id_env: Option<String>,
        #[command(flatten)]
        admit: AdmitArgs,
    },
    /// Print a run row as JSON.
    Show {
        run_id: String,
        #[command(flatten)]
        project: ProjectArgs,
        #[command(flatten)]
        admit: AdmitArgs,
    },
    /// Write a stop onto a pending, running or waiting run.
    Cancel {
        run_id: String,
        #[command(flatten)]
        project: ProjectArgs,
        /// `run` halts this run; `pipeline` halts every in-flight run of its pipeline.
        #[arg(long, value_enum, default_value_t = StopScope::Run)]
        scope: StopScope,
        #[arg(long)]
        reason: Option<String>,
        #[command(flatten)]
        admit: AdmitArgs,
    },
    /// Retire a pending execution pinned to a superseded plan, recording the reason, so its scope's next fire plans fresh.
    Rewind {
        execution_id: String,
        #[command(flatten)]
        project: ProjectArgs,
        /// Why the execution retires; the retirement's run row records it.
        #[arg(long)]
        reason: String,
        /// Retire an execution no attempt has asked another plan reference of.
        #[arg(long)]
        force: bool,
    },
    /// Print run history through a window, newest first.
    History {
        #[command(flatten)]
        project: ProjectArgs,
        /// Repeatable; absent reads every pipeline.
        #[arg(long = "pipeline")]
        pipelines: Vec<String>,
        /// Inclusive lower bound on the start instant: `YYYY-MM-DD` or a UTC instant ending `Z`.
        #[arg(long)]
        since: Option<String>,
        /// Row ceiling.
        #[arg(long)]
        limit: Option<usize>,
        /// NDJSON: a header line, then one run per line.
        #[arg(long)]
        export: bool,
        #[command(flatten)]
        admit: AdmitArgs,
    },
    /// Resolve an awakeable with a payload, printing the recorded payload.
    Awake {
        token: String,
        #[command(flatten)]
        project: ProjectArgs,
        /// The payload file.
        #[arg(long)]
        payload: PathBuf,
    },
    /// Report an awakeable's state without resolving it.
    Awakeable {
        token: String,
        #[command(flatten)]
        project: ProjectArgs,
    },
}

/// A stop's grain as the command line spells it; any other spelling refuses.
#[derive(Clone, Copy, Debug, clap::ValueEnum)]
pub enum StopScope {
    Run,
    Pipeline,
}

pub(crate) use crate::clock::SystemClock;

pub(crate) fn clock(now: &Option<String>) -> Result<Arc<dyn Clock + Send + Sync>> {
    Ok(match now {
        Some(s) => Arc::new(FixedClock(Instant::parse(s)?)),
        None => Arc::new(SystemClock),
    })
}

type SharedJournal = Arc<dyn JournalStore>;
type SharedBlobs = Arc<dyn BlobStore>;
type SharedAwakeables = Arc<dyn AwakeableStore>;

pub(crate) struct Wired {
    pub(crate) engine: Engine<SharedJournal, SharedBlobs>,
    pub(crate) registry: Registry<SharedAwakeables, SharedJournal, SharedBlobs>,
    pub(crate) clock: Arc<dyn Clock + Send + Sync>,
}

impl ProjectArgs {
    /// The project and the declaration a command reads (`store.init.discovery`).
    pub(crate) fn locate(&self, declaration: Option<PathBuf>) -> Result<Located> {
        locate(self.project.as_deref(), declaration)
    }
}

/// Wire the engine to the project `--project` names or discovery finds.
pub(crate) fn wire(args: &ProjectArgs) -> Result<Wired> {
    wire_at(&args.locate(None)?.project, &args.now)
}

/// Wire the engine to a located project's run state and machine catalog.
pub(crate) fn wire_at(project: &Project, now: &Option<String>) -> Result<Wired> {
    let remote = crate::storage::preflight(project)?;
    // The run record reads past a published erasure frontier under the configured pins.
    let store = crate::project::open_store(project, None, None)?;
    let root = project.run_dir();
    let clock = clock(now)?;
    let catalog_path = project.store_root().join(MACHINE_CATALOG_FILE);
    let catalog = open_catalog(&catalog_path, clock.clone(), store.file_cipher())?;
    if remote.is_some() && !catalog.supports_strict_execution_guards() {
        bail!("StorageCapabilityMissing: replacement-safe execution requires strict dispatch and publication guards");
    }
    let (rows, blobs, awakeables): (SharedJournal, SharedBlobs, SharedAwakeables) = if let Some(remote) = remote {
        remote_run_stores(&remote)?
    } else {
        match store.file_cipher() {
            Some(cipher) => {
                let stores = SqliteRunStores::open_sealed(&catalog_path, cipher)?;
                (Arc::new(stores.journal), Arc::new(stores.blobs), Arc::new(stores.awakeables))
            }
            None => (Arc::new(FileJournalStore::open(&root)), Arc::new(FileBlobStore::open(&root)), Arc::new(FileAwakeableStore::open(&root))),
        }
    };
    // The machine catalog caches run rows in front of the store's run record
    // (`run.record.reserved-table`).
    let resolve: run_record::NodeResolver = Box::new(|s| contextful_context::node::resolve(s, |k| std::env::var(k).ok()).map(|(n, _)| n));
    let catalog: Arc<dyn Catalog + Send + Sync> = Arc::new(run_record::RecordedCatalog::new(catalog, store, resolve));
    let journal = Journal::over(rows, blobs);
    let registry = Registry::over(awakeables.clone(), journal.clone());
    Ok(Wired { engine: Engine { catalog, journal, awakeables: Some(awakeables), keeper: Keeper::default(), emitter: None, worlds: crate::component::worlds() }, registry, clock })
}

fn remote_run_stores(remote: &crate::storage::Remote) -> Result<(SharedJournal, SharedBlobs, SharedAwakeables)> {
    match remote {
        crate::storage::Remote::Postgres { url, .. } => {
            #[cfg(feature = "pg-catalog")]
            {
                let stores = contextful_pg::PgRunStores::connect(url)?;
                Ok((Arc::new(stores.journal), Arc::new(stores.blobs), Arc::new(stores.awakeables)))
            }
            #[cfg(not(feature = "pg-catalog"))]
            { let _ = url; bail!("ProfileCapabilityAbsent: durable execution stores require `pg-catalog`") }
        }
        crate::storage::Remote::D1 { endpoint, .. } => {
            #[cfg(feature = "d1-catalog")]
            { Ok((Arc::new(contextful_d1::D1JournalStore::connect(endpoint)?), Arc::new(contextful_d1::D1BlobStore::connect(endpoint)?), Arc::new(contextful_d1::D1AwakeableStore::connect(endpoint)?))) }
            #[cfg(not(feature = "d1-catalog"))]
            { let _ = endpoint; bail!("ProfileCapabilityAbsent: durable execution stores require `d1-catalog`") }
        }
    }
}

/// The variable naming the Postgres catalog a self-hosted cluster's daemons share
/// (`topology.coordinate.backends`).
pub(crate) const CATALOG_URL_VAR: &str = "CONTEXTFUL_CATALOG_URL";

/// The catalog this process coordinates through: the Postgres catalog
/// `CONTEXTFUL_CATALOG_URL` names, else the machine catalog file at `machine`, sealed under
/// `cipher` when the store is encrypted. Swapping one for the other is this wiring alone
/// (`topology.coordinate.catalog-port`).
pub(crate) fn open_catalog(machine: &Path, clock: Arc<dyn Clock + Send + Sync>, cipher: Option<Arc<dyn FileCipher>>) -> Result<Arc<dyn Catalog + Send + Sync>> {
    if std::env::var(crate::storage::STORAGE_PROFILE_VAR).ok().as_deref() == Some(crate::storage::CLOUDFLARE_D1) {
        if cipher.is_some() { bail!("StorageProfileUnsupported: `cloudflare-d1` has no sealed-row adapter") }
        let endpoint = std::env::var(crate::storage::D1_ENDPOINT_VAR).context("StorageCapabilityMissing: private D1 binding")?;
        #[cfg(feature = "d1-catalog")]
        { return Ok(Arc::new(contextful_d1::D1Catalog::connect(&endpoint)?)) }
        #[cfg(not(feature = "d1-catalog"))]
        { let _ = endpoint; bail!("ProfileCapabilityAbsent: D1 catalog requires `d1-catalog`") }
    }
    if let Some(url) = std::env::var(CATALOG_URL_VAR).ok().filter(|u| !u.trim().is_empty()) {
        return open_postgres(&url, cipher.is_some());
    }
    // The catalog's lease rows need a linearizable conditional write
    // (`surface.apply.weak-conditional-backend`).
    contextful_core::surface::control::admit_conditional("the catalog", &machine.display().to_string(), contextful_engine::fsutil::filesystem_kind(machine).as_deref())?;
    Ok(match cipher {
        Some(cipher) => Arc::new(MachineCatalog::open_sealed(machine, clock, cipher)?),
        None => Arc::new(MachineCatalog::open(machine, clock)?),
    })
}

#[cfg(feature = "pg-catalog")]
fn open_postgres(url: &str, encrypted: bool) -> Result<Arc<dyn Catalog + Send + Sync>> {
    if encrypted {
        bail!("an encrypted store seals its catalog in `{MACHINE_CATALOG_FILE}`, and the Postgres catalog `{CATALOG_URL_VAR}` names holds rows unsealed; unset `{CATALOG_URL_VAR}`");
    }
    Ok(Arc::new(contextful_pg::PgCatalog::connect(url)?))
}

#[cfg(not(feature = "pg-catalog"))]
fn open_postgres(_url: &str, _encrypted: bool) -> Result<Arc<dyn Catalog + Send + Sync>> {
    let subcommand = std::env::args().nth(1).unwrap_or_default();
    Err(crate::ProfileError::ProfileCapabilityAbsent(format!(
        "`{subcommand}` reaches the Postgres catalog `{CATALOG_URL_VAR}` names, which `{}` does not link; run it on a `contextful-full` build or one with `--features pg-catalog`",
        crate::PROFILE
    ))
    .into())
}

/// The boot identity of this machine: a process id means nothing across boots.
pub(crate) fn boot_id() -> String {
    if let Ok(id) = std::fs::read_to_string("/proc/sys/kernel/random/boot_id") {
        return id.trim().to_string();
    }
    std::process::Command::new("sysctl")
        .args(["-n", "kern.boottime"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| sha256_hex(&o.stdout)[..16].to_string())
        .unwrap_or_else(|| "unknown".to_string())
}

/// The connector artifact's content hash: its argv, and the bytes of every argument
/// naming a file.
fn artifact_hash(argv: &[String], cwd: &Path) -> String {
    let mut material = Vec::new();
    for a in argv {
        material.extend_from_slice(a.as_bytes());
        material.push(0);
        if let Ok(bytes) = std::fs::read(cwd.join(a)) {
            material.extend_from_slice(&bytes);
        }
    }
    sha256_hex(&material)
}

/// The store as a run's destination.
pub(crate) struct StoreDestination {
    pub(crate) store: Store,
    pub(crate) decls: Vec<TableDecl>,
    pub(crate) node: contextful_core::store::lay_out::NodeId,
    /// The admitted credential every commit lands under; `None` lands unauthored.
    pub(crate) author: Option<Author>,
    /// The pipeline's normalize declaration; `None` lands every object and array as `Json`.
    pub(crate) normalize: Option<Normalize>,
    pub(crate) relational_parts: BTreeMap<String, Vec<Part>>,
    pub(crate) schema_diffs: Vec<SchemaDiff>,
}

fn store_failure(e: ContextError) -> Failure {
    match e {
        ContextError::Store(StoreError::StoreSchemaIncompatible(m)) => Failure::deterministic(FailureTag::SchemaIncompatible, format!("StoreSchemaIncompatible: {m}")),
        // Another landing outlasted the commit wait; the same commit lands once it publishes
        // (`store.reserve.commit-wait`).
        ContextError::Store(e @ StoreError::StoreCommitLockTimeout(_)) => Failure::new(FailureTag::Transient, e.to_string()),
        ContextError::Store(s) => Failure::deterministic(FailureTag::Permanent, s.to_string()),
        other => Failure::new(FailureTag::Storage, other.to_string()),
    }
}

impl StoreDestination {
    fn batch(&mut self, table: &str, rows: Vec<contextful_core::run::ports::Row>, mut types: contextful_core::run::ports::Types) -> Result<Batch, Failure> {
        let decl = self.decl(table);
        if let Some(normalize) = &self.normalize {
            let stored = self.store.try_schema(table).map_err(store_failure)?.unwrap_or_default();
            let declared = decl.column_types();
            let inferred = normalize.store_types(&rows);
            if normalize.mode == Mode::Native {
                for row in &rows {
                    for (column, value) in row {
                        if !value.is_array() && !value.is_object() { continue; }
                        let held = declared.get(column).cloned().or_else(|| stored.get(column).map(|c| c.ty.clone()));
                        let downgrade = held.as_ref().filter(|ty| !ty.is_nested() && *ty != &ColumnType::Null)
                            .map(|ty| (ty.name(), "stored scalar column"))
                            .or_else(|| (!inferred.contains_key(column)).then_some(("Json".into(), "mixed value kinds")));
                        if let Some((landed_type, reason)) = downgrade {
                            let event = SchemaDiff { table:table.into(), column_path:column.clone(), source_type:if value.is_array() { "List" } else { "Struct" }.into(), landed_type, reason:reason.into() };
                            if !self.schema_diffs.contains(&event) { self.schema_diffs.push(event); }
                        }
                    }
                }
            }
            for (column, ty) in inferred {
                let scalar = stored.get(&column).is_some_and(|c| !c.ty.is_nested() && c.ty != ColumnType::Null);
                if !scalar && !declared.contains_key(&column) { types.entry(column).or_insert(ty); }
            }
        }
        Ok(Batch { rows, types:types.into_iter().collect() })
    }
    fn decl(&self, table: &str) -> TableDecl {
        self.decls.iter().find(|d| d.name == table).cloned().unwrap_or_else(|| TableDecl::named(table))
    }

    fn injection(&self, run_id: &str, site_id: &str) -> Injection {
        Injection {
            run_id: run_id.to_string(),
            site_id: site_id.to_string(),
            batch_seq: None,
            authored_by: self.author.as_ref().and_then(Author::on_behalf_of),
            authored_agent: self.author.as_ref().and_then(Author::agent),
            taint: None,
        }
    }

    fn context(&self, run_id: &str, site_id: &str, at: Instant) -> RunContext {
        RunContext { node: self.node.clone(), injection: self.injection(run_id, site_id), committed_at: at }
    }
    fn stage_prepared(&mut self, stage: Stage, prepared: &contextful_context::PreparedRecording, scope: Option<&contextful_core::run::effect::EffectScope>) -> Result<Part, Failure> {
        let mut offsets: BTreeMap<String, u64> = self.relational_parts.iter().map(|(table, parts)| (table.clone(), parts.iter().map(|part| part.rows).sum())).collect();
        offsets.insert(stage.table.clone(), stage.row_offset);
        let injection = self.injection(&stage.run_id, &stage.site_id);
        let mut parts = match scope {
            Some(scope) => contextful_context::land::stage_effect_recorded_group(&self.store, &self.decl(&stage.table), contextful_context::land::ScopedRecording { prepared, scope }, &self.node, &injection, stage.ordinal, &offsets),
            None => contextful_context::land::stage_recorded_group(&self.store, &self.decl(&stage.table), prepared, &self.node, &injection, stage.ordinal, &offsets),
        }.map_err(store_failure)?;
        let root = parts.remove(&stage.table).ok_or_else(|| Failure::new(FailureTag::Storage, "prepared recording has no root part"))?;
        for (table, part) in parts { self.relational_parts.entry(table).or_default().push(Part { name:part.name, rows:part.rows, bytes:part.bytes }); }
        Ok(Part { name:root.name, rows:root.rows, bytes:root.bytes })
    }
}

impl Destination for StoreDestination {
    fn recording_identity(&self, plan: &Plan) -> Result<Option<String>, Failure> {
        self.store.validate_source_plan(plan, self.normalize).map_err(store_failure)?;
        self.store.recording_identity(&plan.spec.table, self.normalize).map_err(store_failure)
    }
    fn validate_recorded_control(&self, plan: &Plan, pull: &contextful_core::run::ports::Pull) -> Result<(), Failure> {
        self.store.validate_recorded_control(&plan.spec.table, pull).map_err(store_failure)
    }
    fn validate_recorded_clock(&self, plan: &Plan, columns: &std::collections::BTreeSet<String>) -> Result<(), Failure> {
        self.store.validate_recording_clock(&plan.spec.table, columns, self.normalize).map_err(store_failure)
    }
    fn prepare_recorded(&mut self, table: &str, rows: Vec<contextful_core::run::ports::Row>, types: contextful_core::run::ports::Types, load_id: &str) -> Result<serde_json::Value, Failure> {
        let batch = self.batch(table, rows, types)?;
        let prepared = self.store.prepare_recording(table, &batch, self.normalize, load_id).map_err(store_failure)?;
        serde_json::from_slice(&prepared.encode().map_err(store_failure)?).map_err(|e| Failure::deterministic(FailureTag::SchemaIncompatible, e.to_string()))
    }
    fn stage_recorded(&mut self, stage: Stage, payload: &serde_json::Value) -> Result<Part, Failure> {
        let bytes = serde_json::to_vec(payload).map_err(|e| Failure::deterministic(FailureTag::SchemaIncompatible, e.to_string()))?;
        let prepared = self.store.admit_recording(&stage.table, &bytes, self.normalize).map_err(store_failure)?;
        self.stage_prepared(stage, &prepared, None)
    }
    fn admit_effect_recorded(&self, table: &str, payload: &serde_json::Value, scope: &contextful_core::run::effect::EffectScope) -> Result<contextful_core::run::effect::EmissionSummary, Failure> {
        let bytes = serde_json::to_vec(payload).map_err(|_| Failure::deterministic(FailureTag::SchemaIncompatible, "prepared body payload does not encode"))?;
        self.store.admit_effect_recording(table, &bytes, self.normalize, scope).map_err(store_failure)?.summary().map_err(store_failure)
    }
    fn stage_effect_recorded(&mut self, stage: Stage, payload: &serde_json::Value, scope: &contextful_core::run::effect::EffectScope) -> Result<Part, Failure> {
        let bytes = serde_json::to_vec(payload).map_err(|_| Failure::deterministic(FailureTag::SchemaIncompatible, "prepared body payload does not encode"))?;
        let prepared = self.store.admit_effect_recording(&stage.table, &bytes, self.normalize, scope).map_err(store_failure)?;
        self.stage_prepared(stage, &prepared, Some(scope))
    }
    fn replaces(&self, table: &str) -> bool {
        self.decl(table).write_mode() == contextful_core::store::declare::WriteMode::Replace
    }
    fn stage_batch(&mut self, stage: Stage) -> Result<Part, Failure> {
        if let Some(normalize) = self.normalize.filter(|n| n.mode == Mode::Relational) {
            let mut offsets: BTreeMap<String, u64> = self.relational_parts.iter().map(|(table, parts)| (table.clone(), parts.iter().map(|p| p.rows).sum())).collect();
            offsets.insert(stage.table.clone(), stage.row_offset);
            let group = NormalizedGroup::new(stage.rows, &stage.table, &stage.run_id, normalize.depth);
            let injection = self.injection(&stage.run_id, &stage.site_id);
            let mut parts = stage_normalized_group(&self.store, &self.decl(&stage.table), NormalizedStage { group, ordinal:stage.ordinal, offsets }, &self.node, &injection).map_err(store_failure)?;
            let root = parts.remove(&stage.table).ok_or_else(|| Failure::new(FailureTag::Storage, "normalized group has no root part"))?;
            for (table, part) in parts {
                self.relational_parts.entry(table).or_default().push(Part { name: part.name, rows: part.rows, bytes: part.bytes });
            }
            return Ok(Part { name:root.name, rows:root.rows, bytes:root.bytes });
        }
        let decl = self.decl(&stage.table);
        let injection = self.injection(&stage.run_id, &stage.site_id);
        let batch = self.batch(&stage.table, stage.rows, stage.types)?;
        let part = stage_part(&self.store, &decl, &batch, &self.node, &injection, stage.ordinal, stage.row_offset).map_err(store_failure)?;
        Ok(Part { name: part.name, rows: part.rows, bytes: part.bytes })
    }

    /// The dead letters stage as a part of the dead-letter table, which commits in the run's
    /// group beside its table (`run.land.dead-letter`).
    fn stage_dead_letters(&mut self, table: &str, run_id: &str, site_id: &str, letters: &[DeadLetter]) -> Result<(), Failure> {
        let held = dead_letter_table(table);
        // A relational child or another table already in `<table>_dead_letter` refuses, so two
        // shapes never commit into one table (`run.land.dead-letter-occupied`).
        let shaped = |columns: &[String]| ["key", "reason", "sha256"].iter().all(|c| columns.iter().any(|held| held == c)) && !columns.iter().any(|c| c == "list_index");
        let child = self.relational_parts.contains_key(&held);
        let stored = self.store.try_schema(&held).map_err(store_failure)?.map(|schema| schema.columns.iter().map(|c| c.name.clone()).collect::<Vec<_>>());
        if child || stored.as_deref().is_some_and(|columns| !shaped(columns)) {
            let by = if child { "a relational child table" } else { "a store table of another shape" };
            return Err(Failure::deterministic(FailureTag::Permanent, RunError::PipelineTableNameCollision(format!("the dead letters of `{table}` and {by} both land in `{held}`")).to_string()));
        }
        let staged = self.relational_parts.get(&held);
        let (ordinal, offset) = (staged.map_or(0, Vec::len), staged.map_or(0, |parts| parts.iter().map(|p| p.rows).sum()));
        let ordinal = u32::try_from(ordinal).map_err(|_| Failure::deterministic(FailureTag::Permanent, format!("run `{run_id}` stages more dead-letter parts than an ordinal numbers")))?;
        let batch = Batch { rows: letters.iter().map(DeadLetter::row).collect(), types: DeadLetter::types().into_iter().collect() };
        let part = stage_part(&self.store, &self.decl(&held), &batch, &self.node, &self.injection(run_id, site_id), ordinal, offset).map_err(store_failure)?;
        self.relational_parts.entry(held).or_default().push(Part { name: part.name, rows: part.rows, bytes: part.bytes });
        Ok(())
    }

    fn discard(&mut self, table: &str, run_id: &str) -> Result<(), Failure> {
        for child in self.relational_parts.keys() {
            discard_staged(&self.store, child, &self.node, run_id).map_err(store_failure)?;
        }
        self.relational_parts.clear();
        self.schema_diffs.clear();
        discard_staged(&self.store, table, &self.node, run_id).map_err(store_failure)
    }

    fn commit(&mut self, commit: Commit, precommit: &dyn Fn() -> Result<(), Failure>) -> Result<Landed, Failure> {
        let decl = self.decl(&commit.table);
        let ctx = self.context(&commit.run_id, &commit.site_id, commit.committed_at);
        let position = Position { pipeline_id: Some(commit.pipeline_id.clone()), cursor: commit.cursor.clone(), fence: commit.fence, logged: commit.fence.is_some(), replace_frontier: commit.replace_frontier };
        // Engine and credential boundary refusals preserve their original failure tags.
        let lapsed: RefCell<Option<Failure>> = RefCell::default();
        let precommit = || {
            precommit().map_err(|f| {
                let message = f.to_string();
                *lapsed.borrow_mut() = Some(f);
                ContextError::Invalid(message)
            })?;
            match &self.author {
                Some(a) => a.boundary().map_err(|e| {
                    let message = format!("{e:#}");
                    *lapsed.borrow_mut() = Some(Failure::deterministic(FailureTag::Permanent, message.clone()));
                    ContextError::Invalid(message)
                }),
                None => Ok(()),
            }
        };
        // Under a lease, the commit-log create is the commit point: the manifest stays
        // unreadable unless the log records this run under its fence.
        let commit_point = |_: &contextful_core::store::lay_out::RunManifest| match commit.fence {
            Some(fence) => {
                let entry = CommitEntry { kind: Kind::Commit, table: commit.table.clone(), run_id: Some(commit.run_id.clone()), cursor: commit.cursor.clone(), cursor_kind: Some(commit.cursor_kind), fence };
                commit_log::append(&self.store, &commit.pipeline_id, self.node.as_str(), &entry).map(|_| ())
            }
            None => Ok(()),
        };
        let names: Vec<String> = commit.parts.iter().map(|p| p.name.clone()).collect();
        let grouped = !self.relational_parts.is_empty();
        let mut group_tables = vec![commit.table.clone()];
        for (table, parts) in &self.relational_parts {
            let child_decl = self.decl(table);
            let names: Vec<String> = parts.iter().map(|p| p.name.clone()).collect();
            let child_position = Position { pipeline_id: Some(commit.pipeline_id.clone()), cursor: None, fence: None, logged: false, replace_frontier: false };
            commit_parts_group(&self.store, &child_decl, &names, &ctx, &child_position, &commit.table, &[], &|| Ok(()), &|_| Ok(())).map_err(store_failure)?;
            group_tables.push(table.clone());
        }
        let manifest = if grouped {
            commit_parts_group(&self.store, &decl, &names, &ctx, &position, &commit.table, &self.schema_diffs, &precommit, &commit_point)
        } else if !self.schema_diffs.is_empty() {
            commit_parts_with_diffs(&self.store, &decl, &names, &ctx, &position, &self.schema_diffs, &precommit, &commit_point)
        } else {
            commit_parts(&self.store, &decl, &names, &ctx, &position, &precommit, &commit_point)
        }.map_err(|e| lapsed.take().unwrap_or_else(|| store_failure(e)))?;
        self.schema_diffs.clear();
        if grouped { publish_group(&self.store, &commit.table, &group_tables, &ctx).map_err(store_failure)?; }
        self.relational_parts.clear();
        // The manifests published, behind every lower commit ticket, wake the exports reading
        // their tables (`run.export.queue-wake`); no wake-up spools before its publish returns.
        for table in &group_tables { crate::queue::announce(&self.store, table, &commit.run_id, manifest.commit_seq); }
        // The committed parts carry `_commit_seq`, so their bytes are measured after the commit.
        let dir = self.store.table_dir(&commit.table).map_err(store_failure)?.join(contextful_core::store::lay_out::RUNS_DIR).join(&commit.run_id).join(&manifest.node_id);
        let mut bytes = 0;
        for p in &manifest.parts {
            bytes += std::fs::metadata(dir.join(&p.name)).map(|m| m.len()).map_err(|e| Failure::new(FailureTag::Storage, e.to_string()))?;
        }
        Ok(Landed { rows: commit.parts.iter().map(|p| p.rows).sum(), bytes })
    }

    fn open_fence(&mut self, pipeline_id: &str, table: &str, fence: u64) -> Result<(), Failure> {
        commit_log::open_fence(&self.store, pipeline_id, self.node.as_str(), table, fence).map(|_| ()).map_err(store_failure)
    }

    fn newest_marker(&self, pipeline_id: &str, table: &str) -> Result<Option<Marker>, Failure> {
        if self.store.try_schema(table).map_err(store_failure)?.is_none() {
            return Ok(None);
        }
        let runs = self.store.committed_runs(table).map_err(store_failure)?;
        let manifests = runs
            .into_iter()
            .filter(|m| m.pipeline_id.as_deref() == Some(pipeline_id))
            .map(|m| Marker { run_id: m.run_id, cursor: m.cursor, committed_at: m.committed_at });
        // A pulled run state keeps the marker of a run whose manifest collection removed
        // (`store.pull.run-state-cursor`).
        let pulled = contextful_sync::run_state::newest_cursor(&self.store, pipeline_id, table)
            .map_err(|e| Failure::new(FailureTag::Storage, e.to_string()))?
            .map(|c| Marker { run_id: c.run_id, cursor: c.position, committed_at: c.committed_at });
        Ok(manifests.chain(pulled).max_by(|a, b| a.committed_at.cmp(&b.committed_at).then_with(|| a.run_id.cmp(&b.run_id))))
    }
}

pub(crate) use crate::project::site_id_for;

fn awake_error(e: AwakeError) -> anyhow::Error {
    match e {
        AwakeError::Refused(r) => r.into(),
        AwakeError::Storage(f) => f.into(),
    }
}

/// The grants of the credential accompanying a run-record read, admitted now; `None`
/// when none accompanies it, the local owner reading its own catalog.
fn reader_grants(admit: &AdmitArgs, project: &ProjectArgs, what: &str) -> Result<Option<Vec<Grant>>> {
    if !AdmitArgs::presented() {
        return Ok(None);
    }
    let (authority, _) = admit.admit(project.project.as_deref(), what)?;
    Ok(Some(authority.grants().to_vec()))
}

pub fn run(cmd: RunCmd) -> Result<()> {
    match cmd {
        RunCmd::Start { project, plan, declaration, run_id, site_id, site_id_env, admit } => {
            let l = project.locate(declaration)?;
            crate::sync::pull_before_run(&l)?;
            let declaration = &l.declaration;
            let text = std::fs::read_to_string(declaration).with_context(|| format!("reading the declaration `{}`", declaration.display()))?;
            let site_id = site_id_for(&text, declaration, site_id, site_id_env)?;
            let bytes = std::fs::read(&plan).with_context(|| format!("reading the plan `{}`", plan.display()))?;
            let plan = Plan::compile(&bytes).with_context(|| format!("`{}`", plan.display()))?;
            let cwd = std::env::current_dir()?;
            let decls = TableDecl::parse_pipeline(&text).with_context(|| format!("`{}`", declaration.display()))?;
            let author = admit.author(project.project.as_deref(), &text, &[&plan.spec.table])?;
            let store = Store::open_declared(&l.project.dir, &l.project.name, declaration)?;
            store.validate_source_plan(&plan, None)?;
            let (node, _) = node::resolve(&store, |k| std::env::var(k).ok())?;
            let w = wire_at(&l.project, &project.now)?;
            for reaped in w.engine.reap_orphans()? {
                eprintln!("{reaped}: reaped as partial_failure, its owner lease lapsed");
            }
            let referenced = w.registry.referenced_blobs()?;
            w.engine.journal.sweep_if_due(&referenced, w.clock.now().unix_secs())?;
            let run_id = match run_id {
                Some(id) => id,
                None => format!("run-{}", &sha256_hex(format!("{}{}", w.clock.now().unix_nanos(), std::process::id()).as_bytes())[..12]),
            };
            let connector = plan.connector_pin(&artifact_hash(&plan.spec.connector.command, &cwd));
            let trace = crate::telemetry::RunTrace::open();
            let spec = RunSpec { connector, plan: plan.clone(), run_id, site_id, pid: std::process::id(), boot_id: boot_id(), trace_id: Some(trace.trace_id()) };
            let mut source = CommandSource { argv: plan.spec.connector.command.clone(), cwd };
            let mut dest = StoreDestination { store, decls, node, author, normalize: None, relational_parts: BTreeMap::new(), schema_diffs: Vec::new() };
            let ran = w.engine.run(&spec, &mut source, &mut dest);
            if let Ok(row) = &ran {
                trace.close(row);
            }
            // Every outcome past run open publishes (`run.record.failure-publishes`).
            if w.engine.catalog.run(&spec.run_id)?.is_some() {
                let pushed = crate::sync::push_after_run(&l, w.clock.now());
                match (&ran, pushed) {
                    (Ok(row), Err(e)) if row.status == RunStatus::Success => return Err(e),
                    (_, Err(e)) => eprintln!("{e:#}"),
                    _ => {}
                }
            }
            let row = ran?;
            if row.status == RunStatus::Success {
                println!("{}: success · {} rows in {} batches", row.run_id, row.rows, row.batches);
                Ok(())
            } else {
                bail!("{}: {} — {}", row.run_id, row.status, row.error_message.unwrap_or_default())
            }
        }
        RunCmd::Show { run_id, project, admit } => {
            let grants = reader_grants(&admit, &project, "`run show`")?;
            let w = wire(&project)?;
            let row = w.engine.catalog.run(&run_id)?;
            if let Some(grants) = &grants {
                // No grant covers a run the catalog does not hold, so an absent run refuses
                // exactly as an uncovered one and a credential learns no run id.
                let covering: &[Grant] = if row.is_some() { grants } else { &[] };
                trace_run(covering, row.as_ref().map_or("", |r| r.pipeline_id.as_str()))?;
            }
            let row = row.with_context(|| format!("no run `{run_id}`"))?;
            println!("{}", serde_json::to_string_pretty(&row)?);
            Ok(())
        }
        RunCmd::Cancel { run_id, project, scope, reason, admit } => {
            let grants = if AdmitArgs::presented() { Some(admit.admit(project.project.as_deref(), "`run cancel`")?.0.grants().to_vec()) } else { None };
            let scope = match scope {
                StopScope::Run => Scope::Run,
                StopScope::Pipeline => Scope::Pipeline,
            };
            for id in stop(&wire(&project)?, grants.as_deref(), &run_id, scope, reason)? {
                println!("{id}: stop requested");
            }
            Ok(())
        }
        RunCmd::Rewind { execution_id, project, reason, force } => {
            let row = wire(&project)?.engine.rewind(&execution_id, &reason, force)?;
            println!("{execution_id}: rewound as {} · plan reference {}", row.run_id, row.plan_ref.unwrap_or_default());
            Ok(())
        }
        RunCmd::History { project, pipelines, since, limit, export, admit } => {
            let grants = reader_grants(&admit, &project, "`run history`")?;
            let located = project.locate(None)?.project;
            let w = wire_at(&located, &project.now)?;
            let since = since.map(|s| parse_bound(&s)).transpose()?;
            let ceiling = if export { export_ceiling(limit) } else { describe_ceiling(limit) };
            let window = Window { since, ceiling };
            if let Some(grants) = &grants {
                for p in &pipelines {
                    describe_pipeline(grants, p)?;
                }
            }
            let covered: Vec<String> = match (&grants, pipelines.is_empty()) {
                // A credentialed listing reads the pipelines the catalog records and keeps
                // the covered ones; none covered refuses.
                (Some(grants), true) => {
                    let mut recorded: Vec<String> = w.engine.catalog.runs(None)?.into_iter().map(|r| r.pipeline_id).collect();
                    recorded.sort();
                    recorded.dedup();
                    list_pipelines(grants, &recorded)?.into_iter().map(str::to_string).collect()
                }
                _ => pipelines.clone(),
            };
            let names: Vec<Option<&str>> = match (&grants, covered.is_empty()) {
                (None, true) => vec![None],
                _ => covered.iter().map(|p| Some(p.as_str())).collect(),
            };
            // Each pipeline's window takes the full ceiling before the merged result is clipped once.
            let mut merged = Vec::new();
            let mut truncated = false;
            for p in names {
                let page = w.engine.history(p, &window)?;
                truncated |= page.truncated;
                merged.extend(page.runs);
            }
            let page = select_history(merged, &window);
            let truncated = truncated || page.truncated;
            let store = Store::open(&located.dir, &located.name)?;
            let runs: Vec<serde_json::Value> = page.runs.iter().map(|r| -> Result<serde_json::Value> {
                let mut value = serde_json::to_value(r)?;
                let diffs: Vec<SchemaDiff> = if r.host_scope.is_some() { Vec::new() } else {
                    store.committed_runs(&r.table)?.into_iter().filter(|m| m.run_id == r.run_id)
                        .flat_map(|m| m.schema_diffs).collect()
                };
                value["schema_diffs"] = serde_json::to_value(diffs)?;
                Ok(value)
            }).collect::<Result<_>>()?;
            if export {
                let header = serde_json::json!({ "store": located.name, "window": window, "count": runs.len(), "truncated": truncated });
                println!("{header}");
                for r in &runs {
                    println!("{}", serde_json::to_string(r)?);
                }
            } else {
                println!("{}", serde_json::to_string_pretty(&serde_json::json!({ "window": window, "runs": runs, "truncated": truncated }))?);
            }
            Ok(())
        }
        RunCmd::Awake { token, project, payload } => {
            let w = wire(&project)?;
            let bytes = std::fs::read(&payload).with_context(|| format!("reading `{}`", payload.display()))?;
            let recorded = w.registry.resolve(&token, &bytes, w.clock.now()).map_err(awake_error)?;
            use std::io::Write;
            std::io::stdout().write_all(&recorded)?;
            Ok(())
        }
        RunCmd::Awakeable { token, project } => {
            let w = wire(&project)?;
            let row = w.registry.state(&token, w.clock.now()).map_err(awake_error)?;
            println!("{}", serde_json::to_string_pretty(&row)?);
            Ok(())
        }
    }
}

/// What the served resume route answers: the recorded payload of a resolution, or the
/// state of an awakeable read without resuming it.
pub(crate) enum Awoken {
    Payload(Vec<u8>),
    State(serde_json::Value),
}

/// `POST /awake/:token` with `payload`, or `GET` with none, for `caller`, the subject the
/// admitted credential verifies (`run.suspend.resume-route`, `run.suspend.caller-binding`):
/// `404` for an unknown token or one bound to another subject, `409` for a conflicting
/// resolution and `410` past the deadline.
pub(crate) fn served_awake(project: &ProjectArgs, caller: Option<&str>, token: &str, payload: Option<&[u8]>) -> Result<Awoken, (u16, serde_json::Value)> {
    let w = wire(project).map_err(|e| awake_refusal(500, "", format!("{e:#}")))?;
    let now = w.clock.now();
    match payload {
        Some(bytes) => w.registry.resolve_as(token, caller, bytes, now).map(Awoken::Payload).map_err(registry_refusal),
        None => {
            let row = w.registry.state_as(token, caller, now).map_err(registry_refusal)?;
            // The caller binding is the registry's to compare; the route never echoes it.
            let mut state = serde_json::to_value(&row).map_err(|e| awake_refusal(500, "", e.to_string()))?;
            if let Some(fields) = state.as_object_mut() {
                fields.remove("caller");
            }
            Ok(Awoken::State(state))
        }
    }
}

/// A registry refusal as the resume route's status and error body.
fn registry_refusal(e: AwakeError) -> (u16, serde_json::Value) {
    use contextful_core::run::RunError;
    match e {
        AwakeError::Refused(r) => {
            let status = match r {
                RunError::AwakeableUnknown(_) => 404,
                RunError::AwakeableAlreadyResolved(_) => 409,
                RunError::AwakeableTimedOut(_) => 410,
                _ => 500,
            };
            let text = r.to_string();
            awake_refusal(status, text.split_once(':').map_or("", |(id, _)| id).trim(), text.clone())
        }
        AwakeError::Storage(f) => awake_refusal(500, "", f.to_string()),
    }
}

fn awake_refusal(status: u16, identifier: &str, message: String) -> (u16, serde_json::Value) {
    (status, serde_json::json!({ "error": { "http": status, "identifier": identifier, "message": message } }))
}

/// Write a stop onto `run_id`, authorized against the pipeline its run record holds when
/// `grants` accompany it; the local owner presenting none stops as the catalog's owner
/// (`run.cancel.stop-unauthorized`, `run.cancel.authority-from-the-record`).
pub(crate) fn stop(w: &Wired, grants: Option<&[Grant]>, run_id: &str, scope: Scope, reason: Option<String>) -> Result<Vec<String>> {
    if let Some(grants) = grants {
        let record = w.engine.catalog.run(run_id)?;
        contextful_core::run::cancel::authorize_stop(grants, record.as_ref())?;
    }
    Ok(w.engine.cancel(run_id, scope, reason)?)
}

/// `POST /runs/:run_id/stop`: an optional JSON body of `scope` and `reason`, then
/// [`stop`] under the request's grants; `200` with the marked run ids, `403` for
/// `CancelUnauthorized`, `409` for `CancelTargetNotInFlight` (`run.cancel.stop-route`).
pub(crate) fn served_stop(project: &ProjectArgs, grants: &[Grant], run_id: &str, body: &[u8]) -> (u16, serde_json::Value) {
    use serde_json::{json, Value};
    let malformed = |why: &str| (400, json!({ "error": { "http": 400, "message": why } }));
    let fields = match body.iter().all(u8::is_ascii_whitespace) {
        true => serde_json::Map::new(),
        false => match serde_json::from_slice::<Value>(body) {
            Ok(Value::Object(fields)) => fields,
            _ => return malformed("a stop body is a JSON object of `scope` and `reason`"),
        },
    };
    let scope = match fields.get("scope").map(Value::as_str) {
        None | Some(Some("run")) => Scope::Run,
        Some(Some("pipeline")) => Scope::Pipeline,
        _ => return malformed("`scope` is `run` or `pipeline`"),
    };
    let reason = match fields.get("reason") {
        None | Some(Value::Null) => None,
        Some(Value::String(r)) => Some(r.clone()),
        _ => return malformed("`reason` is a string"),
    };
    let stopped = wire(project).and_then(|w| stop(&w, Some(grants), run_id, scope, reason));
    match stopped {
        Ok(ids) => (200, json!({ "stopped": ids })),
        Err(e) => {
            let run = e.chain().find_map(|c| {
                c.downcast_ref::<contextful_core::run::RunError>().cloned().or_else(|| match c.downcast_ref::<contextful_engine::EngineError>() {
                    Some(contextful_engine::EngineError::Refused(r)) => Some(r.clone()),
                    _ => None,
                })
            });
            let status = match run {
                Some(contextful_core::run::RunError::CancelUnauthorized(_)) => 403,
                Some(contextful_core::run::RunError::CancelTargetNotInFlight(_)) => 409,
                _ => 500,
            };
            let text = run.map(|r| r.to_string()).unwrap_or_else(|| format!("{e:#}"));
            let identifier = text.split_once(':').map_or("", |(id, _)| id).trim().to_string();
            (status, json!({ "error": { "http": status, "identifier": identifier, "message": text } }))
        }
    }
}

/// The wire snapshot of `run_id` for a credential whose read grant covers its recorded
/// pipeline, recovered from the durable record; `None` for an unrecorded or uncovered run
/// (`run.project.unauthenticated-upgrade`, `run.project.restart-discards`).
pub(crate) fn served_snapshot(project: &ProjectArgs, grants: &[Grant], run_id: &str) -> Option<String> {
    let w = wire(project).ok()?;
    let row = w.engine.catalog.run(run_id).ok()??;
    trace_run(grants, &row.pipeline_id).ok()?;
    let hub = contextful_engine::project::Hub::new(w.clock.now());
    Some(hub.connect_or_recover(&row).0.to_wire())
}
