//! `contextful pipeline` — declared pipelines on the command line.
//!
//! `run` fires one pipeline once: it reads the manifests, checks the declaration before
//! any I/O, assembles the credential resolver from the process environment, and runs
//! each table through the engine with a built-in source or a component guest. `plan`,
//! `apply` and `serve` are the lifecycle verbs over the local snapshot directory
//! (`run.declare.lifecycle-verbs`): `plan` diffs, `apply` claims a version and fires
//! nothing, and `serve` arms the applied version and dispatches each due pipeline as a
//! `run --applied` child process.

use crate::admit::AdmitArgs;
use crate::component::{self, ComponentTarget};
use crate::run::{boot_id, wire_at, ProjectArgs, StoreDestination};
use anyhow::{bail, Context, Result};
use clap::Subcommand;
use contextful_connectors::derive::{DeriveSource, HostDerive, Staged};
use contextful_connectors::file::{FileConfig, FileSource};
use contextful_connectors::http::{HttpConfig, HttpSource, Mediation};
use contextful_connectors::image::{ImageConfig, ImageSource};
use contextful_core::connector::component::ComponentSource;
use contextful_core::connector::meter::{manifest_bindings, require_binding, LimiterBinding};
use contextful_core::ports::Clock;
use contextful_core::store::lay_out::NodeId;
use contextful_core::store::ledger::RequestRecord;
use contextful_core::connector::ConnectorError;
#[cfg(feature = "s3-sync")]
use contextful_connectors::object::ObjectConfig;
use contextful_core::run::derive::config::{bind, bindings, check_output_table, check_single_output, Binding, DeriveConfig};
use contextful_core::run::derive::task::{check_host_tables, DeriveTask, Tasks};
use contextful_core::run::ports::{Row, Source, TableReader};
use contextful_core::run::{Failure, FailureTag};
use contextful_context::{node, ContextError, Store};
use contextful_core::pipeline::declare::{collect, Declared, ManifestFile, PipelineSpec, SourceBlock};
use contextful_core::pipeline::transform::Chain;
use contextful_core::run::advance::CursorKind;
use contextful_core::run::journal::sha256_hex;
use contextful_core::run::own::ConnectorPin;
use contextful_core::run::plan::{ConnectorSpec, CursorSpec, Plan, PlanSpec, NATIVE_WORLD};
use contextful_core::run::record::RunStatus;
use contextful_core::run::retry::Schedule;
use contextful_core::run::RunError;
use contextful_core::memory::declare::MemoryDeclarations;
use contextful_core::pipeline::seed::check_compaction;
use contextful_core::store::declare::{FoldCoverage, TableDecl};
use contextful_engine::RunSpec;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::Mutex;
use std::time::Instant as MonotonicInstant;
use contextful_outbound::{Intent, Outcome, PreSendHook};

struct LedgerState {
    next: u64,
    started: Option<(u64, contextful_core::time::Instant, MonotonicInstant)>,
    records: Vec<RequestRecord>,
}

/// One link-preview run's requests settle into its output table before a pull returns.
struct DeriveLedger {
    store: Store,
    table: String,
    node: NodeId,
    run_id: String,
    clock: Arc<dyn Clock + Send + Sync>,
    nonce: u128,
    state: Mutex<LedgerState>,
}

impl DeriveLedger {
    fn new(store: Store, table: String, node: NodeId, run_id: String, clock: Arc<dyn Clock + Send + Sync>) -> DeriveLedger {
        let nonce = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|elapsed| elapsed.as_nanos()).unwrap_or_default();
        DeriveLedger { store, table, node, run_id, clock, nonce, state: Mutex::new(LedgerState { next: 0, started: None, records: Vec::new() }) }
    }
}

impl PreSendHook for DeriveLedger {
    fn admit(&self, intent: &Intent) -> Result<(), String> {
        let mut state = self.state.lock().unwrap_or_else(|poison| poison.into_inner());
        if intent.run_id.as_deref() != Some(self.run_id.as_str()) {
            return Err("request carries no matching run id".into());
        }
        let next = state.next;
        state.next += 1;
        state.started = Some((next, self.clock.now(), MonotonicInstant::now()));
        Ok(())
    }

    fn settle(&self, intent: &Intent, outcome: &Outcome) {
        let mut state = self.state.lock().unwrap_or_else(|poison| poison.into_inner());
        let Some((number, started_at, started)) = state.started.take() else { return };
        let record = RequestRecord {
            request_id: format!("{}-{}-{number}", self.run_id, self.nonce),
            vendor_request_id: None,
            connector: "derive".into(),
            method: intent.method.clone(),
            url_host: intent.host.clone(),
            status_code: outcome.status,
            started_at,
            duration_ms: started.elapsed().as_millis().try_into().unwrap_or(u64::MAX),
            batch_seq: None,
        };
        state.records.push(record);
    }

    fn finish(&self, batch_seq: Option<i32>) -> Result<(), Failure> {
        let mut state = self.state.lock().unwrap_or_else(|poison| poison.into_inner());
        for record in &mut state.records {
            record.batch_seq = batch_seq;
        }
        contextful_context::ledger::append(&self.store, &self.table, &self.run_id, &self.node, &state.records)
            .map_err(|error| Failure::new(FailureTag::Storage, format!("request ledger: {error}")))?;
        state.records.clear();
        Ok(())
    }
}

