//! `contextful pipeline` — declared pipelines on the command line.
//!
//! `run` fires one pipeline once: it reads the manifests, checks the declaration before
//! any I/O, assembles the credential resolver from the process environment, and runs
//! each table through the engine with the built-in source behind the secret guard.

use crate::run::{boot_id, wire_at, ProjectArgs, StoreDestination};
use anyhow::{bail, Context, Result};
use clap::Subcommand;
use contextful_connectors::derive::DeriveSource;
use contextful_connectors::http::{HttpConfig, HttpSource};
use contextful_core::run::derive::config::{bind, bindings, check_output_table, Binding, DeriveConfig};
use contextful_core::run::ports::{Row, Source, TableReader};
use contextful_core::run::{Failure, FailureTag};
use contextful_context::{node, ContextError, Store};
use contextful_core::pipeline::declare::{collect, Declared, ManifestFile, PipelineSpec, SourceBlock};
use contextful_core::pipeline::transform::Chain;
use contextful_core::run::advance::CursorKind;
use contextful_core::run::journal::sha256_hex;
use contextful_core::run::own::ConnectorPin;
use contextful_core::run::plan::{ConnectorSpec, CursorSpec, Plan, PlanSpec, NATIVE_WORLD};
use contextful_core::run::record::{resolve_site_id, RunStatus, SiteIdSources};
use contextful_core::run::retry::Schedule;
use contextful_core::run::RunError;
use contextful_core::store::declare::TableDecl;
use contextful_engine::guard::{log_counts, Guarded};
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
        #[arg(long)]
        site_id: Option<String>,
        #[arg(long)]
        site_id_env: Option<String>,
    },
    /// Check every declared pipeline without I/O.
    Validate {
        #[arg(long, default_value = "contextful.toml")]
        declaration: PathBuf,
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
    Derive(Box<(DeriveConfig, Binding)>),
}

/// Hold a specification to every rule checked before I/O, returning its source configuration.
/// A declared seed source is built as a compiled-in HTTP source, so it refuses before a seeding run reaches it.
fn check(spec: &PipelineSpec, declaration: &Path) -> Result<Checked> {
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
        contextful_connectors::derive::NAME => {
            let config = DeriveConfig::parse(&spec.id, &spec.source.config)?;
            let [table] = spec.tables.as_slice() else {
                bail!("derive pipeline `{}` declares {} tables; it writes one output table", spec.id, spec.tables.len());
            };
            check_output_table(&TableDecl { name: spec.table_name(table.name()), ..table.decl() })?;
            let text = std::fs::read_to_string(declaration).unwrap_or_default();
            let bindings = bindings(&text)?;
            let binding = bind(&spec.id, &config, &bindings, &declaration.display().to_string())?.clone();
            Ok(Checked::Derive(Box::new((config, binding))))
        }
        other => bail!("pipeline `{}` names source `{other}`; the compiled-in sources are {}", spec.id, contextful_connectors::BUILT_IN.join(", ")),
    }
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

