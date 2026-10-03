//! `contextful job` — the manifest's `[[job]]` blocks on the command line.
//!
//! `validate` holds every block to the closed kind union and the row bodies the binary
//! registers. `fire` runs one store-driven job once: it admits the job's credential, reads
//! the input statement through the read face under it at the job's `as_of`, runs the
//! registered body per row through the engine, and lands the emitted rows, one table run
//! per output table.

use crate::admit::{face, AdmitArgs};
use crate::run::{boot_id, site_id_for, wire_at, ProjectArgs, StoreDestination};
use anyhow::{bail, Context, Result};
use clap::Subcommand;
use contextful_connectors::derive::Staged;
use contextful_context::{node, Store};
use contextful_core::job::{bind_targets, parse_jobs, Job, JobKind, StoreDriven, Targets};
use contextful_core::run::advance::CursorKind;
use contextful_core::run::drive::{Bodies, Emitted, InputSet};
use contextful_core::run::journal::sha256_hex;
use contextful_core::run::plan::{ConnectorSpec, CursorSpec, Plan, PlanSpec, NATIVE_WORLD};
use contextful_core::run::ports::{Landed, Source, Unshaped};
use contextful_core::run::record::RunStatus;
use contextful_core::run::retry::Schedule;
use contextful_core::run::{Failure, FailureTag};
use contextful_core::store::bound_time::{Bound, Bounds};
use contextful_core::store::declare::TableDecl;
use contextful_engine::drive::{Drive, PARKED_POLL};
use contextful_engine::RunSpec;
use contextful_policy::enforce::session::Request;
use std::path::PathBuf;

#[derive(Subcommand)]
pub enum JobCmd {
    /// Check every `[[job]]` block against the kind union and the registered row bodies.
    Validate {
        #[arg(long, default_value = "contextful.toml")]
        declaration: PathBuf,
    },
    /// Fire one store-driven job once, resuming its pending execution when one holds.
    Fire {
        name: String,
        #[command(flatten)]
        project: ProjectArgs,
        /// The project manifest holding the job block; absent, the project's `contextful.toml`.
        #[arg(long)]
        declaration: Option<PathBuf>,
        /// The run id of this attempt; each output table's run suffixes it with the table name.
        #[arg(long)]
        run_id: Option<String>,
        /// The site id of this fire; replaces the manifest's `site_id` or `site_id_env`.
        #[arg(long)]
        site_id: Option<String>,
        /// The environment variable holding this fire's site id; replaces the manifest's declaration.
        #[arg(long)]
        site_id_env: Option<String>,
        #[command(flatten)]
        admit: AdmitArgs,
    },
}

/// The plan one output table of a store-driven job lands under.
fn output_plan(job: &str, table: &str, driven: &StoreDriven) -> Result<Plan> {
    let plan = Plan {
        spec: PlanSpec {
            pipeline: job.to_string(),
            table: table.to_string(),
            connector: ConnectorSpec {
                id: contextful_core::job::STORE_DRIVEN.to_string(),
                version: env!("CARGO_PKG_VERSION").to_string(),
                world: NATIVE_WORLD.to_string(),
                command: vec![format!("builtin:{}", contextful_core::job::STORE_DRIVEN)],
            },
            cursor: CursorSpec { kind: Some(CursorKind::OpaqueToken.name().to_string()), field: None },
            retry: None,
            // The rows are the body's recorded output; the land re-derives them on a resume.
            journal: false,
            redact: Vec::new(),
        },
        content_hash: driven.input.plan_ref(),
        cursor_kind: CursorKind::OpaqueToken,
        schedule: Schedule::default(),
    };
    plan.validate()?;
    Ok(plan)
}

