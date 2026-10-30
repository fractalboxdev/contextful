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
use contextful_core::run::derive::available::AvailableSelect;
use contextful_connectors::derive::{DeriveSource, HostDerive, Staged};
use contextful_connectors::file::{FileConfig, FileSource};
use contextful_connectors::http::{HttpConfig, HttpSource, Mediation};
use contextful_connectors::image::{ImageConfig, ImageSource};
use contextful_connectors::report::declared::{DeclaredVendor, ReportDeclaration};
use contextful_connectors::report::ReportSource;
use contextful_core::connector::component::ComponentSource;
use contextful_core::connector::meter::{manifest_bindings, require_binding, LimiterBinding};
use contextful_core::ports::Clock;
use contextful_core::store::lay_out::NodeId;
use contextful_core::store::ledger::RequestRecord;
use contextful_core::connector::ConnectorError;
#[cfg(feature = "s3-sync")]
use contextful_connectors::object::ObjectConfig;
use contextful_core::run::derive::config::{bind, bindings, check_output_table, check_single_output, Binding, DeriveConfig};
use contextful_core::run::derive::engine::check_locality;
use contextful_core::memory::synthesize::{CallRecord, Inference, Message};
use contextful_core::run::derive::config::ModelEndpoint;
use contextful_core::run::derive::task::{check_host_tables, Answer, DeriveTask, MeteredClient, Request, Tasks};
use contextful_outbound::client::HeaderValue;
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

/// The run's metered client for a host task declaring `pure` false: completions reach the
/// declared model endpoint, every request's allowlist is that endpoint's host alone, and each
/// request passes the run's request-ledger hook under the marker table's run id
/// (`run.bind.model-endpoint`, `run.bind.off-endpoint`).
struct HostClient {
    endpoint: contextful_outbound::infer::Endpoint,
    client: contextful_outbound::client::Client,
}

impl HostClient {
    /// The client for one run; under a `grant`, every request and completion reserves a
    /// permit against `meter` first (`run.bind.reach-grant`).
    fn open(model: &ModelEndpoint, hook: Arc<dyn PreSendHook>, run_id: &str, meter: Option<contextful_outbound::Meter>) -> Result<HostClient> {
        let key = std::env::var(crate::memory::INFERENCE_KEY_VAR).ok();
        let endpoint = contextful_outbound::infer::Endpoint::new(&model.endpoint, &model.model, key).map_err(anyhow::Error::msg)?;
        let mut endpoint = crate::telemetry::traced(endpoint).with_hook(hook.clone()).for_run(run_id);
        let origin = url::Url::parse(&model.endpoint)?;
        let allow = contextful_core::connector::attach::Allowlist::parse(&[origin.host_str().unwrap_or_default()])?;
        let mut client = contextful_outbound::client::Client::new(allow, origin).with_hook(hook).for_run(run_id);
        if let Some(meter) = meter {
            endpoint = endpoint.metered(meter.clone());
            client = client.metered(meter);
        }
        Ok(HostClient { endpoint, client })
    }
}

/// A mediated client's failure as a host task reads it: a refusal no retry changes is
/// `DeriveMeteredClient` (`run.bind.off-endpoint`); any other fails the unit.
fn host_failure(failure: Failure) -> RunError {
    if failure.deterministic && failure.tag == FailureTag::Config {
        RunError::DeriveMeteredClient(failure.message)
    } else {
        RunError::Invalid(failure.message)
    }
}

impl MeteredClient for HostClient {
    fn send(&self, request: &Request) -> Result<Answer, RunError> {
        let url = url::Url::parse(&request.url).map_err(|e| RunError::DeriveMeteredClient(format!("`{}` is no URL: {e}", request.url)))?;
        let headers: Vec<(String, HeaderValue)> = request.headers.iter().map(|(k, v)| (k.clone(), HeaderValue::Plain(v.clone()))).collect();
        let response = self.client.send(&request.method, &url, &headers, request.body.as_deref()).map_err(host_failure)?;
        Ok(Answer { status: response.status, body: response.body })
    }

