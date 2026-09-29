//! `contextful pipeline` — declared pipelines on the command line.
//!
//! `run` fires one pipeline once: it reads the manifests, checks the declaration before
//! any I/O, assembles the credential resolver from the process environment, and runs
//! each table through the engine with a built-in source or a component guest.

use crate::component::{self, ComponentTarget};
use crate::run::{boot_id, wire_at, ProjectArgs, StoreDestination};
use anyhow::{bail, Context, Result};
use clap::Subcommand;
use contextful_connectors::derive::{DeriveSource, HostDerive, Staged};
use contextful_connectors::http::{HttpConfig, HttpSource};
use contextful_core::connector::component::ComponentSource;
use contextful_core::connector::ConnectorError;
use contextful_connectors::object::{ObjectConfig, ObjectSource};
use contextful_core::run::derive::config::{bind, bindings, check_output_table, Binding, DeriveConfig};
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
use contextful_core::store::declare::TableDecl;
use contextful_engine::RunSpec;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

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
    },
    /// Check every declared pipeline without network I/O; a local component artifact loads and runs discovery.
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
}

/// The manifest files, in reading order: the project manifest, then `pipelines/` sorted.
fn manifests(declaration: &Path) -> Result<Vec<ManifestFile>> {
    let mut files = Vec::new();
    if declaration.exists() {
        files.push(ManifestFile { path: declaration.display().to_string(), text: std::fs::read_to_string(declaration)? });
    }
    let dir = declaration.parent().map(|p| p.join("pipelines")).unwrap_or_else(|| PathBuf::from("pipelines"));
    if let Ok(entries) = std::fs::read_dir(&dir) {
        let mut paths: Vec<PathBuf> =
            entries.flatten().map(|e| e.path()).filter(|p| p.extension().is_some_and(|x| x == "toml" || x == "json")).collect();
        paths.sort();
        for p in paths {
            let rel = p.strip_prefix(".").unwrap_or(&p).display().to_string();
            files.push(ManifestFile { path: rel, text: std::fs::read_to_string(&p)? });
        }
    }
    Ok(files)
}

/// A pipeline's source, checked and ready to build.
enum Checked {
    Http(HttpConfig),
    #[cfg(feature = "drive")]
    Drive(contextful_connectors::drive::DriveConfig),
    Object(ObjectConfig),
    Derive(Box<(DeriveConfig, Binding)>),
    Component(Box<ComponentSource>),
    Host(Box<HostChecked>),
}

/// A registered host task, with the store table each of its tables lands in.
struct HostChecked {
    config: DeriveConfig,
    task: Arc<dyn DeriveTask>,
    tables: BTreeMap<String, String>,
}