pub fn run(cmd: JobCmd, bodies: &Bodies) -> Result<()> {
    let registered = |name: &str| bodies.get(name).is_some();
    match cmd {
        JobCmd::Validate { declaration } => {
            let text = std::fs::read_to_string(&declaration).with_context(|| format!("reading the declaration `{}`", declaration.display()))?;
            let jobs = parse_jobs(&text, &registered).with_context(|| declaration.display().to_string())?;
            bind(&jobs, &declaration)?;
            for job in jobs {
                match &job.kind {
                    JobKind::StoreDriven(d) => {
                        println!("{}: valid ({}, body {}, max_in_flight {}, plan {})", job.name, job.kind_name(), d.input.body, d.max_in_flight, &d.input.plan_ref()[..16])
                    }
                    JobKind::Maintenance(_) => println!("{}: valid ({})", job.name, job.kind_name()),
                }
            }
            Ok(())
        }
        JobCmd::Fire { name, project, declaration, run_id, site_id, site_id_env, admit } => {
            let l = project.locate(declaration)?;
            let text = std::fs::read_to_string(&l.declaration).with_context(|| format!("reading the declaration `{}`", l.declaration.display()))?;
            let jobs = parse_jobs(&text, &registered).with_context(|| l.declaration.display().to_string())?;
            bind(&jobs, &l.declaration)?;
            let job = jobs.into_iter().find(|j| j.name == name).with_context(|| format!("no job `{name}` is declared"))?;
            let JobKind::StoreDriven(driven) = &job.kind else {
                bail!("job `{name}` is kind `{}`; `job fire` fires a store-driven job", job.kind_name());
            };
            let body = bodies.get(&driven.input.body).context("a validated body is registered")?;
            let site_id = site_id_for(&text, &l.declaration, site_id, site_id_env)?;
            let (authority, _) = admit.admit(project.project.as_deref(), "a store-driven job")?;
            let face = face(&l)?;
            let w = wire_at(&l.project, &project.now)?;
            for reaped in w.engine.reap_orphans()? {
                eprintln!("{reaped}: reaped as partial_failure, its owner lease lapsed");
            }
            let run_id = run_id.unwrap_or_else(|| format!("job-{}", &sha256_hex(format!("{}{}", w.clock.now().unix_nanos(), std::process::id()).as_bytes())[..12]));

            let statement = driven.input.statement.clone();
            let mut read = |as_of: &str| -> Result<InputSet, Failure> {
                let refused = |e: &dyn std::fmt::Display| Failure::deterministic(FailureTag::Permanent, e.to_string());
                let bound = Bound::parse(as_of).map_err(|e| refused(&e))?;
                let bounds = Bounds { as_of: Some(bound), valid_as_of: None };
                let session = face.session(&authority, &Request { zone: None }, bounds).map_err(|e| refused(&e))?;
                let (response, snapshots) = face.input(&session, &statement, bounds).map_err(|e| refused(&e))?;
                InputSet::from_response(as_of, snapshots, &response.columns, response.rows, response.truncated).map_err(|e| refused(&e))
            };

            let decls = TableDecl::parse_pipeline(&text).with_context(|| l.declaration.display().to_string())?;
            let store = Store::open(&l.project.dir, &l.project.name)?;
            let (node, _) = node::resolve(&store, |k| std::env::var(k).ok())?;
            let mut dest = StoreDestination { store, decls, node, author: None };
            let engine = &w.engine;
            let mut land = |emitted: &Emitted| -> Result<Landed, Failure> {
                if let Some(table) = emitted.keys().find(|t| !driven.tables.contains(t)) {
                    return Err(Failure::deterministic(FailureTag::Permanent, format!("body `{}` emitted rows for `{table}`, which job `{name}` does not declare in `tables`", driven.input.body)));
                }
                let mut total = Landed { rows: 0, bytes: 0 };
                for table in &driven.tables {
                    let plan = output_plan(&name, table, driven).map_err(|e| Failure::deterministic(FailureTag::Permanent, e.to_string()))?;
                    let connector = plan.connector_pin(&plan.content_hash);
                    let spec = RunSpec { plan, connector, run_id: format!("{run_id}.{table}"), site_id: site_id.clone(), pid: std::process::id(), boot_id: boot_id(), trace_id: None };
                    let mut source: Box<dyn Source> = Box::new(Staged(emitted.get(table).cloned().unwrap_or_default()));
                    let row = engine.run_with(&spec, &mut source, &Unshaped, &mut dest).map_err(|e| Failure::new(FailureTag::Storage, e.to_string()))?;
                    if row.status != RunStatus::Success {
                        return Err(Failure::new(
                            row.error_kind.unwrap_or(FailureTag::Storage),
                            format!("table `{table}` closed {} in run `{}`: {}", row.status, row.run_id, row.error_message.unwrap_or_default()),
                        ));
                    }
                    total.rows += row.rows;
                    total.bytes += row.bytes;
                }
                Ok(total)
            };

            let drive = Drive {
                job: name.clone(),
                input: driven.input.clone(),
                body: body.as_ref(),
                max_in_flight: driven.max_in_flight,
                run_id: run_id.clone(),
                site_id: site_id.clone(),
                pid: std::process::id(),
                boot_id: boot_id(),
                schedule: Schedule::default(),
                poll: PARKED_POLL,
            };
            let row = w.engine.drive(&drive, &mut read, &mut land)?;
            let input = row.input.clone().map(|i| i.rows).unwrap_or_default();
            if row.status == RunStatus::Success {
                println!("{name}: {} success · {} rows landed from {input} input rows", row.run_id, row.rows);
                Ok(())
            } else {
                bail!("{name}: {} {} — {}", row.run_id, row.status, row.error_message.unwrap_or_default())
            }
        }
    }
}

/// Bind each job's target to the tables the manifest's pipelines and store-driven jobs
/// produce (`surface.fire.target-unbound`).
fn bind(jobs: &[Job], declaration: &std::path::Path) -> Result<()> {
    let specs: Vec<_> = contextful_core::pipeline::declare::collect(&crate::pipeline::manifests(declaration)?)?.into_iter().map(|d| d.spec).collect();
    bind_targets(jobs, &Targets::new(&specs, jobs)).with_context(|| declaration.display().to_string())?;
    Ok(())
}