#[derive(Subcommand)]
pub enum PipelineCmd {
    /// Fire one pipeline once.
    Run {
        id: String,
        #[command(flatten)]
        project: ProjectArgs,
        /// The project manifest; `pipelines/*.toml` and `pipelines/*.json` beside it are read
        /// too. Absent, the project's `contextful.toml`.
        #[arg(long)]
        declaration: Option<PathBuf>,
        /// The run id of the fire; a pipeline with several tables suffixes it with each destination table name.
        #[arg(long)]
        run_id: Option<String>,
        /// The site id of this fire; replaces the manifest's `site_id` or `site_id_env`.
        #[arg(long)]
        site_id: Option<String>,
        /// The environment variable holding this fire's site id; replaces the manifest's declaration.
        #[arg(long)]
        site_id_env: Option<String>,
        /// The instruction set a component source compiles for.
        #[arg(long, value_enum, env = "CONTEXTFUL_COMPONENT_TARGET", default_value_t = ComponentTarget::Native)]
        component_target: ComponentTarget,
        #[command(flatten)]
        admit: Box<AdmitArgs>,
        /// Fire the specification applied snapshot version N holds rather than the declared one.
        #[arg(long)]
        applied: Option<u64>,
    },
    /// Check every declared pipeline and model without network I/O; a local component artifact loads and runs discovery.
    Validate {
        #[arg(long, default_value = "contextful.toml")]
        declaration: PathBuf,
        /// The project whose directory a local component artifact resolves against, as
        /// `pipeline run` resolves it; absent, the nearest `contextful.toml` upward names it,
        /// and the working directory stands in when none does.
        #[arg(long)]
        project: Option<String>,
        /// The instruction set a component source compiles for.
        #[arg(long, value_enum, env = "CONTEXTFUL_COMPONENT_TARGET", default_value_t = ComponentTarget::Native)]
        component_target: ComponentTarget,
    },
    /// Diff the declared pipelines against the applied snapshot, writing nothing.
    Plan {
        #[command(flatten)]
        project: ProjectArgs,
        #[arg(long)]
        declaration: Option<PathBuf>,
        /// Emit the diff as JSON.
        #[arg(long)]
        json: bool,
    },
    /// Validate and claim declared pipelines and jobs; naming one pipeline preserves applied jobs.
    Apply {
        /// Converge this pipeline alone; absent, every declared pipeline.
        id: Option<String>,
        #[command(flatten)]
        project: ProjectArgs,
        #[arg(long)]
        declaration: Option<PathBuf>,
        #[command(flatten)]
        admit: Box<AdmitArgs>,
        /// Issuer seed used to sign a synced control receipt; absent, the project's default seed.
        #[arg(long)]
        issuer_key: Option<PathBuf>,
    },
    /// Take the snapshot directory's guarded import: claim v1 from the declared pipelines while
    /// no version exists. Apply refuses until a directory has taken it.
    Import {
        #[command(flatten)]
        project: ProjectArgs,
        #[arg(long)]
        declaration: Option<PathBuf>,
        #[command(flatten)]
        admit: Box<AdmitArgs>,
        /// Issuer seed used to sign a synced control receipt; absent, the project's default seed.
        #[arg(long)]
        issuer_key: Option<PathBuf>,
    },
    /// Dispatch applied pipelines and registered store-driven jobs under the cadence lease.
    Serve {
        #[command(flatten)]
        project: ProjectArgs,
        #[arg(long)]
        declaration: Option<PathBuf>,
        /// Evaluate due-ness once, wait for the dispatched units, print the answer and exit.
        #[arg(long)]
        cycle: bool,
        /// The address the external trigger's `POST /wake` listens on, such as `127.0.0.1:8788`.
        #[arg(long)]
        http: Option<String>,
        /// Locally pinned issuer keys for pulled control receipts; absent, `CONTEXTFUL_ISSUER_PUBKEY`.
        #[arg(long)]
        public_key: Option<String>,
    },
    /// Run steps a `serve` submits: take `POST /submit`, run each step as `run --applied`,
    /// heartbeat and call back through the step's awakeable route under `CONTEXTFUL_WORKER_KEY`.
    Worker {
        #[command(flatten)]
        project: ProjectArgs,
        #[arg(long)]
        declaration: Option<PathBuf>,
        /// The address the worker listens on, such as `127.0.0.1:9001`.
        #[arg(long)]
        listen: String,
    },
}

pub(crate) use crate::project::manifests;

/// The tables the manifests' scheduled, enabled folds cover (`store.declare.fold-job`).
fn fold_coverage(files: &[ManifestFile]) -> Result<FoldCoverage> {
    let mut coverage = FoldCoverage::default();
    for f in files.iter().filter(|f| f.path.ends_with(".toml")) {
        coverage.extend(FoldCoverage::parse(&f.text).with_context(|| f.path.clone())?);
    }
    Ok(coverage)
}

/// A pipeline's source, checked and ready to build.
pub(crate) enum Checked {
    /// The source and the operator's binding of the quota it declares.
    Http(HttpConfig, Option<LimiterBinding>),
    #[cfg(feature = "drive")]
    Drive(contextful_connectors::drive::DriveConfig),
    #[cfg(feature = "s3-sync")]
    Object(ObjectConfig),
    File(FileConfig),
    Image(ImageConfig),
    Derive(Box<(DeriveConfig, Binding, Option<LimiterBinding>)>),
    Component(Box<ComponentSource>),
    Host(Box<HostChecked>),
}

/// A registered host task, with the store table each of its tables lands in.
pub(crate) struct HostChecked {
    config: DeriveConfig,
    task: Arc<dyn DeriveTask>,
    tables: BTreeMap<String, String>,
}