/// The plan one table of `spec` runs against.
fn plan(spec: &PipelineSpec, table: &str) -> Result<Plan> {
    let cursor_kind = if spec.incremental.is_some() { CursorKind::Monotonic } else { CursorKind::OpaqueToken };
    let plan = Plan {
        spec: PlanSpec {
            pipeline: spec.id.clone(),
            table: spec.table_name(table),
            connector: ConnectorSpec {
                id: spec.source.name.clone(),
                version: env!("CARGO_PKG_VERSION").to_string(),
                world: NATIVE_WORLD.to_string(),
                command: vec![format!("builtin:{}", spec.source.name)],
            },
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

pub fn run(cmd: PipelineCmd) -> Result<()> {
    match cmd {
        PipelineCmd::Validate { declaration } => {
            let files = manifests(&declaration)?;
            // Store tables declared under `[pipeline]` answer to the same load checks the
            // store and the read face run.
            for f in files.iter().filter(|f| f.path.ends_with(".toml")) {
                TableDecl::parse_pipeline(&f.text).with_context(|| f.path.clone())?;
            }
            for d in collect(&files)? {
                check(&d.spec, &declaration).with_context(|| format!("{}:{}", d.file, d.line))?;
                println!("{}: valid ({} tables, content hash {})", d.spec.id, d.spec.tables.len(), &d.spec.content_hash()[..16]);
            }
            Ok(())
        }
        PipelineCmd::Run { id, project, declaration, run_id, site_id, site_id_env } => {
            let site_id = resolve_site_id(&SiteIdSources { manifest: site_id, env: site_id_env.map(|v| { let value = std::env::var(&v).ok(); (v, value) }) })?;
            let l = project.locate(declaration)?;
            let declaration = l.declaration.clone();
            let declared: Vec<Declared> = collect(&manifests(&declaration)?)?;
            let d = declared.into_iter().find(|d| d.spec.id == id).with_context(|| format!("no pipeline `{id}` is declared"))?;
            let spec = d.spec;
            let checked = check(&spec, &declaration)?;
            let w = wire_at(&l.project, &project.now)?;
            let vars: BTreeMap<String, String> = std::env::vars().collect();
            let resolver = Arc::new(contextful_outbound::assemble(&vars, w.clock.clone())?);
            if let Checked::Http(config) = &checked {
                resolver.preflight(config.headers.values())?;
            }

            let cwd = std::env::current_dir()?;
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
            let artifact = sha256_hex(serde_json::to_string(&spec.source).unwrap_or_default().as_bytes());
            let mut failed: Vec<String> = Vec::new();
            for t in &spec.tables {
                let table = spec.table_name(t.name());
                // The destination name is path-safe, so a run id built from it is too.
                let run_id = if spec.tables.len() == 1 { base_run.clone() } else { format!("{base_run}.{table}") };
                // A table that cannot open is a failed table like one whose run fails, so
                // `continue` lands the others.
                let outcome = (|| -> Result<contextful_core::run::record::RunRow> {
                    let plan = plan(&spec, t.name())?;
                    let connector: ConnectorPin = plan.connector_pin(&artifact);
                    let run = RunSpec { plan, connector, run_id: run_id.clone(), site_id: site_id.clone(), pid: std::process::id(), boot_id: boot_id(), trace_id: None };
                    let shape = Chain { ops: spec.transforms.clone(), table: table.clone() };
                    let inner: Box<dyn Source> = match &checked {
                        Checked::Http(config) => Box::new(HttpSource::new(config.clone(), t.name(), resolver.clone())?),
                        Checked::Derive(pair) => Box::new(DeriveSource {
                            pipeline_id: spec.id.clone(),
                            config: pair.0.clone(),
                            binding: pair.1.clone(),
                            output_table: table.clone(),
                            output_schema: serde_json::to_value(dest.decls.iter().find(|d| d.name == table).and_then(|d| d.columns.clone()))?,
                            reader: Box::new(StoreReader { store: Store::open(&l.project.dir, &l.project.name)?, decls: dest.decls.clone() }),
                            resolver: resolver.clone(),
                            cwd: cwd.clone(),
                        }),
                    };
                    let mut source = Guarded { inner, report: log_counts };
                    Ok(w.engine.run_with(&run, &mut source, &shape, &mut dest)?)
                })();
                let row = match outcome {
                    Ok(row) => row,
                    Err(e) => {
                        eprintln!("{}", RunError::PipelineTableFailed(format!("table `{table}` failed as refused in run `{run_id}`: {e:#}")));
                        failed.push(run_id);
                        if spec.on_table_error() == contextful_core::pipeline::declare::OnTableError::Abort {
                            break;
                        }
                        continue;
                    }
                };
                if row.status == RunStatus::Success {
                    println!("{table}: {} success · {} rows in {} batches", row.run_id, row.rows, row.batches);
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
                if spec.on_table_error() == contextful_core::pipeline::declare::OnTableError::Abort {
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