    fn complete(&self, messages: &[Message]) -> Result<CallRecord, RunError> {
        self.endpoint.call(messages).map_err(|e| RunError::Invalid(format!("model call: {e}")))
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
        /// Print a derive pipeline's eligible, already-derived and outstanding unit counts
        /// from one selection, calling no engine and landing no row.
        #[arg(long)]
        dry_run: bool,
        /// Select only the derive parents available at this bound (`run.select.available-select`).
        #[arg(long)]
        available_as_of: Option<String>,
    },
    /// Run one provider step: one table pull, or one chunk of a table's seed plan, and print its
    /// commit pointer. A replay of a completed step prints that pointer and runs nothing.
    Step {
        id: String,
        /// The table the step pulls, by declared or destination name; absent, the pipeline's one table.
        #[arg(long)]
        table: Option<String>,
        /// Run the chunk of this ordinal in the table's seed plan in place of its live pull.
        #[arg(long)]
        chunk: Option<u32>,
        #[command(flatten)]
        project: ProjectArgs,
        #[arg(long)]
        declaration: Option<PathBuf>,
        /// The step's id: its first attempt runs as `pipeline run --run-id <id>` names the
        /// unit's run, and attempt k of a replay under `<id>.replay-<k>`.
        #[arg(long)]
        run_id: String,
        /// The site id of this step; replaces the manifest's `site_id` or `site_id_env`.
        #[arg(long)]
        site_id: Option<String>,
        /// The environment variable holding this step's site id; replaces the manifest's declaration.
        #[arg(long)]
        site_id_env: Option<String>,
        /// The instruction set a component source compiles for.
        #[arg(long, value_enum, env = "CONTEXTFUL_COMPONENT_TARGET", default_value_t = ComponentTarget::Native)]
        component_target: ComponentTarget,
        #[command(flatten)]
        admit: Box<AdmitArgs>,
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
    /// Land one delivered batch of a `receive` pipeline from stdin: one JSON object per line,
    /// `table` and `row`. The answer follows every touched table's commit and names the run id.
    Receive {
        id: String,
        /// The batch key: a resend under it replays the tables that committed and lands the rest.
        #[arg(long)]
        key: String,
        #[command(flatten)]
        project: ProjectArgs,
        #[arg(long)]
        declaration: Option<PathBuf>,
        /// The site id of this batch; replaces the manifest's `site_id` or `site_id_env`.
        #[arg(long)]
        site_id: Option<String>,
        /// The environment variable holding this batch's site id.
        #[arg(long)]
        site_id_env: Option<String>,
        #[command(flatten)]
        admit: Box<AdmitArgs>,
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
        /// Issuer seed each store-driven fire's credential is minted under; absent, each
        /// child inherits `CONTEXTFUL_TOKEN`.
        #[arg(long)]
        issuer_key: Option<PathBuf>,
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
    /// An async-report source over its declared HTTP vendor.
    Report(Box<ReportDeclaration>),
    Derive(Box<(DeriveConfig, Binding, Option<LimiterBinding>)>),
    Component(Box<ComponentSource>),
    Host(Box<HostChecked>),
    /// A source whose rows a sender delivers through `pipeline receive`.
    Receive(contextful_connectors::receive::ReceiveConfig),
}

/// A registered host task, with the store table each of its tables lands in.
pub(crate) struct HostChecked {
    config: DeriveConfig,
    task: Arc<dyn DeriveTask>,
    tables: BTreeMap<String, String>,
    /// The limiter binding the task's `grant` names (`run.bind.reach-grant`).
    limiter: Option<LimiterBinding>,
}

/// The tables each derive pipeline's registered host task joins, which order it after the
/// derive pipelines landing them (`run.select.join-order`).
pub(crate) fn joined_tables(tasks: &Tasks) -> impl Fn(&PipelineSpec) -> Vec<String> + '_ {
    move |spec| {
        let task = (spec.source.name == contextful_connectors::derive::NAME)
            .then(|| spec.source.config.get("task").and_then(serde_json::Value::as_str).and_then(|name| tasks.get(name)))
            .flatten();
        task.map(|t| t.joins().into_iter().map(|j| j.table).collect()).unwrap_or_default()
    }
}

/// The limiter binding a declared seed source's quota names, building that source first: a
/// seed quota the manifest leaves unbound refuses at load (`connector.meter.quota-unbound`).
pub(crate) fn seed_binding(spec: &PipelineSpec, declaration: &Path) -> Result<Option<LimiterBinding>> {
    let Some(seed) = spec.seed_block()? else { return Ok(None) };
    let config = build_source(spec, "seed source", &seed.source)?;
    let Some(d) = &config.limiter else { return Ok(None) };
    let text = std::fs::read_to_string(declaration).unwrap_or_default();
    Ok(Some(require_binding(d, &manifest_bindings(&text)?)?.clone()))
}

/// Hold a specification to every rule checked before I/O, returning its source configuration.
/// A declared seed source is built as a compiled-in HTTP source, so it refuses before a seeding run reaches it.
pub(crate) fn check(spec: &PipelineSpec, declaration: &Path, tasks: &Tasks) -> Result<Checked> {
    spec.validate()?;
    contextful_core::pipeline::normalize::Normalize::parse(spec.normalize.as_ref())?;
    for op in &spec.transforms {
        op.validate()?;
    }
    seed_binding(spec, declaration)?;
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
        contextful_connectors::DRIVE | contextful_connectors::DRIVE_PICKS => {
            contextful_connectors::compiled_in(&spec.source.name).map_err(|why| anyhow::anyhow!("pipeline `{}`: {why}", spec.id))?;
            check_drive(spec)
        }
        contextful_connectors::object::NAME => {
            contextful_connectors::compiled_in(contextful_connectors::object::NAME).map_err(|why| anyhow::anyhow!("pipeline `{}`: {why}", spec.id))?;
            check_object(spec)
        }
        contextful_connectors::file::NAME => check_file(spec),
        contextful_connectors::report::NAME => check_report(spec),
        contextful_connectors::receive::NAME => {
            let config = contextful_connectors::receive::ReceiveConfig::parse(&spec.source.config).with_context(|| format!("pipeline `{}` source", spec.id))?;
            if let Some(field) = &spec.incremental {
                return Err(ConnectorError::ConnectorPositionOwned(format!(
                    "pipeline `{}` declares `incremental = \"{field}\"` beside the `receive` source, whose batch key is its position",
                    spec.id
                ))
                .into());
            }
            if let Some(schedule) = &spec.schedule {
                return Err(RunError::Invalid(format!(
                    "pipeline `{}` declares `schedule = \"{schedule}\"` beside the `receive` source; a sender, not the cadence, starts its runs",
                    spec.id
                ))
                .into());
            }
            Ok(Checked::Receive(config))
        }
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
                // A declared grant binds at load, ahead of any request (`connector.meter.quota-unbound`).
                let limiter = match &config.grant {
                    Some(grant) => {
                        let text = std::fs::read_to_string(declaration).unwrap_or_default();
                        Some(require_binding(grant, &manifest_bindings(&text)?)?.clone())
                    }
                    None => None,
                };
                return Ok(Checked::Host(Box::new(HostChecked { config, task, tables, limiter })));
            }
            let names: Vec<&str> = spec.tables.iter().map(|t| t.name()).collect();
            check_single_output(&spec.id, &names)?;
            let [table] = spec.tables.as_slice() else { unreachable!("one output table passed its check") };
            check_output_table(&TableDecl { name: spec.table_name(table.name()), ..table.decl() })?;
            let text = std::fs::read_to_string(declaration).unwrap_or_default();
            let bindings = bindings(&text)?;
            let binding = bind(&spec.id, &config, &bindings, &declaration.display().to_string())?.clone();
            check_locality(&config.engine, &binding, &config.source_table, &source_zones(declaration, &config.source_table)?)?;
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

/// The zones a derive source table admits: its declaring pipeline's table policy, or the
/// fail-closed pair where no manifest declares the table (`authority.place.fail-closed`).
fn source_zones(declaration: &Path, table: &str) -> Result<contextful_core::place::AllowSet> {
    let files = if declaration.exists() { manifests(declaration)? } else { Vec::new() };
    for d in collect(&files)? {
        if let Some(t) = d.spec.tables.iter().find(|t| d.spec.table_name(t.name()) == table) {
            let policy = contextful_policy::enforce::policy::TablePolicy::from_decl(&d.spec.destination_decl(t))?;
            return Ok(policy.placement.effective());
        }
    }
    Ok(contextful_core::place::AllowSet::fail_closed())
}

/// Print one derive selection's unit counts, calling no engine and landing no row
/// (`run.select.dry-run`).
fn dry_run_counts(spec: &PipelineSpec, checked: &Checked, l: &crate::project::Located, text: &str, author: Option<&crate::admit::Author>, resolver: Arc<contextful_outbound::Resolver>, available: Option<AvailableSelect>) -> Result<()> {
    let decls: Vec<TableDecl> = spec.tables.iter().map(|t| spec.destination_decl(t)).collect();
    let reader = Box::new(StoreReader::open(l, text, decls.clone(), author)?);
    let counts = match checked {
        Checked::Derive(pair) => {
            let table = spec.table_name(spec.tables[0].name());
            let source = DeriveSource {
                pipeline_id: spec.id.clone(),
                config: pair.0.clone(),
                binding: pair.1.clone(),
                output_schema: serde_json::to_value(decls.iter().find(|d| d.name == table).and_then(|d| d.columns.clone()))?,
                output_table: table,
                reader,
                resolver,
                mediation: Mediation::default(),
                store_root: Some(Store::open(&l.project.dir, &l.project.name)?.root().to_path_buf()),
                cwd: l.project.dir.clone(),
                unsettled: None,
                available,
            };
            source.plan()
        }
        Checked::Host(host) => HostDerive {
            pipeline_id: spec.id.clone(),
            config: host.config.clone(),
            task: host.task.clone(),
            tables: host.tables.clone(),
            retention_columns: spec.tables.iter().filter_map(|t| t.decl().retain_rows.map(|r| (t.name().to_string(), r.column))).collect(),
            reader,
            available,
            client: None,
        }
        .plan(),
        _ => bail!("pipeline `{}` reads source `{}`; a dry run counts a derive pipeline's units", spec.id, spec.source.name),
    }
    .map_err(|f| anyhow::anyhow!("pipeline `{}`: {f}", spec.id))?;
    println!("{}", serde_json::json!({ "pipeline": spec.id, "eligible": counts.eligible, "derived": counts.derived, "outstanding": counts.outstanding }));
    Ok(())
}

/// The availability select of a derive pipeline whose parent table declares `available_time`;
/// a bound over a parent declaring none raises `StoreAvailableTimeUndeclared`
/// (`run.select.available-select`).
fn available_select(spec: &PipelineSpec, config: &contextful_core::run::derive::config::DeriveConfig, files: &[contextful_core::pipeline::declare::ManifestFile], bound: Option<contextful_core::store::bound_time::Bound>) -> Result<Option<AvailableSelect>> {
    let mut declared: Vec<TableDecl> = Vec::new();
    for file in files {
        declared.extend(TableDecl::parse_pipeline(&file.text).unwrap_or_default());
    }
    for d in collect(files)? {
        declared.extend(d.spec.tables.iter().map(|t| d.spec.destination_decl(t)));
    }
    let parent = declared.iter().find(|d| d.name == config.source_table);
    let Some(time) = parent.and_then(|d| d.available_time.as_ref()) else {
        if bound.is_some() {
            return Err(contextful_core::store::StoreError::StoreAvailableTimeUndeclared(format!("pipeline `{}` derives from table `{}`, which declares no `available_time`", spec.id, config.source_table)).into());
        }
        return Ok(None);
    };
    let outputs = spec.tables.iter().map(|t| spec.destination_decl(t)).filter_map(|d| d.available_time.map(|a| (d.name.clone(), a.column))).collect();
    Ok(Some(AvailableSelect { bound, parent_column: time.column.clone(), outputs }))
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

/// A `report` source's declaration, checked before any request: the source owns its
/// per-partition position and lands one table.
fn check_report(spec: &PipelineSpec) -> Result<Checked> {
    let declaration = ReportDeclaration::parse(&spec.source.config).with_context(|| format!("pipeline `{}` source", spec.id))?;
    if let Some(field) = &spec.incremental {
        return Err(ConnectorError::ConnectorPositionOwned(format!(
            "pipeline `{}` declares `incremental = \"{field}\"` beside the `report` source, whose position holds each partition's watermark",
            spec.id
        ))
        .into());
    }
    if spec.tables.len() != 1 {
        bail!("pipeline `{}` declares {} tables beside the `report` source, which lands one table", spec.id, spec.tables.len());
    }
    Ok(Checked::Report(Box::new(declaration)))
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
    let config = if spec.source.name == contextful_connectors::DRIVE_PICKS {
        DriveConfig::parse_picks(&spec.source.config)
    } else {
        DriveConfig::parse(&spec.source.config)
    }.with_context(|| format!("pipeline `{}` source", spec.id))?;
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
    use contextful_connectors::object::{ObjectKeys, ObjectSource, Presign, Presigner};
    use contextful_core::run::{Failure, FailureTag};
    use contextful_sync::{S3Bucket, S3Credentials};

    /// A presign no credential can sign fails the read as transient, naming the bucket.
    fn unsigned(bucket: &str, e: contextful_core::store::object::ObjectError) -> Failure {
        Failure::new(FailureTag::Transient, format!("`s3://{bucket}`: {e}"))
    }
    struct Signer(S3Bucket, String);
    impl Presign for Signer {
        fn get(&self, key: &str) -> Result<String, Failure> {
            self.0.presign_get(key).map_err(|e| unsigned(&self.1, e))
        }
        fn head(&self, key: &str) -> Result<String, Failure> {
            self.0.presign_head(key).map_err(|e| unsigned(&self.1, e))
        }
        fn list(&self, prefix: &str, token: Option<&str>) -> Result<String, Failure> {
            self.0.presign_list(prefix, token).map_err(|e| unsigned(&self.1, e))
        }
        fn page(&self, body: &str) -> Result<(Vec<(String, String)>, Option<String>), String> {
            S3Bucket::parse_list(body).map_err(|e| e.to_string())
        }
    }

    let presigner: Presigner = Arc::new(|c: &ObjectConfig, credentials| {
        let endpoint = c.endpoint.as_str();
        let bucket = match credentials {
            Some(ObjectKeys { access_key_id, secret_access_key, session_token }) => {
                S3Bucket::open(endpoint, &c.region, &c.bucket, S3Credentials { access_key_id, secret_access_key, session_token })
            }
            // A role no source answers fails the read before any request (`store.endpoint.role-unavailable`).
            None if c.role => {
                let role = crate::sync::role_provider();
                role.credentials().map_err(|e| Failure::new(FailureTag::Transient, format!("`s3://{}`: SyncRoleUnavailable: {e}", c.bucket)))?;
                S3Bucket::with_role(endpoint, &c.region, &c.bucket, role)
            }
            None => S3Bucket::anonymous(endpoint, &c.region, &c.bucket),
        }
        .map_err(|e| Failure::deterministic(FailureTag::Config, format!("`s3://{}`: {e}", c.bucket)))?;
        Ok(Box::new(Signer(bucket, c.bucket.clone())) as Box<dyn Presign>)
    });
    ObjectSource::signed(config, resolver, presigner)
}

/// The store's landed tables, read for a derive source.
struct StoreReader {
    store: Store,
    decls: Vec<TableDecl>,
    /// Every table the declaration set declares, so a joined table another pipeline owns
    /// reads under its own key.
    declared: Vec<TableDecl>,
    admitted: Option<(contextful_context::read::Face, crate::admit::Author)>,
}

impl StoreReader {
    fn open(located: &crate::project::Located, manifest: &str, decls: Vec<TableDecl>, author: Option<&crate::admit::Author>) -> Result<StoreReader> {
        let admitted = author.map(|author| {
            let pepper = contextful_policy::enforce::mask::Pepper::resolve(|k| std::env::var(k).ok());
            crate::project::open_face(&located.project, &located.declaration, manifest, pepper).map(|face| (face, author.clone()))
        }).transpose()?;
        let declared = TableDecl::parse_declaration_set(manifest, &crate::project::pipeline_files(&located.declaration)?)
            .with_context(|| format!("`{}`", located.declaration.display()))?;
        Ok(StoreReader { store: Store::open(&located.project.dir, &located.project.name)?, decls, declared, admitted })
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
        contextful_context::rows::table_rows(&self.store, &self.decl(table), columns).map_err(|e| store_failure(table, e))
    }

    /// An admitted read answers through the read face, which counts no rows, so it selects in memory.
    fn row_count(&self, table: &str) -> Result<Option<u64>, Failure> {
        if self.admitted.is_some() {
            return Ok(None);
        }
        contextful_context::rows::table_row_count(&self.store, &self.decl(table)).map(Some).map_err(|e| store_failure(table, e))
    }

    fn row_batches(&self, table: &str, columns: &[&str], each: &mut dyn FnMut(Vec<Row>) -> Result<(), Failure>) -> Result<(), Failure> {
        if self.admitted.is_some() {
            return each(self.rows(table, columns)?);
        }
        let mut failed = None;
        let read = contextful_context::rows::table_row_batches(&self.store, &self.decl(table), columns, &mut |rows| {
            each(rows).map_err(|f| {
                let message = f.message.clone();
                failed = Some(f);
                ContextError::Invalid(message)
            })
        });
        match (read, failed) {
            (_, Some(f)) => Err(f),
            (Err(e), None) => Err(store_failure(table, e)),
            (Ok(()), None) => Ok(()),
        }
    }

    /// An admitted read answers through the read face's keyed relation; the store read
    /// keeps one survivor per key and refuses a table with no schema
    /// (`run.select.join-keyed`, `run.select.join-absent`).
    fn latest_rows(&self, table: &str, columns: &[&str]) -> Result<Vec<Row>, Failure> {
        if self.admitted.is_some() {
            return self.rows(table, columns);
        }
        let decl = self.decls.iter().chain(&self.declared).find(|d| d.name == table).cloned().unwrap_or_else(|| TableDecl::named(table));
        if self.store.try_schema(table).map_err(|e| store_failure(table, e))?.is_none() {
            return Err(Failure::deterministic(FailureTag::Config, format!("joined table `{table}` holds no schema; the store has landed no row in it")));
        }
        let read = contextful_context::rows::latest_columns(&decl, columns);
        let read: Vec<&str> = read.iter().map(String::as_str).collect();
        let mut rows = contextful_context::rows::table_rows(&self.store, &decl, &read).map_err(|e| store_failure(table, e))?;
        Ok(contextful_context::rows::latest_survivors(&decl, &rows)
            .into_iter()
            .map(|i| {
                let mut row = std::mem::take(&mut rows[i]);
                row.retain(|c, _| columns.contains(&c.as_str()));
                row
            })
            .collect())
    }
}

impl StoreReader {
    fn decl(&self, table: &str) -> TableDecl {
        self.decls.iter().find(|d| d.name == table).cloned().unwrap_or_else(|| TableDecl::named(table))
    }
}

fn store_failure(table: &str, e: ContextError) -> Failure {
    match e {
        ContextError::ColumnType { .. } => Failure::deterministic(FailureTag::SchemaIncompatible, format!("`{table}`: {e}")),
        e => Failure::new(FailureTag::Storage, e.to_string()),
    }
}

/// Build one source block of `spec` as a compiled-in source and bind every table to it.
pub(crate) fn build_source(spec: &PipelineSpec, role: &str, source: &SourceBlock) -> Result<HttpConfig> {
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
pub(crate) fn built_in(spec: &PipelineSpec) -> ConnectorSpec {
    ConnectorSpec {
        id: spec.source.name.clone(),
        version: env!("CARGO_PKG_VERSION").to_string(),
        world: NATIVE_WORLD.to_string(),
        command: vec![format!("builtin:{}", spec.source.name)],
        dedup_window_secs: None,
    }
}

/// The plan one table of `spec` runs against, pinning `connector`.
pub(crate) fn plan(spec: &PipelineSpec, table: &str, connector: ConnectorSpec) -> Result<Plan> {
    let cursor_kind = if spec.incremental.is_some() { CursorKind::Monotonic } else { CursorKind::OpaqueToken };
    let plan = Plan {
        spec: PlanSpec {
            pipeline: spec.id.clone(),
            table: spec.table_name(table),
            connector,
            cursor: CursorSpec { kind: Some(cursor_kind.name().to_string()), field: spec.incremental.clone(), allowed_lateness: spec.tables.iter().find(|t| t.name() == table).and_then(|t| t.decl().allowed_lateness) },
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
            // Each `[[acl_sweep]]` block parses, its cadences under the budget grammar
            // (`disclosure.sweep.cadence-grammar`), and no source declares two across files.
            let sweeps = contextful_core::disclosure::sweep::SweepBlock::parse_set(&files)?;
            // Each pack loads: no key asserting access outside its mapping schema, no any-of
            // allowlist, no `coarse` without a container signal (`disclosure.pack`).
            let packs = contextful_core::disclosure::pack::Pack::parse_set(&files)?;
            crate::secrets::check_records(&declaration, &files)?;
            let coverage = fold_coverage(&files)?;
            // A job of a kind no fire runs refuses by name (`surface.fire.kind-undispatched`).
            for f in files.iter().filter(|f| f.path.ends_with(".toml")) {
                contextful_core::job::refuse_undispatched(&f.text)?;
            }
            let declared = collect(&files)?;
            for d in &declared {
                let at = || format!("{}:{}", d.file, d.line);
                let checked = check(&d.spec, &declaration, tasks).with_context(at)?;
                check_compaction(&d.spec, &coverage).with_context(at)?;
                let mut discovered = String::new();
                if let Checked::Component(decl) = &checked {
                    crate::connector::admit(decl, &base).with_context(at)?;
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
            let mut resolved = Vec::new();
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
                resolved.push(spec);
            }
            crate::model_source::refuse_cycle(&resolved)?;
            // A binding at a servable level names a declared sweep and a staleness budget
            // (`disclosure.mirror.incomplete-binding`).
            let mut bindings = Vec::new();
            for t in tables.values() {
                bindings.extend(contextful_core::disclosure::declare::Binding::of(t)?);
            }
            contextful_core::disclosure::sweep::check_bindings(&bindings, &sweeps)?;
            // A budget the source's sweep cadence cannot meet (`disclosure.bound-staleness.budget-below-cadence`).
            contextful_core::disclosure::stale::check_cadence(&bindings, &sweeps)?;
            // Each table a pack lands carries a mapping or an exclusion (`disclosure.pack.mapping-absent`).
            for p in &packs {
                p.diagnose()?;
            }
            // On an organization-wide face every table carries a visibility block
            // (`disclosure.mirror.unbound-table`).
            if let Some(root) = files.iter().find(|f| Path::new(&f.path) == declaration.as_path()) {
                let scope = contextful_core::read::face::FaceScope::declared(&root.text).with_context(|| root.path.clone())?;
                contextful_core::disclosure::declare::check_unbound(scope, tables.values())?;
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
        PipelineCmd::Serve { project, declaration, cycle, http, public_key, issuer_key } => crate::cadence::serve(&project, declaration, cycle, http.as_deref(), public_key.as_deref(), issuer_key.as_deref(), tasks, bodies),
        PipelineCmd::Receive { id, key, project, declaration, site_id, site_id_env, admit } => {
            crate::receive::receive(crate::receive::Receive { id, key, project, declaration, site_id, site_id_env, admit: &admit }, tasks)
        }
        PipelineCmd::Worker { project, declaration, listen } => crate::worker::serve_worker(&project, declaration, &listen),
        PipelineCmd::Run { id, project, declaration, run_id, site_id, site_id_env, component_target, admit, applied, dry_run, available_as_of } => {
            fire(Fire { id, project, declaration, run_id, site_id, site_id_env, component_target, admit, applied, dry_run, available_as_of, step: None }, tasks)
        }
        PipelineCmd::Step { id, table, chunk, project, declaration, run_id, site_id, site_id_env, component_target, admit } => {
            let step = Some(StepUnit { table, chunk });
            fire(Fire { id, project, declaration, run_id: Some(run_id), site_id, site_id_env, component_target, admit, applied: None, dry_run: false, available_as_of: None, step }, tasks)
        }
    }
}

/// The unit `pipeline step` names: a table, and optionally a chunk of its seed plan.
struct StepUnit {
    table: Option<String>,
    chunk: Option<u32>,
}

/// One provider step resolved against its pipeline (`run.orchestrate.provider-step`): the
/// destination table, the chunk, and the id the provider replays the step under.
struct Step {
    table: String,
    chunk: Option<u32>,
    id: String,
}

/// Where a provider step stands: committed under an attempt, or due under its next attempt's base.
enum Attempt {
    Done(String),
    Next(String),
}

impl Step {
    fn of(spec: &PipelineSpec, unit: StepUnit, id: String) -> Result<Step> {
        let table = match &unit.table {
            Some(want) => spec
                .tables
                .iter()
                .map(|t| (t.name(), spec.table_name(t.name())))
                .find(|(name, destination)| name == want || destination == want)
                .map(|(_, destination)| destination)
                .with_context(|| format!("pipeline `{}` declares no table `{want}`", spec.id))?,
            None => match spec.tables.as_slice() {
                [t] => spec.table_name(t.name()),
                tables => bail!("pipeline `{}` declares {} tables; `--table` names the step's table", spec.id, tables.len()),
            },
        };
        if unit.chunk.is_some() && spec.seed_block()?.is_none() {
            bail!("table `{table}` of pipeline `{}` holds no chunk plan; `--chunk` runs a chunk of a declared `[pipeline.seed]`", spec.id);
        }
        Ok(Step { table, chunk: unit.chunk, id })
    }

    /// Attempt `k`'s base: the step id, then `<id>.replay-<k>`.
    fn base(&self, k: u32) -> String {
        if k == 1 { self.id.clone() } else { format!("{}.replay-{k}", self.id) }
    }

    /// The run `pipeline run --run-id <base>` names for this step's table or chunk.
    fn run_name(&self, spec: &PipelineSpec, base: &str) -> String {
        let run = if spec.tables.len() == 1 { base.to_string() } else { format!("{base}.{}", self.table) };
        match self.chunk {
            Some(n) => format!("{run}.seed-{n}"),
            None => run,
        }
    }

    /// The commit pointer the step prints: its unit and the run whose commit holds it.
    fn pointer(&self, run: &str) -> String {
        match self.chunk {
            Some(n) => format!("{} chunk {n}: commit {run}", self.table),
            None => format!("{}: commit {run}", self.table),
        }
    }

    /// The pointer of the attempt that committed the step, read from its run row or the table's
    /// committed manifests, else the base of its next attempt. A done chunk whose committing
    /// attempt never closed points at the step's last attempt (`run.orchestrate.resume`). An
    /// attempt still in flight on an unexpired owner lease refuses (`run.orchestrate.step-lease`).
    fn attempt(&self, spec: &PipelineSpec, catalog: &dyn contextful_core::coordinate::Catalog, store: &Store, chunk_done: bool) -> Result<Attempt> {
        let committed: Vec<String> = match store.try_schema(&self.table)? {
            Some(_) => store.committed_runs(&self.table)?.into_iter().map(|m| m.run_id).collect(),
            None => Vec::new(),
        };
        let now = catalog.now()?;
        let (mut k, mut last): (u32, Option<String>) = (1, None);
        loop {
            let base = self.base(k);
            let run = self.run_name(spec, &base);
            let Some(row) = catalog.run(&run)? else {
                return Ok(match last {
                    Some(last) if chunk_done => Attempt::Done(self.pointer(&last)),
                    _ => Attempt::Next(base),
                });
            };
            if row.status == RunStatus::Success || committed.iter().any(|m| commit_of(m, &run)) {
                return Ok(Attempt::Done(self.pointer(&run)));
            }
            if !chunk_done && row.status.is_in_flight() && row.owner.as_ref().is_some_and(|o| !o.expired(now)) {
                bail!("step `{}` runs under attempt `{run}`, whose owner lease has not lapsed", self.id);
            }
            last = Some(run);
            k += 1;
        }
    }
}

/// Whether manifest run id `marker` is a commit of run `run`: its own or a checkpoint's (`run.own.checkpoint-commit`).
fn commit_of(marker: &str, run: &str) -> bool {
    marker == run || marker.strip_prefix(run).and_then(|rest| rest.strip_prefix('.')).is_some_and(|n| n.len() == 6 && n.bytes().all(|b| b.is_ascii_digit()))
}

/// One fire of `pipeline run`, or one provider step of `pipeline step` when `step` names its unit.
struct Fire {
    id: String,
    project: ProjectArgs,
    declaration: Option<PathBuf>,
    run_id: Option<String>,
    site_id: Option<String>,
    site_id_env: Option<String>,
    component_target: ComponentTarget,
    admit: Box<AdmitArgs>,
    applied: Option<u64>,
    dry_run: bool,
    available_as_of: Option<String>,
    step: Option<StepUnit>,
}

fn fire(f: Fire, tasks: &Tasks) -> Result<()> {
    let Fire { id, project, declaration, run_id, site_id, site_id_env, component_target, admit, applied, dry_run, available_as_of, step } = f;
    let l = project.locate(declaration)?;
    crate::sync::pull_before_run(&l)?;
    let declaration = l.declaration.clone();
    let text = if declaration.exists() { std::fs::read_to_string(&declaration)? } else { String::new() };
    let site_id = crate::run::site_id_for(&text, &declaration, site_id, site_id_env)?;
    let files = match applied {
        Some(version) => vec![crate::effective::public_manifest(crate::cadence::snapshot_manifest(&l.project, &text, version)?)?],
        None => manifests(&declaration)?,
    };
    crate::secrets::check_records(&declaration, &files)?;
    let declared: Vec<Declared> = collect(&files)?;
    let d = declared.into_iter().find(|d| d.spec.id == id).with_context(|| match applied {
        Some(v) => format!("applied version v{v} holds no pipeline `{id}`"),
        None => format!("no pipeline `{id}` is declared"),
    })?;
    let spec = d.spec;
    let checked = check(&spec, &declaration, tasks)?;
    if let Checked::Receive(_) = checked {
        bail!("pipeline `{}` reads a `receive` source, whose rows a sender delivers through `pipeline receive`", spec.id);
    }
    let destinations: Vec<String> = spec.tables.iter().map(|t| spec.table_name(t.name())).collect();
    let author = admit.author(project.project.as_deref(), &text, &destinations.iter().map(String::as_str).collect::<Vec<_>>())?;
    check_compaction(&spec, &fold_coverage(&manifests(&declaration)?)?)?;
    let w = wire_at(&l.project, &project.now)?;
    let vars: BTreeMap<String, String> = std::env::vars().collect();
    let assembly = contextful_outbound::secrets::assemble_all(&vars, w.clock.clone())?;
    let resolver = Arc::new(assembly.resolver);
    let credentials = component::Credentials { resolver: resolver.clone(), writers: assembly.writers, clock: w.clock.clone() };
    match &checked {
        Checked::Http(config, _) => resolver.preflight(config.headers.values())?,
        Checked::Report(declaration) => resolver.preflight(declaration.headers.values())?,
        Checked::Component(decl) => component::preflight(decl, crate::connector::admit(decl, &l.project.dir)?.auth.as_ref(), &resolver)?,
        #[cfg(feature = "drive")]
        Checked::Drive(config) => resolver.preflight(config.templates())?,
        Checked::Derive(_) | Checked::Host(_) | Checked::File(_) | Checked::Image(_) | Checked::Receive(_) => {}
        #[cfg(feature = "s3-sync")]
        Checked::Object(config) => resolver.preflight(config.credentials())?,
    }
    let bound = available_as_of.as_deref().map(contextful_core::store::bound_time::Bound::parse).transpose()?;
    let available = match &checked {
        Checked::Derive(pair) => available_select(&spec, &pair.0, &files, bound)?,
        Checked::Host(host) => available_select(&spec, &host.config, &files, bound)?,
        _ if bound.is_some() => bail!("pipeline `{}` reads source `{}`; an availability bound selects a derive pipeline's parents", spec.id, spec.source.name),
        _ => None,
    };
    if dry_run {
        return dry_run_counts(&spec, &checked, &l, &text, author.as_ref(), resolver, available);
    }
    let applied_audit = applied.map(|_| crate::project::read_audit(&l.project, None, None, None)).transpose()?;
    let prior_runs: std::collections::BTreeSet<String> = if applied.is_some() {
        w.engine.catalog.runs(Some(&spec.id))?.into_iter().map(|row| row.run_id).collect()
    } else { Default::default() };

    // A declaration's relative paths resolve against the project directory
    // (`store.init.declaration-base`), whichever subdirectory the command runs from.
    let base = l.project.dir.clone();
    let store = Store::open_declared(&l.project.dir, &l.project.name, &declaration)?;
    // A provider step that already committed prints its pointer before any request
    // (`run.orchestrate.resume`); one that did not runs under its next attempt.
    let step = match step {
        Some(unit) => {
            let step = Step::of(&spec, unit, run_id.clone().unwrap_or_default())?;
            let chunk_done = match step.chunk {
                Some(n) => w.engine.seeding().state(&spec.id, &step.table)?.chunks.iter().any(|c| c.ordinal == n && c.status == contextful_core::run::backfill::ChunkStatus::Done),
                None => false,
            };
            match step.attempt(&spec, w.engine.catalog.as_ref(), &store, chunk_done)? {
                Attempt::Done(pointer) => {
                    println!("{pointer}");
                    return Ok(());
                }
                Attempt::Next(base) => Some((step, base)),
            }
        }
        None => None,
    };
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
    let base_run = step.as_ref().map(|(_, base)| base.clone()).or(run_id).unwrap_or_else(|| format!("run-{}", &sha256_hex(format!("{}{}", w.clock.now().unix_nanos(), std::process::id()).as_bytes())[..12]));
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
        // A task declaring `pure` false and naming its endpoint receives the run's metered
        // client, its requests settling into the marker table's ledger (`run.bind.model-endpoint`).
        let marker = host.tables.get(&host.task.marker_table()).cloned().unwrap_or_else(|| spec.table_name(&host.task.marker_table()));
        let marker_run = if spec.tables.len() == 1 { base_run.clone() } else { format!("{base_run}.{marker}") };
        let ledger = match &host.config.model {
            Some(_) if !host.task.pure() => Some(Arc::new(DeriveLedger::new(dest.store.clone(), marker, dest.node.clone(), marker_run.clone(), w.clock.clone()))),
            _ => None,
        };
        // A grant reserves each request's permit from its shared quota (`run.bind.reach-grant`).
        let limiter = host.limiter.clone().map(|binding| contextful_outbound::Limiter::new(binding, resolver.clone(), &marker_run, w.clock.clone()).map(Arc::new)).transpose()?;
        let meter = host.config.grant.clone().map(|grant| contextful_outbound::Meter::new(grant, limiter.clone()));
        let client = match (&host.config.model, &ledger) {
            (Some(model), Some(ledger)) => Some(Arc::new(HostClient::open(model, ledger.clone(), &marker_run, meter)?) as Arc<dyn MeteredClient>),
            _ => None,
        };
        let derive = HostDerive {
            pipeline_id: spec.id.clone(),
            config: host.config.clone(),
            task: host.task.clone(),
            tables: host.tables.clone(),
            retention_columns: spec.tables.iter().filter_map(|t| {
                t.decl().retain_rows.map(|r| (t.name().to_string(), r.column))
            }).collect(),
            reader: Box::new(StoreReader::open(&l, &text, dest.decls.clone(), dest.author.as_ref())?),
            available: available.clone(),
            client,
        };
        let staged_rows = derive.stage(&Uncanceled);
        // The run's report surrenders every unspent permit (`connector.meter.report-delivery`).
        if let Some(limiter) = &limiter {
            limiter.finish();
            for entry in limiter.audit() {
                eprintln!("{}: {entry}", spec.id);
            }
        }
        // The run's requests settle into its ledger before any table lands, a refused tick's included
        // (`run.journal.ledger-settles-first`).
        if let Some(ledger) = &ledger {
            ledger.finish(None).map_err(|f| anyhow::anyhow!("pipeline `{}`: {f}", spec.id))?;
        }
        let (landing, skipped) = staged_rows.map_err(|f| anyhow::anyhow!("pipeline `{}`: {f}", spec.id))?;
        derive_skipped = skipped;
        order = landing.iter().filter_map(|(table, _)| spec.tables.iter().find(|t| spec.table_name(t.name()) == *table)).collect();
        staged = landing.into_iter().collect();
    }
    let stop_on_failure = matches!(checked, Checked::Host(_)) || spec.on_table_error() == contextful_core::pipeline::declare::OnTableError::Abort;
    let seed = spec.seed_block()?;
    let mut failed: Vec<String> = Vec::new();
    let mut tallies: Vec<(String, IngestTally)> = Vec::new();
    let mut attempted: Vec<String> = Vec::new();
    let mut pointer: Option<String> = None;
    for t in order {
        let table = spec.table_name(t.name());
        // A provider step runs its one table (`run.orchestrate.provider-step`).
        if step.as_ref().is_some_and(|(s, _)| s.table != table) {
            continue;
        }
        // The destination name is path-safe, so a run id built from it is too.
        let run_id = if spec.tables.len() == 1 { base_run.clone() } else { format!("{base_run}.{table}") };
        // A declared seed loads before the table's live pull, and a seed another worker
        // holds defers that pull (`run.seed.one-land-path`).
        if let Some(seed) = &seed {
            let metered = crate::seed::Metered { binding: seed_binding(&spec, &l.declaration)?, resolver: resolver.clone(), clock: w.clock.clone() };
            // A chunk step runs that chunk alone, named as the fire names its run.
            if let Some((s, n)) = step.as_ref().and_then(|(s, _)| s.chunk.map(|n| (s, n))) {
                let run = format!("{run_id}.seed-{n}");
                match crate::seed::step_chunk(&w.engine, &spec, seed, t, &run, n, &site_id, &metered, &mut dest, &mut attempted) {
                    Ok(row) if row.status == RunStatus::Success => {
                        tallies.push((table.clone(), IngestTally::of(&row)));
                        pointer = Some(s.pointer(&run));
                    }
                    Ok(row) => {
                        eprintln!(
                            "{}",
                            RunError::PipelineTableFailed(format!(
                                "chunk {n} of table `{table}` failed as {} in run `{run}`: {}",
                                row.error_kind.map(|k| k.name()).unwrap_or("unknown"),
                                row.error_message.unwrap_or_default()
                            ))
                        );
                        failed.push(run);
                    }
                    Err(e) => {
                        eprintln!("{}", RunError::PipelineTableFailed(format!("chunk {n} of table `{table}` failed in run `{run}`: {e:#}")));
                        failed.push(run);
                    }
                }
                break;
            }
            // A table step pulls live only once its seed committed.
            if step.is_some() && !w.engine.seeding().state(&spec.id, &table)?.committed() {
                bail!("table `{table}` of pipeline `{}` holds an uncommitted seed; a `--chunk` step loads each of its chunks before the live pull", spec.id);
            }
            match crate::seed::drive(&w.engine, &spec, seed, t, &run_id, &site_id, &metered, &mut dest, &mut attempted) {
                Ok(true) => {}
                Ok(false) => {
                    println!("{table}: the seed's chunk runs elsewhere; the live pull waits for its commit");
                    continue;
                }
                Err(e) => {
                    // A failed chunk halts the fire under either `on_table_error`
                    // (`run.declare.chunked-load`).
                    eprintln!("{}", RunError::PipelineTableFailed(format!("table `{table}` failed seeding in run `{run_id}`: {e:#}")));
                    tallies.push((table.clone(), IngestTally { failed: 1, ..IngestTally::default() }));
                    failed.push(run_id);
                    break;
                }
            }
        }
        attempted.push(run_id.clone());
        // A table that cannot open is a failed table like one whose run fails, so
        // `continue` lands the others.
        let mut limiters: Vec<Arc<contextful_outbound::Limiter>> = Vec::new();
        let trace = crate::telemetry::RunTrace::open();
        let outcome = (|| -> Result<contextful_core::run::record::RunRow> {
            let connector = loaded.as_ref().map_or_else(|| built_in(&spec), component::Loaded::connector_spec);
            let plan = plan(&spec, t.name(), connector)?;
            dest.store.validate_source_plan(&plan, dest.normalize)?;
            let connector: ConnectorPin = plan.connector_pin(&artifact);
            // A pending owner's replay runs the component build its pin names, resolved
            // from the admission store; a build that store lacks admits the current one,
            // which the pinned owner then refuses (`run.own.admission-pin`).
            let (connector, recorded) = match (&checked, &loaded) {
                (Checked::Component(decl), Some(c)) => {
                    let admitted = c.artifacts(decl, &base)?;
                    match w.engine.admit_connector(&spec.id, &table, &connector, &admitted) {
                        Ok((pin, wasm)) if pin != connector => {
                            let recorded = c.recorded(decl, &base, &wasm, component_target, dest.store.requires_connector_pin())?;
                            (pin, Some(recorded))
                        }
                        _ => (connector, None),
                    }
                }
                _ => (connector, None),
            };
            let run = RunSpec { plan, connector, run_id: run_id.clone(), site_id: site_id.clone(), pid: std::process::id(), boot_id: boot_id(), trace_id: Some(trace.trace_id()) };
            let shape = Chain { ops: spec.transforms.clone(), table: table.clone() };
            let source: Box<dyn Source> = match &checked {
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
                Checked::Report(declaration) => {
                    let mediation = Mediation { run_id: Some(run_id.clone()), ..Mediation::default() };
                    let vendor = DeclaredVendor::mediated((**declaration).clone(), resolver.clone(), &mediation)?;
                    let today = w.clock.now().to_rfc3339();
                    Box::new(ReportSource::new(declaration.report.clone(), vendor, &today[..10])?)
                }
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
                        unsettled: None,
                        available: available.clone(),
                    })
                }
                Checked::Component(decl) => match recorded.as_ref().or(loaded.as_ref()) {
                    Some(c) => c.source(decl, t.name(), &credentials, &run_id)?,
                    None => bail!("pipeline `{}`: component source `{}` did not load", spec.id, spec.source.name),
                },
                Checked::Host(_) => Box::new(Staged(staged.get(&table).cloned().unwrap_or_default(), derive_skipped)),
                Checked::Receive(_) => bail!("pipeline `{}`: a `receive` source lands only delivered batches", spec.id),
            };
            // The closing row carries the names this fire hydrated and their adapters
            // (`connector.record.provider-attribution`).
            let mut source = Attributed { inner: source, resolver: resolver.clone() };
            Ok(trace.engine(&w.engine).run_with(&run, &mut source, &shape, &mut dest)?)
        })();
        // The run's reports surrender every unspent permit (`connector.meter.report-delivery`).
        for limiter in &limiters {
            limiter.finish();
            for entry in limiter.audit() {
                eprintln!("{table}: {entry}");
            }
        }
        if let Ok(row) = &outcome {
            trace.close(row);
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
            if let Some((s, _)) = &step {
                pointer = Some(s.pointer(&row.run_id));
                continue;
            }
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
    let recorded = (|| -> Result<()> {
        if let (Some(version), Some(audit)) = (applied, applied_audit) {
            let mut rows = Vec::new();
            for id in attempted.iter().filter(|id| !prior_runs.contains(*id)) {
                if let Some(row) = w.engine.catalog.run(id)? { rows.push(row.run_id); }
            }
            crate::source_runs::record(&audit, version, &spec, &base_run, &rows, !failed.is_empty())?;
        }
        Ok(())
    })();
    // A provider step prints its commit pointer alone (`run.orchestrate.provider-step`).
    if step.is_none() {
        println!("tally: {total}");
        for (table, t) in &tallies {
            println!("tally {table}: {t}");
        }
    }
    // Every outcome past run open publishes (`run.record.failure-publishes`).
    let opened = attempted.iter().any(|id| w.engine.catalog.run(id).ok().flatten().is_some());
    // A run that closed without committing drops its mark before the push (`store.drain.mark`).
    let pushed = if opened { crate::sync::unmark_uncommitted(&l, &attempted).and_then(|()| crate::sync::push_after_run(&l, w.clock.now())) } else { Ok(()) };
    if !failed.is_empty() {
        if let Err(error) = recorded { eprintln!("{error:#}"); }
        if let Err(e) = pushed {
            eprintln!("{e:#}");
        }
        bail!("pipeline `{}`: {} table(s) failed: {}", spec.id, failed.len(), failed.join(", "));
    }
    recorded?;
    pushed?;
    if let Some(pointer) = pointer {
        println!("{pointer}");
    }
    Ok(())
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

/// A source reporting its fire's provider attribution from the resolver every credential
/// of the fire hydrates through, an exec step's loopback proxy included
/// (`connector.attach.exec-proxy-attach`).
struct Attributed {
    inner: Box<dyn Source>,
    resolver: Arc<contextful_outbound::Resolver>,
}

impl Source for Attributed {
    fn pull(&mut self, request: &contextful_core::run::ports::PullRequest, cancel: &dyn contextful_core::run::ports::Cancellation) -> Result<Vec<u8>, contextful_core::run::Failure> {
        self.inner.pull(request, cancel)
    }
    fn settle(&mut self) -> Result<(), contextful_core::run::Failure> {
        self.inner.settle()
    }
    fn credential_providers(&self) -> BTreeMap<String, String> {
        self.resolver.attribution()
    }
}