/// Hold a specification to every rule checked before I/O, returning its source configuration.
/// A declared seed source is built as a compiled-in HTTP source, so it refuses before a seeding run reaches it.
pub(crate) fn check(spec: &PipelineSpec, declaration: &Path, tasks: &Tasks) -> Result<Checked> {
    spec.validate()?;
    contextful_core::pipeline::normalize::Normalize::parse(spec.normalize.as_ref())?;
    for op in &spec.transforms {
        op.validate()?;
    }
    if let Some(seed) = spec.seed_block()? {
        build_source(spec, "seed source", &seed.source)?;
    }
    match spec.source.name.as_str() {
        contextful_connectors::http::NAME => {
            let config = build_source(spec, "source", &spec.source)?;
            if spec.incremental.is_some() {
                config.accepts_incremental()?;
            }
            // A declared quota binds at load, ahead of any request (`connector.meter.quota-unbound`).
            let binding = match &config.limiter {
                Some(d) => {
                    let text = std::fs::read_to_string(declaration).unwrap_or_default();
                    Some(require_binding(d, &manifest_bindings(&text)?)?.clone())
                }
                None => None,
            };
            Ok(Checked::Http(config, binding))
        }
        contextful_connectors::DRIVE => {
            contextful_connectors::compiled_in(contextful_connectors::DRIVE).map_err(|why| anyhow::anyhow!("pipeline `{}`: {why}", spec.id))?;
            check_drive(spec)
        }
        contextful_connectors::object::NAME => {
            contextful_connectors::compiled_in(contextful_connectors::object::NAME).map_err(|why| anyhow::anyhow!("pipeline `{}`: {why}", spec.id))?;
            check_object(spec)
        }
        contextful_connectors::file::NAME => check_file(spec),
        contextful_connectors::image::NAME => {
            let config = ImageConfig::parse(&spec.source.config)?;
            for table in &spec.tables { config.table(table.name())?; }
            Ok(Checked::Image(config))
        }
        contextful_connectors::derive::NAME => {
            let config = DeriveConfig::parse_with(&spec.id, &spec.source.config, tasks)?;
            if let Some(task) = tasks.get(config.task.name()).filter(|_| config.task.is_host()) {
                let decls: Vec<TableDecl> = spec.tables.iter().map(|t| t.decl()).collect();
                check_host_tables(&spec.id, config.task.name(), task.as_ref(), &decls)?;
                let tables = spec.tables.iter().map(|t| (t.name().to_string(), spec.table_name(t.name()))).collect();
                return Ok(Checked::Host(Box::new(HostChecked { config, task, tables })));
            }
            let names: Vec<&str> = spec.tables.iter().map(|t| t.name()).collect();
            check_single_output(&spec.id, &names)?;
            let [table] = spec.tables.as_slice() else { unreachable!("one output table passed its check") };
            check_output_table(&TableDecl { name: spec.table_name(table.name()), ..table.decl() })?;
            let text = std::fs::read_to_string(declaration).unwrap_or_default();
            let bindings = bindings(&text)?;
            let binding = bind(&spec.id, &config, &bindings, &declaration.display().to_string())?.clone();
            let limiter = config.grant.as_ref().map(|grant| require_binding(grant, &manifest_bindings(&text)?).cloned()).transpose()?;
            Ok(Checked::Derive(Box::new((config, binding, limiter))))
        }
        other => {
            let Some(decl) = ComponentSource::parse(other, &spec.source.config).with_context(|| format!("pipeline `{}` source", spec.id))? else {
                bail!("pipeline `{}` names source `{other}`; the compiled-in sources are {}", spec.id, contextful_connectors::BUILT_IN.join(", "));
            };
            component::check(other, &decl)?;
            if let Some(field) = &spec.incremental {
                return Err(ConnectorError::ConnectorPositionOwned(format!(
                    "pipeline `{}` declares `incremental = \"{field}\"` beside component source `{other}`; the guest's cursor is the run's position",
                    spec.id
                ))
                .into());
            }
            Ok(Checked::Component(Box::new(decl)))
        }
    }
}

/// A host task's staging runs outside any execution, so nothing stops it.
struct Uncanceled;

impl contextful_core::run::ports::Cancellation for Uncanceled {
    fn requested(&self) -> bool {
        false
    }
}

/// A `file` source's configuration, its table and position checked before the walk.
fn check_file(spec: &PipelineSpec) -> Result<Checked> {
    let config = FileConfig::parse(&spec.source.config).with_context(|| format!("pipeline `{}` source", spec.id))?;
    for t in &spec.tables {
        config.table(t.name()).with_context(|| format!("pipeline `{}`", spec.id))?;
    }
    if let Some(field) = &spec.incremental {
        return Err(ConnectorError::ConnectorPositionOwned(format!(
            "pipeline `{}` declares `incremental = \"{field}\"` beside the `file` source, whose position records each file's digest",
            spec.id
        ))
        .into());
    }
    Ok(Checked::File(config))
}

/// The PDF decoder a `file` source reads pages through: this binary's worker behind the
/// process boundary, or none when the build links no PDF decoder.
fn pdf_decoder() -> Result<Option<Arc<dyn contextful_connectors::boundary::PageDecoder>>> {
    #[cfg(feature = "pdf")]
    {
        let worker = contextful_connectors::boundary::Boundary::new(std::env::current_exe()?, &["decode", "pdf"]);
        Ok(Some(Arc::new(worker)))
    }
    #[cfg(not(feature = "pdf"))]
    {
        Ok(None)
    }
}

