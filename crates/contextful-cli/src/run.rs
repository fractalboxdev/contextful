//! `contextful run` — the run-path surface of the command line.
//!
//! Each subcommand wires the engine to this process: the local catalog, journal and
//! awakeable registry under `.contextful/run/<project>/`, a command source, and the
//! store as the destination. No run rule lives here.

use anyhow::{bail, Context, Result};
use clap::Subcommand;
use contextful_context::land::{land_batches, Batch, Position, RunContext};
use contextful_context::{node, ContextError, Store};
use contextful_core::ports::{Clock, FixedClock};
use contextful_core::run::cancel::Scope;
use contextful_core::run::journal::sha256_hex;
use contextful_core::run::plan::Plan;
use contextful_core::run::ports::{Commit, Destination, Landed, Marker};
use contextful_core::run::record::{describe_ceiling, export_ceiling, parse_bound, resolve_site_id, select_history, RunStatus, SiteIdSources, Window};
use contextful_core::run::{Failure, FailureTag};
use contextful_core::store::declare::TableDecl;
use contextful_core::store::reserve::Injection;
use contextful_core::store::StoreError;
use contextful_core::time::Instant;
use contextful_engine::awake::{AwakeError, Registry};
use contextful_engine::cancel::Cadence;
use contextful_engine::command::CommandSource;
use contextful_engine::{Engine, Journal, LocalCatalog, RunSpec};
use std::path::{Path, PathBuf};
use std::sync::Arc;