/// Hold a specification to every rule checked before I/O, returning its source configuration.
/// A declared seed source is built as a compiled-in HTTP source, so it refuses before a seeding run reaches it.
fn check(spec: &PipelineSpec, declaration: &Path, tasks: &Tasks) -> Result<Checked> {
    spec.validate()?;
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
            Ok(Checked::Http(config))
        }
        contextful_connectors::DRIVE => {
            contextful_connectors::compiled_in(contextful_connectors::DRIVE).map_err(|why| anyhow::anyhow!("pipeline `{}`: {why}", spec.id))?;
            check_drive(spec)
        }
        contextful_connectors::object::NAME => {
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
        contextful_connectors::derive::NAME => {
            let config = DeriveConfig::parse_with(&spec.id, &spec.source.config, tasks)?;
            if let Some(task) = tasks.get(config.task.name()).filter(|_| config.task.is_host()) {
                let decls: Vec<TableDecl> = spec.tables.iter().map(|t| t.decl()).collect();
                check_host_tables(&spec.id, config.task.name(), task.as_ref(), &decls)?;
                let tables = spec.tables.iter().map(|t| (t.name().to_string(), spec.table_name(t.name()))).collect();
                return Ok(Checked::Host(Box::new(HostChecked { config, task, tables })));
            }
            let [table] = spec.tables.as_slice() else {
                bail!("derive pipeline `{}` declares {} tables; it writes one output table", spec.id, spec.tables.len());
            };
            check_output_table(&TableDecl { name: spec.table_name(table.name()), ..table.decl() })?;
            let text = std::fs::read_to_string(declaration).unwrap_or_default();
            let bindings = bindings(&text)?;
            let binding = bind(&spec.id, &config, &bindings, &declaration.display().to_string())?.clone();
            Ok(Checked::Derive(Box::new((config, binding))))
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

/// The store's landed tables, read for a derive source.
struct StoreReader {
    store: Store,
    decls: Vec<TableDecl>,
}

impl TableReader for StoreReader {
    fn rows(&self, table: &str, columns: &[&str]) -> Result<Vec<Row>, Failure> {
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
            journal: spec.source.name != contextful_connectors::derive::NAME,
            redact: spec.redaction.iter().map(|_| "declared".to_string()).collect(),
        },
        content_hash: spec.content_hash(),
        cursor_kind,
        schedule: Schedule::default(),
    };
    plan.validate()?;
    Ok(plan)
}

/// The directory `pipeline run` resolves a declaration's relative paths against
/// (`store.init.declaration-base`): the located project's, or the working directory when
/// discovery names no project.
fn declaration_base(project: Option<&str>) -> Result<PathBuf> {
    match crate::project::locate(project, None) {
        Ok(l) => Ok(l.project.dir),
        Err(_) if project.is_none() => Ok(std::env::current_dir()?),
        Err(e) => Err(e),
    }
}

pub fn run(cmd: PipelineCmd, tasks: &Tasks) -> Result<()> {
    match cmd {
        PipelineCmd::Validate { declaration, project, component_target } => {
            let files = manifests(&declaration)?;
            let base = declaration_base(project.as_deref())?;
            // Store tables declared under `[pipeline]` answer to the same load checks the
            // store and the read face run.
            for f in files.iter().filter(|f| f.path.ends_with(".toml")) {
                TableDecl::parse_pipeline(&f.text).with_context(|| f.path.clone())?;
            }
            for d in collect(&files)? {
                let checked = check(&d.spec, &declaration, tasks).with_context(|| format!("{}:{}", d.file, d.line))?;
                let mut discovered = String::new();
                if let Checked::Component(decl) = &checked {
                    if component::is_local(decl) {
                        let loaded = component::load(&d.spec.source.name, decl, &base, component_target)
                            .with_context(|| format!("{}:{}", d.file, d.line))?;
                        let names = loaded.discover(decl).with_context(|| format!("{}:{}", d.file, d.line))?;
                        discovered = format!(" · discovers {}", names.join(", "));
                    }
                }
                println!("{}: valid ({} tables, content hash {}){discovered}", d.spec.id, d.spec.tables.len(), &d.spec.content_hash()[..16]);
            }
            Ok(())
        }
        PipelineCmd::Run { id, project, declaration, run_id, site_id, site_id_env, component_target } => {
            let l = project.locate(declaration)?;
            crate::sync::pull_before_run(&l)?;
            let declaration = l.declaration.clone();
            let text = if declaration.exists() { std::fs::read_to_string(&declaration)? } else { String::new() };
            let site_id = crate::run::site_id_for(&text, &declaration, site_id, site_id_env)?;
            let declared: Vec<Declared> = collect(&manifests(&declaration)?)?;
            let d = declared.into_iter().find(|d| d.spec.id == id).with_context(|| format!("no pipeline `{id}` is declared"))?;
            let spec = d.spec;
            let checked = check(&spec, &declaration, tasks)?;
            let w = wire_at(&l.project, &project.now)?;
            let vars: BTreeMap<String, String> = std::env::vars().collect();
            let resolver = Arc::new(contextful_outbound::assemble(&vars, w.clock.clone())?);
            match &checked {
                Checked::Http(config) => resolver.preflight(config.headers.values())?,
                Checked::Component(decl) => resolver.preflight(decl.attach.iter().map(|(_, t)| t))?,
                #[cfg(feature = "drive")]
                Checked::Drive(config) => resolver.preflight(config.templates())?,
                Checked::Derive(_) | Checked::Host(_) => {}
                Checked::Object(config) => resolver.preflight(config.credentials())?,
            }

            // A declaration's relative paths resolve against the project directory
            // (`store.init.declaration-base`), whichever subdirectory the command runs from.
            let base = l.project.dir.clone();
            // A component resolves, admits and compiles once per fire, before any run row.
            let loaded = match &checked {
                Checked::Component(decl) => Some(component::load(&spec.source.name, decl, &base, component_target)?),
                _ => None,
            };
            // One drive fire mints one token and walks the tree once for every table it lands.
            #[cfg(feature = "drive")]
            let drive = match &checked {
                Checked::Drive(config) => {
                    // PDF bodies decode in this binary's worker, behind the process boundary.
                    let worker = contextful_connectors::boundary::Boundary::new(std::env::current_exe()?, &["decode", "pdf"]);
                    Some(contextful_connectors::drive::Drive::new(config.clone(), resolver.clone(), Arc::new(worker))?)
                }
                _ => None,
            };
            let store = Store::open(&l.project.dir, &l.project.name)?;
            let (node, _) = node::resolve(&store, |k| std::env::var(k).ok())?;
            let decls: Vec<TableDecl> = spec
                .tables
                .iter()
                .map(|t| {
                    let mut decl = t.decl();
                    decl.name = spec.table_name(t.name());
                    decl
                })
                .collect();
            let mut dest = StoreDestination { store, decls, node };
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
            let mut order: Vec<&contextful_core::pipeline::declare::TableEntry> = spec.tables.iter().collect();
            if let Checked::Host(host) = &checked {
                let derive = HostDerive {
                    pipeline_id: spec.id.clone(),
                    config: host.config.clone(),
                    task: host.task.clone(),
                    tables: host.tables.clone(),
                    reader: Box::new(StoreReader { store: Store::open(&l.project.dir, &l.project.name)?, decls: dest.decls.clone() }),
                };
                let landing = derive.stage(&Uncanceled).map_err(|f| anyhow::anyhow!("pipeline `{}`: {f}", spec.id))?;
                order = landing.iter().filter_map(|(table, _)| spec.tables.iter().find(|t| spec.table_name(t.name()) == *table)).collect();
                staged = landing.into_iter().collect();
            }
            let stop_on_failure = matches!(checked, Checked::Host(_)) || spec.on_table_error() == contextful_core::pipeline::declare::OnTableError::Abort;
            let mut failed: Vec<String> = Vec::new();
            for t in order {
                let table = spec.table_name(t.name());
                // The destination name is path-safe, so a run id built from it is too.
                let run_id = if spec.tables.len() == 1 { base_run.clone() } else { format!("{base_run}.{table}") };
                // A table that cannot open is a failed table like one whose run fails, so
                // `continue` lands the others.
                let outcome = (|| -> Result<contextful_core::run::record::RunRow> {
                    let connector = loaded.as_ref().map_or_else(|| built_in(&spec), component::Loaded::connector_spec);
                    let plan = plan(&spec, t.name(), connector)?;
                    let connector: ConnectorPin = plan.connector_pin(&artifact);
                    let run = RunSpec { plan, connector, run_id: run_id.clone(), site_id: site_id.clone(), pid: std::process::id(), boot_id: boot_id(), trace_id: None };
                    let shape = Chain { ops: spec.transforms.clone(), table: table.clone() };
                    let mut source: Box<dyn Source> = match &checked {
                        Checked::Http(config) => Box::new(HttpSource::new(config.clone(), t.name(), resolver.clone())?),
                        #[cfg(feature = "drive")]
                        Checked::Drive(_) => match &drive {
                            Some(d) => Box::new(d.source(t.name())?),
                            None => bail!("pipeline `{}`: the drive source did not open", spec.id),
                        },
                        Checked::Object(config) => Box::new(ObjectSource::signed(config.clone(), resolver.clone())),
                        Checked::Derive(pair) => Box::new(DeriveSource {
                            pipeline_id: spec.id.clone(),
                            config: pair.0.clone(),
                            binding: pair.1.clone(),
                            output_table: table.clone(),
                            output_schema: serde_json::to_value(dest.decls.iter().find(|d| d.name == table).and_then(|d| d.columns.clone()))?,
                            reader: Box::new(StoreReader { store: Store::open(&l.project.dir, &l.project.name)?, decls: dest.decls.clone() }),
                            resolver: resolver.clone(),
                            cwd: base.clone(),
                        }),
                        Checked::Component(decl) => match &loaded {
                            Some(c) => c.source(decl, t.name(), &resolver, &run_id)?,
                            None => bail!("pipeline `{}`: component source `{}` did not load", spec.id, spec.source.name),
                        },
                        Checked::Host(_) => Box::new(Staged(staged.get(&table).cloned().unwrap_or_default())),
                    };
                    Ok(w.engine.run_with(&run, &mut source, &shape, &mut dest)?)
                })();
                let row = match outcome {
                    Ok(row) => row,
                    Err(e) => {
                        eprintln!("{}", RunError::PipelineTableFailed(format!("table `{table}` failed as refused in run `{run_id}`: {e:#}")));
                        failed.push(run_id);
                        if stop_on_failure {
                            break;
                        }
                        continue;
                    }
                };
                if row.status == RunStatus::Success {
                    let skipped = if row.skipped > 0 { format!(" · {} skipped", row.skipped) } else { String::new() };
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
            if !failed.is_empty() {
                bail!("pipeline `{}`: {} table(s) failed: {}", spec.id, failed.len(), failed.join(", "));
            }
            Ok(())
        }
    }
}