/// A drive source's configuration, every table it lands checked before any request.
#[cfg(feature = "drive")]
fn check_drive(spec: &PipelineSpec) -> Result<Checked> {
    use contextful_connectors::drive::DriveConfig;
    let config = DriveConfig::parse(&spec.source.config).with_context(|| format!("pipeline `{}` source", spec.id))?;
    for t in &spec.tables {
        config.table(t.name()).with_context(|| format!("pipeline `{}`", spec.id))?;
    }
    if let Some(field) = &spec.incremental {
        return Err(ConnectorError::ConnectorPositionOwned(format!(
            "pipeline `{}` declares `incremental = \"{field}\"` beside the `drive` source, whose position records each file's modification time",
            spec.id
        ))
        .into());
    }
    Ok(Checked::Drive(config))
}

#[cfg(not(feature = "drive"))]
fn check_drive(spec: &PipelineSpec) -> Result<Checked> {
    bail!("pipeline `{}`: source `drive` is compiled out of this build", spec.id)
}

/// An `s3` source's configuration, its position checked against the pipeline's.
#[cfg(feature = "s3-sync")]
fn check_object(spec: &PipelineSpec) -> Result<Checked> {
    let config = ObjectConfig::parse(&spec.source.config).with_context(|| format!("pipeline `{}` source", spec.id))?;
    if let (true, Some(field)) = (config.skip_unchanged, &spec.incremental) {
        return Err(ConnectorError::ConnectorPositionOwned(format!(
            "pipeline `{}` declares `incremental = \"{field}\"` beside the `s3` source's `skip_unchanged`, whose position records each object's ETag",
            spec.id
        ))
        .into());
    }
    Ok(Checked::Object(config))
}

#[cfg(not(feature = "s3-sync"))]
fn check_object(spec: &PipelineSpec) -> Result<Checked> {
    bail!("pipeline `{}`: source `s3` is compiled out of this build", spec.id)
}

/// The `s3` source, each request presigned by the store's S3 bucket adapter and sent through
/// the mediated client; an unbound source presigns anonymously
/// (`connector.source.object-transport`).
#[cfg(feature = "s3-sync")]
fn object_source(config: ObjectConfig, resolver: Arc<contextful_outbound::Resolver>) -> contextful_connectors::object::ObjectSource {
    use contextful_connectors::object::{ObjectSource, Presign, Presigner};
    use contextful_core::run::{Failure, FailureTag};
    use contextful_sync::{S3Bucket, S3Credentials};

    struct Signer(S3Bucket);
    impl Presign for Signer {
        fn get(&self, key: &str) -> String {
            self.0.presign_get(key)
        }
        fn head(&self, key: &str) -> String {
            self.0.presign_head(key)
        }
        fn list(&self, prefix: &str, token: Option<&str>) -> String {
            self.0.presign_list(prefix, token)
        }
        fn page(&self, body: &str) -> Result<(Vec<(String, String)>, Option<String>), String> {
            S3Bucket::parse_list(body).map_err(|e| e.to_string())
        }
    }

    let presigner: Presigner = Arc::new(|c: &ObjectConfig, credentials| {
        let endpoint = c.endpoint.as_str();
        let bucket = match credentials {
            Some((access_key_id, secret_access_key)) => {
                S3Bucket::open(endpoint, &c.region, &c.bucket, S3Credentials { access_key_id, secret_access_key, session_token: None })
            }
            None => S3Bucket::anonymous(endpoint, &c.region, &c.bucket),
        }
        .map_err(|e| Failure::deterministic(FailureTag::Config, format!("`s3://{}`: {e}", c.bucket)))?;
        Ok(Box::new(Signer(bucket)) as Box<dyn Presign>)
    });
    ObjectSource::signed(config, resolver, presigner)
}

/// The store's landed tables, read for a derive source.
struct StoreReader {
    store: Store,
    decls: Vec<TableDecl>,
    admitted: Option<(contextful_context::read::Face, crate::admit::Author)>,
}

impl StoreReader {
    fn open(located: &crate::project::Located, manifest: &str, decls: Vec<TableDecl>, author: Option<&crate::admit::Author>) -> Result<StoreReader> {
        let admitted = author.map(|author| {
            let pepper = contextful_policy::enforce::mask::Pepper::resolve(|k| std::env::var(k).ok());
            crate::project::open_face(&located.project, &located.declaration, manifest, pepper).map(|face| (face, author.clone()))
        }).transpose()?;
        Ok(StoreReader { store: Store::open(&located.project.dir, &located.project.name)?, decls, admitted })
    }
}

impl TableReader for StoreReader {
    fn rows(&self, table: &str, columns: &[&str]) -> Result<Vec<Row>, Failure> {
        if let Some((face, author)) = &self.admitted {
            let fault = |e: contextful_context::read::ReadFault| {
                let message = format!("source `{table}`: {e}");
                match e {
                    contextful_context::read::ReadFault::Store(ContextError::ColumnType { .. }) => Failure::deterministic(FailureTag::SchemaIncompatible, message),
                    contextful_context::read::ReadFault::Store(_) | contextful_context::read::ReadFault::Engine(_) => Failure::new(FailureTag::Storage, message),
                    _ => Failure::deterministic(FailureTag::Config, message),
                }
            };
            author.boundary().map_err(|e| Failure::deterministic(FailureTag::Config, format!("source `{table}`: {e:#}")))?;
            let session = face.session(author.authority(), &contextful_policy::enforce::session::Request::default(), contextful_core::store::bound_time::Bounds::default()).map_err(&fault)?;
            let response = face.source_rows(&session, table, None, columns).map_err(fault)?;
            if response.truncated {
                return Err(Failure::deterministic(FailureTag::Config, format!("source `{table}`: the admitted read is truncated; a derive source requires complete input")));
            }
            let selected: Vec<_> = response.columns.iter().enumerate().filter(|(_, name)| columns.contains(&name.as_str())).collect();
            return Ok(response.rows.into_iter().map(|row| selected.iter().map(|(index, name)| ((*name).clone(), row[*index].clone())).collect()).collect());
        }
        let decl = self.decls.iter().find(|d| d.name == table).cloned().unwrap_or_else(|| TableDecl::named(table));
        contextful_context::rows::table_rows(&self.store, &decl, columns).map_err(|e| match e {
            ContextError::ColumnType { .. } => Failure::deterministic(FailureTag::SchemaIncompatible, format!("`{table}`: {e}")),
            e => Failure::new(FailureTag::Storage, e.to_string()),
        })
    }
}