#[derive(clap::Args)]
pub struct ProjectArgs {
    /// The project whose store root is `.contextful/context/<project>/` and whose run
    /// state is `.contextful/run/<project>/`.
    #[arg(long)]
    project: String,
    /// The evaluation instant (RFC 3339); absent reads the system clock.
    #[arg(long)]
    now: Option<String>,
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
        /// The pipeline manifest holding the `[[pipeline.tables]]` declarations.
        #[arg(long, default_value = "contextful.toml")]
        declaration: PathBuf,
        /// The catalog run id of this attempt; absent mints one.
        #[arg(long)]
        run_id: Option<String>,
        /// The site id, declared on the command line.
        #[arg(long)]
        site_id: Option<String>,
        /// The environment variable holding the site id.
        #[arg(long)]
        site_id_env: Option<String>,
    },
    /// Print a run row as JSON.
    Show {
        run_id: String,
        #[command(flatten)]
        project: ProjectArgs,
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

/// The system clock.
struct SystemClock;

impl Clock for SystemClock {
    fn now(&self) -> Instant {
        let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or_default();
        Instant::from_unix_nanos(i128::try_from(nanos).unwrap_or_default()).unwrap_or_else(|_| FixedClock(Instant::from_unix_secs(0).expect("the epoch is an instant")).now())
    }
}

fn clock(now: &Option<String>) -> Result<Arc<dyn Clock + Send + Sync>> {
    Ok(match now {
        Some(s) => Arc::new(FixedClock(Instant::parse(s)?)),
        None => Arc::new(SystemClock),
    })
}

struct Wired {
    engine: Engine,
    registry: Registry,
    clock: Arc<dyn Clock + Send + Sync>,
}

fn wire(args: &ProjectArgs) -> Result<Wired> {
    let root = std::env::current_dir()?.join(".contextful").join("run").join(&args.project);
    let clock = clock(&args.now)?;
    let journal = Journal::open(&root);
    let catalog = Arc::new(LocalCatalog::open(&root, clock.clone()));
    let registry = Registry::open(&root, journal.clone());
    Ok(Wired { engine: Engine { catalog, journal, cadence: Cadence::default(), emitter: None }, registry, clock })
}

/// The boot identity of this machine: a process id means nothing across boots.
fn boot_id() -> String {
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
struct StoreDestination {
    store: Store,
    decls: Vec<TableDecl>,
    node: contextful_core::store::lay_out::NodeId,
}

fn store_failure(e: ContextError) -> Failure {
    match e {
        ContextError::Store(StoreError::StoreSchemaIncompatible(m)) => Failure::deterministic(FailureTag::SchemaIncompatible, format!("StoreSchemaIncompatible: {m}")),
        ContextError::Store(s) => Failure::deterministic(FailureTag::Permanent, s.to_string()),
        other => Failure::new(FailureTag::Storage, other.to_string()),
    }
}

impl Destination for StoreDestination {
    fn land(&mut self, commit: Commit, precommit: &dyn Fn() -> Result<(), Failure>) -> Result<Landed, Failure> {
        let decl = self.decls.iter().find(|d| d.name == commit.table).cloned().unwrap_or_else(|| TableDecl::named(&commit.table));
        let rows: u64 = commit.batches.iter().map(|b| b.len() as u64).sum();
        let batches: Vec<Batch> = commit.batches.into_iter().map(|rows| Batch { rows, types: Default::default() }).collect();
        let ctx = RunContext {
            node: self.node.clone(),
            injection: Injection { run_id: commit.run_id.clone(), site_id: commit.site_id.clone(), batch_seq: None, authored_by: None },
            committed_at: commit.committed_at,
        };
        let position = Position { pipeline_id: Some(commit.pipeline_id.clone()), cursor: commit.cursor.clone(), fence: commit.fence };
        let precommit = || precommit().map_err(|f| ContextError::Invalid(f.to_string()));
        let manifest = land_batches(&self.store, &decl, &batches, &ctx, &position, &precommit).map_err(store_failure)?;
        let dir = self.store.table_dir(&commit.table).map_err(store_failure)?.join("data").join("runs").join(&commit.run_id).join(&manifest.node_id);
        let mut bytes = 0;
        for p in &manifest.parts {
            bytes += std::fs::metadata(dir.join(&p.name)).map(|m| m.len()).map_err(|e| Failure::new(FailureTag::Storage, e.to_string()))?;
        }
        Ok(Landed { rows, bytes })
    }

    fn newest_marker(&self, pipeline_id: &str, table: &str) -> Result<Option<Marker>, Failure> {
        if self.store.try_schema(table).map_err(store_failure)?.is_none() {
            return Ok(None);
        }
        let runs = self.store.committed_runs(table).map_err(store_failure)?;
        Ok(runs
            .into_iter()
            .filter(|m| m.pipeline_id.as_deref() == Some(pipeline_id))
            .max_by(|a, b| a.committed_at.cmp(&b.committed_at).then_with(|| a.run_id.cmp(&b.run_id)))
            .map(|m| Marker { run_id: m.run_id, cursor: m.cursor, committed_at: m.committed_at }))
    }
}

fn awake_error(e: AwakeError) -> anyhow::Error {
    match e {
        AwakeError::Refused(r) => r.into(),
        AwakeError::Storage(f) => f.into(),
    }
}

pub fn run(cmd: RunCmd) -> Result<()> {
    match cmd {
        RunCmd::Start { project, plan, declaration, run_id, site_id, site_id_env } => {
            let sources = SiteIdSources { manifest: site_id, env: site_id_env.map(|v| { let value = std::env::var(&v).ok(); (v, value) }) };
            let site_id = resolve_site_id(&sources)?;
            let bytes = std::fs::read(&plan).with_context(|| format!("reading the plan `{}`", plan.display()))?;
            let plan = Plan::compile(&bytes).with_context(|| format!("`{}`", plan.display()))?;
            let cwd = std::env::current_dir()?;
            let text = std::fs::read_to_string(&declaration).with_context(|| format!("reading the declaration `{}`", declaration.display()))?;
            let decls = TableDecl::parse_pipeline(&text).with_context(|| format!("`{}`", declaration.display()))?;
            let store = Store::open(&cwd, &project.project)?;
            let (node, _) = node::resolve(&store, |k| std::env::var(k).ok())?;
            let w = wire(&project)?;
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
            let spec = RunSpec { connector, plan: plan.clone(), run_id, site_id, pid: std::process::id(), boot_id: boot_id(), trace_id: None };
            let mut source = CommandSource { argv: plan.spec.connector.command.clone(), cwd };
            let mut dest = StoreDestination { store, decls, node };
            let row = w.engine.run(&spec, &mut source, &mut dest)?;
            if row.status == RunStatus::Success {
                println!("{}: success · {} rows in {} batches", row.run_id, row.rows, row.batches);
                Ok(())
            } else {
                bail!("{}: {} — {}", row.run_id, row.status, row.error_message.unwrap_or_default())
            }
        }
        RunCmd::Show { run_id, project } => {
            let w = wire(&project)?;
            let row = w.engine.catalog.run(&run_id)?.with_context(|| format!("no run `{run_id}`"))?;
            println!("{}", serde_json::to_string_pretty(&row)?);
            Ok(())
        }
        RunCmd::Cancel { run_id, project, scope, reason } => {
            let w = wire(&project)?;
            let scope = match scope {
                StopScope::Run => Scope::Run,
                StopScope::Pipeline => Scope::Pipeline,
            };
            for id in w.engine.cancel(&run_id, scope, reason)? {
                println!("{id}: stop requested");
            }
            Ok(())
        }
        RunCmd::History { project, pipelines, since, limit, export } => {
            let w = wire(&project)?;
            let since = since.map(|s| parse_bound(&s)).transpose()?;
            let ceiling = if export { export_ceiling(limit) } else { describe_ceiling(limit) };
            let window = Window { since, ceiling };
            let names: Vec<Option<&str>> = if pipelines.is_empty() { vec![None] } else { pipelines.iter().map(|p| Some(p.as_str())).collect() };
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
            if export {
                let header = serde_json::json!({ "store": project.project, "window": window, "count": page.runs.len(), "truncated": truncated });
                println!("{header}");
                for r in &page.runs {
                    println!("{}", serde_json::to_string(r)?);
                }
            } else {
                println!("{}", serde_json::to_string_pretty(&serde_json::json!({ "window": window, "runs": page.runs, "truncated": truncated }))?);
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