/// Build one source block of `spec` as a compiled-in source and bind every table to it.
fn build_source(spec: &PipelineSpec, role: &str, source: &SourceBlock) -> Result<HttpConfig> {
    if source.name != contextful_connectors::http::NAME {
        bail!(
            "pipeline `{}` names {role} `{}`; the compiled-in sources are {}",
            spec.id,
            source.name,
            contextful_connectors::BUILT_IN.join(", ")
        );
    }
    let built = || -> Result<HttpConfig> {
        let config = HttpConfig::parse(&source.config)?;
        for t in &spec.tables {
            config.allowlist(t.name())?;
        }
        Ok(config)
    };
    built().with_context(|| format!("pipeline `{}` {role}", spec.id))
}

/// The connector a built-in source's run pins.
fn built_in(spec: &PipelineSpec) -> ConnectorSpec {
    ConnectorSpec {
        id: spec.source.name.clone(),
        version: env!("CARGO_PKG_VERSION").to_string(),
        world: NATIVE_WORLD.to_string(),
        command: vec![format!("builtin:{}", spec.source.name)],
    }
}

/// The plan one table of `spec` runs against, pinning `connector`.
fn plan(spec: &PipelineSpec, table: &str, connector: ConnectorSpec) -> Result<Plan> {
    let cursor_kind = if spec.incremental.is_some() { CursorKind::Monotonic } else { CursorKind::OpaqueToken };
    let plan = Plan {
        spec: PlanSpec {
            pipeline: spec.id.clone(),
            table: spec.table_name(table),
            connector,
            cursor: CursorSpec { kind: Some(cursor_kind.name().to_string()), field: spec.incremental.clone() },
            retry: None,
            // A derive pipeline re-reads the store each tick and records no pull.
            journal: spec.journals(),
            redact: Vec::new(),
            redaction: spec.tables.iter().find(|t| t.name() == table).and_then(|t| spec.destination_decl(t).redaction).unwrap_or_default(),
        },
        content_hash: spec.content_hash(),
        cursor_kind,
        schedule: Schedule::default(),
    };
    plan.validate()?;
    Ok(plan)
}

/// The store as the port a drive source lands each body it reads through.
#[cfg(feature = "drive")]
struct StoreBodies(Store);

#[cfg(feature = "drive")]
impl contextful_connectors::drive::BodyStore for StoreBodies {
    fn put(&self, sha256: &str, bytes: &[u8]) -> std::result::Result<(), Failure> {
        self.0.land_blob(sha256, bytes).map_err(|e| Failure::new(FailureTag::Permanent, format!("landing body `{sha256}`: {e}")))
    }
}

/// The directory `pipeline run` resolves a declaration's relative paths against
/// (`store.init.declaration-base`): the located project's, or the working directory when
/// discovery names no project.
/// The directory a local artifact resolves against, and the located store's pin switch
/// (`connector.package.pin-requirement`); with no project located, the working directory
/// and no store-wide switch.
fn declaration_base(project: Option<&str>) -> Result<(PathBuf, bool)> {
    match crate::project::locate(project, None) {
        Ok(l) => {
            let store_pin = Store::open(&l.project.dir, &l.project.name)?.requires_connector_pin();
            Ok((l.project.dir, store_pin))
        }
        Err(_) if project.is_none() => Ok((std::env::current_dir()?, false)),
        Err(e) => Err(e),
    }
}

pub fn run(cmd: PipelineCmd, tasks: &Tasks, bodies: &contextful_core::run::drive::Bodies) -> Result<()> {
    match cmd {
        PipelineCmd::Validate { declaration, project, component_target } => {
            let files = manifests(&declaration)?;
            let (base, store_pin) = declaration_base(project.as_deref())?;
            if files.is_empty() {
                return Err(RunError::PipelineManifestMissing(format!(
                    "no manifest at `{}` and no `pipelines/*.toml` or `pipelines/*.json` beside it",
                    declaration.display()
                ))
                .into());
            }
            // Store tables declared under `[pipeline]` answer to the same load checks the
            // store and the read face run.
            let mut tables: BTreeMap<String, TableDecl> = BTreeMap::new();
            for f in files.iter().filter(|f| f.path.ends_with(".toml")) {
                for t in TableDecl::parse_pipeline(&f.text).with_context(|| f.path.clone())? {
                    tables.insert(t.name.clone(), t);
                }
            }
            // A `[[table]]` memory table is keyed on its shape's key.
            for f in files.iter().filter(|f| f.path.ends_with(".toml")) {
                for t in MemoryDeclarations::parse(&f.text).with_context(|| f.path.clone())?.tables {
                    tables.entry(t.name.clone()).or_insert_with(|| t.table_decl());
                }
            }
            let coverage = fold_coverage(&files)?;
            let declared = collect(&files)?;
            for d in &declared {
                let at = || format!("{}:{}", d.file, d.line);
                let checked = check(&d.spec, &declaration, tasks).with_context(at)?;
                check_compaction(&d.spec, &coverage).with_context(at)?;
                let mut discovered = String::new();
                if let Checked::Component(decl) = &checked {
                    crate::connector::admit_hosts(decl, &base).with_context(at)?;
                    if component::is_local(decl) {
                        let loaded = component::load(&d.spec.source.name, decl, &base, "", None, component_target, store_pin).with_context(at)?;
                        let names = loaded.discover(decl).with_context(at)?;
                        discovered = format!(" · discovers {}", names.join(", "));
                    }
                }
                println!("{}: valid ({} tables, content hash {}){discovered}", d.spec.id, d.spec.tables.len(), &d.spec.content_hash()[..16]);
                for t in &d.spec.tables {
                    let name = d.spec.table_name(t.name());
                    tables.entry(name).or_insert_with(|| d.spec.destination_decl(t));
                }
            }
            let models = contextful_core::pipeline::model::collect_models(&files, &declared)?;
            let model_ids: std::collections::BTreeSet<&str> = models.iter().map(|m| m.spec.id.as_str()).collect();
            for m in &models {
                let mut spec = m.spec.clone();
                crate::model_source::resolve(&mut spec, &m.file).with_context(|| format!("{}:{}", m.file, m.line))?;
                spec.validate().with_context(|| format!("{}:{}", m.file, m.line))?;
                let reads = contextful_context::build::admit_statements(&spec, |t| tables.get(t).cloned())
                    .map_err(|(what, e)| anyhow::Error::from(e).context(format!("{}:{} {what}", m.file, m.line)))?;
                for relation in reads.iter().filter(|name| !tables.contains_key(*name) && !model_ids.contains(name.as_str())) {
                    eprintln!("model `{}` reads `{relation}`, which no manifest declares; `build` resolves it against the store", m.spec.id);
                }
                println!("{}: valid model ({} tests)", m.spec.id, m.spec.tests.len());
            }
            for t in tables.values().filter(|t| t.is_keyed() && !coverage.covers(&t.name)) {
                eprintln!(
                    "warning: table `{}` declares a primary key and no enabled `[[job]]` of kind `fold` with a `schedule` covers it",
                    t.name
                );
            }
            Ok(())
        }
        PipelineCmd::Plan { project, declaration, json } => crate::cadence::plan(&project, declaration, json),
        PipelineCmd::Apply { id, project, declaration, admit, issuer_key } => {
            crate::cadence::apply(&project, declaration, id.as_deref(), tasks, bodies, &admit, issuer_key.as_deref())
        }
        PipelineCmd::Import { project, declaration, admit, issuer_key } => {
            crate::cadence::import(&project, declaration, tasks, bodies, &admit, issuer_key.as_deref())
        }
        PipelineCmd::Serve { project, declaration, cycle, http, public_key } => crate::cadence::serve(&project, declaration, cycle, http.as_deref(), public_key.as_deref(), tasks, bodies),
        PipelineCmd::Worker { project, declaration, listen } => crate::worker::serve_worker(&project, declaration, &listen),
        PipelineCmd::Run { id, project, declaration, run_id, site_id, site_id_env, component_target, admit, applied } => {
            let l = project.locate(declaration)?;
            crate::sync::pull_before_run(&l)?;
            let declaration = l.declaration.clone();
            let text = if declaration.exists() { std::fs::read_to_string(&declaration)? } else { String::new() };
            let site_id = crate::run::site_id_for(&text, &declaration, site_id, site_id_env)?;
            let files = match applied {
                Some(version) => vec![crate::cadence::snapshot_manifest(&l.project, &text, version)?],
                None => manifests(&declaration)?,
            };
            let declared: Vec<Declared> = collect(&files)?;
            let d = declared.into_iter().find(|d| d.spec.id == id).with_context(|| match applied {
                Some(v) => format!("applied version v{v} holds no pipeline `{id}`"),
                None => format!("no pipeline `{id}` is declared"),
            })?;
            let spec = d.spec;
            let checked = check(&spec, &declaration, tasks)?;
            let destinations: Vec<String> = spec.tables.iter().map(|t| spec.table_name(t.name())).collect();
            let author = admit.author(project.project.as_deref(), &text, &destinations.iter().map(String::as_str).collect::<Vec<_>>())?;
            check_compaction(&spec, &fold_coverage(&manifests(&declaration)?)?)?;
            let w = wire_at(&l.project, &project.now)?;
            let vars: BTreeMap<String, String> = std::env::vars().collect();
            let resolver = Arc::new(contextful_outbound::assemble(&vars, w.clock.clone())?);
            match &checked {
                Checked::Http(config, _) => resolver.preflight(config.headers.values())?,
                Checked::Component(decl) => resolver.preflight(decl.attach.iter().map(|(_, t)| t))?,
                #[cfg(feature = "drive")]
                Checked::Drive(config) => resolver.preflight(config.templates())?,
                Checked::Derive(_) | Checked::Host(_) | Checked::File(_) | Checked::Image(_) => {}
                #[cfg(feature = "s3-sync")]
                Checked::Object(config) => resolver.preflight(config.credentials())?,
            }

            // A declaration's relative paths resolve against the project directory
            // (`store.init.declaration-base`), whichever subdirectory the command runs from.
            let base = l.project.dir.clone();
            let store = Store::open_declared(&l.project.dir, &l.project.name, &declaration)?;
            // A component resolves, admits and compiles once per fire, before any run row.
            let loaded = match &checked {
                Checked::Component(decl) => Some(component::load(&spec.source.name, decl, &base, &l.project.name, Some(&resolver), component_target, store.requires_connector_pin())?),
                _ => None,
            };
            // One drive fire mints one token and walks the tree once for every table it lands.
            #[cfg(feature = "drive")]
            let drive = match &checked {
                Checked::Drive(config) => {
                    // PDF bodies decode in this binary's worker, behind the process boundary.
                    let worker = contextful_connectors::boundary::Boundary::new(std::env::current_exe()?, &["decode", "pdf"]);
                    // Each body read whole lands under the store's `blobs/` (`store.lay-out.landed-blob`).
                    let bodies = StoreBodies(Store::open(&l.project.dir, &l.project.name)?);
                    Some(contextful_connectors::drive::Drive::new(config.clone(), resolver.clone(), Arc::new(worker), Arc::new(bodies))?)
                }
                _ => None,
            };
            let (node, _) = node::resolve(&store, |k| std::env::var(k).ok())?;
            let decls: Vec<TableDecl> = spec
                .tables
                .iter()
                .map(|t| spec.destination_decl(t))
                .collect();
            let normalize = Some(contextful_core::pipeline::normalize::Normalize::parse(spec.normalize.as_ref())?);
            let mut dest = StoreDestination { store, decls, node, author, normalize, relational_parts: Default::default(), schema_diffs: Vec::new() };
            for reaped in w.engine.reap_orphans()? {
                eprintln!("{reaped}: reaped as partial_failure, its owner lease lapsed");
            }
            let base_run = run_id.unwrap_or_else(|| format!("run-{}", &sha256_hex(format!("{}{}", w.clock.now().unix_nanos(), std::process::id()).as_bytes())[..12]));
            let artifact = match &loaded {
                Some(c) => c.content_hash(),
                None => sha256_hex(serde_json::to_string(&spec.source).unwrap_or_default().as_bytes()),
            };
            // A host task derives each unit once, then lands its content tables and its
            // marker table last, stopping at the first failing table (`run.emit.marker-last`).
            let mut staged: BTreeMap<String, Vec<Row>> = BTreeMap::new();
            let mut derive_skipped = 0;
            let mut order: Vec<&contextful_core::pipeline::declare::TableEntry> = spec.tables.iter().collect();
            if let Checked::Host(host) = &checked {
                let derive = HostDerive {
                    pipeline_id: spec.id.clone(),
                    config: host.config.clone(),
                    task: host.task.clone(),
                    tables: host.tables.clone(),
                    retention_columns: spec.tables.iter().filter_map(|t| {
                        t.decl().retain_rows.map(|r| (t.name().to_string(), r.column))
                    }).collect(),
                    reader: Box::new(StoreReader::open(&l, &text, dest.decls.clone(), dest.author.as_ref())?),
                };
                let (landing, skipped) = derive.stage(&Uncanceled).map_err(|f| anyhow::anyhow!("pipeline `{}`: {f}", spec.id))?;
                derive_skipped = skipped;
                order = landing.iter().filter_map(|(table, _)| spec.tables.iter().find(|t| spec.table_name(t.name()) == *table)).collect();
                staged = landing.into_iter().collect();
            }
            let stop_on_failure = matches!(checked, Checked::Host(_)) || spec.on_table_error() == contextful_core::pipeline::declare::OnTableError::Abort;
            let mut failed: Vec<String> = Vec::new();
            let mut tallies: Vec<(String, IngestTally)> = Vec::new();
            for t in order {
                let table = spec.table_name(t.name());
                // The destination name is path-safe, so a run id built from it is too.
                let run_id = if spec.tables.len() == 1 { base_run.clone() } else { format!("{base_run}.{table}") };
                // A table that cannot open is a failed table like one whose run fails, so
                // `continue` lands the others.
                let mut limiters: Vec<Arc<contextful_outbound::Limiter>> = Vec::new();
                let outcome = (|| -> Result<contextful_core::run::record::RunRow> {
                    let connector = loaded.as_ref().map_or_else(|| built_in(&spec), component::Loaded::connector_spec);
                    let plan = plan(&spec, t.name(), connector)?;
                    dest.store.validate_source_plan(&plan, dest.normalize)?;
                    let connector: ConnectorPin = plan.connector_pin(&artifact);
                    let run = RunSpec { plan, connector, run_id: run_id.clone(), site_id: site_id.clone(), pid: std::process::id(), boot_id: boot_id(), trace_id: None };
                    let shape = Chain { ops: spec.transforms.clone(), table: table.clone() };
                    let mut source: Box<dyn Source> = match &checked {
                        Checked::Http(config, binding) => {
                            let limiter = binding.clone().map(|b| contextful_outbound::Limiter::new(b, resolver.clone(), &run_id, w.clock.clone()).map(Arc::new)).transpose()?;
                            limiters.extend(limiter.clone());
                            let mediation = Mediation { limiter, run_id: Some(run_id.clone()), ..Mediation::default() };
                            let source = HttpSource::mediated(config.clone(), t.name(), resolver.clone(), mediation)?;
                            Box::new(if let Some(field) = spec.incremental.as_deref() { source.watermarked_for(field) } else { source })
                        }
                        #[cfg(feature = "drive")]
                        Checked::Drive(_) => match &drive {
                            Some(d) => Box::new(d.source(t.name())?),
                            None => bail!("pipeline `{}`: the drive source did not open", spec.id),
                        },
                        #[cfg(feature = "s3-sync")]
                        Checked::Object(config) => Box::new(object_source(config.clone(), resolver.clone())),
                        Checked::File(config) => Box::new(FileSource::new(config.clone(), &base, pdf_decoder()?)),
                        Checked::Image(config) => Box::new(ImageSource::new(config.clone(), &base)),
                        Checked::Derive(pair) => {
                            let limiter = pair.2.clone().map(|binding| contextful_outbound::Limiter::new(binding, resolver.clone(), &run_id, w.clock.clone()).map(Arc::new)).transpose()?;
                            limiters.extend(limiter.clone());
                            let hook = if pair.0.task == contextful_core::run::derive::config::Task::LinkPreview {
                                Some(Arc::new(DeriveLedger::new(dest.store.clone(), table.clone(), dest.node.clone(), run_id.clone(), w.clock.clone())) as Arc<dyn PreSendHook>)
                            } else {
                                None
                            };
                            let mediation = Mediation { limiter, hook, run_id: Some(run_id.clone()), ..Mediation::default() };
                            Box::new(DeriveSource {
                                pipeline_id: spec.id.clone(),
                                config: pair.0.clone(),
                                binding: pair.1.clone(),
                                output_table: table.clone(),
                                output_schema: serde_json::to_value(dest.decls.iter().find(|d| d.name == table).and_then(|d| d.columns.clone()))?,
                                reader: Box::new(StoreReader::open(&l, &text, dest.decls.clone(), dest.author.as_ref())?),
                                resolver: resolver.clone(),
                                mediation,
                                store_root: Some(dest.store.root().to_path_buf()),
                                cwd: base.clone(),
                            })
                        }
                        Checked::Component(decl) => match &loaded {
                            Some(c) => c.source(decl, t.name(), &resolver, &run_id)?,
                            None => bail!("pipeline `{}`: component source `{}` did not load", spec.id, spec.source.name),
                        },
                        Checked::Host(_) => Box::new(Staged(staged.get(&table).cloned().unwrap_or_default(), derive_skipped)),
                    };
                    Ok(w.engine.run_with(&run, &mut source, &shape, &mut dest)?)
                })();
                // The run's reports surrender every unspent permit (`connector.meter.report-delivery`).
                for limiter in &limiters {
                    limiter.finish();
                    for entry in limiter.audit() {
                        eprintln!("{table}: {entry}");
                    }
                }
                let row = match outcome {
                    Ok(row) => row,
                    Err(e) => {
                        eprintln!("{}", RunError::PipelineTableFailed(format!("table `{table}` failed as refused in run `{run_id}`: {e:#}")));
                        tallies.push((table.clone(), IngestTally { failed: 1, ..IngestTally::default() }));
                        failed.push(run_id);
                        if stop_on_failure {
                            break;
                        }
                        continue;
                    }
                };
                tallies.push((table.clone(), IngestTally::of(&row)));
                if row.status == RunStatus::Success {
                    let mut skipped = if row.skipped > 0 { format!(" · {} skipped", row.skipped) } else { String::new() };
                    if !row.declined.is_empty() {
                        let by: Vec<String> = row.declined.iter().map(|(ext, n)| if ext.is_empty() { format!("{n} without extension") } else { format!("{n} .{ext}") }).collect();
                        skipped.push_str(&format!(" ({})", by.join(", ")));
                    }
                    println!("{table}: {} success · {} rows in {} batches{skipped}", row.run_id, row.rows, row.batches);
                    continue;
                }
                let e = RunError::PipelineTableFailed(format!(
                    "table `{table}` failed as {} in run `{}`: {}",
                    row.error_kind.map(|k| k.name()).unwrap_or("unknown"),
                    row.run_id,
                    row.error_message.unwrap_or_default()
                ));
                eprintln!("{e}");
                failed.push(row.run_id);
                if stop_on_failure {
                    break;
                }
            }
            let total = tallies.iter().fold(IngestTally::default(), |sum, (_, t)| sum.plus(t));
            println!("tally: {total}");
            for (table, t) in &tallies {
                println!("tally {table}: {t}");
            }
            if !failed.is_empty() {
                bail!("pipeline `{}`: {} table(s) failed: {}", spec.id, failed.len(), failed.join(", "));
            }
            Ok(())
        }
    }
}

/// One fire's ingest counts, for a table or summed over the fire (`run.land.ingest-tally`).
#[derive(Debug, Clone, Copy, Default)]
struct IngestTally {
    fetched: u64,
    kept: u64,
    skipped: u64,
    failed: u64,
    /// Rows the declared `filter` operations dropped: the chain never adds a row, so this
    /// is `fetched - kept`.
    dropped_low_quality: u64,
}

impl IngestTally {
    fn of(row: &contextful_core::run::record::RunRow) -> IngestTally {
        if row.status != RunStatus::Success {
            return IngestTally { failed: 1, ..IngestTally::default() };
        }
        IngestTally { fetched: row.fetched, kept: row.kept, skipped: row.skipped, failed: 0, dropped_low_quality: row.fetched.saturating_sub(row.kept) }
    }

    fn plus(self, other: &IngestTally) -> IngestTally {
        IngestTally {
            fetched: self.fetched.saturating_add(other.fetched),
            kept: self.kept.saturating_add(other.kept),
            skipped: self.skipped.saturating_add(other.skipped),
            failed: self.failed.saturating_add(other.failed),
            dropped_low_quality: self.dropped_low_quality.saturating_add(other.dropped_low_quality),
        }
    }
}

impl std::fmt::Display for IngestTally {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "fetched {} · kept {} · skipped {} · failed {} · dropped_low_quality {}",
            self.fetched, self.kept, self.skipped, self.failed, self.dropped_low_quality
        )
    }
}
