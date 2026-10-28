//! `contextful build` — build a declared model, and hold a published build.
//!
//! A thin adapter: it reads the manifests, resolves the project, site id and instant,
//! calls the store's build or hold, and prints the receipt. No model rule lives here.

use crate::pipeline::manifests;
use crate::project::open_face;
use crate::run::{site_id_for, ProjectArgs};
use anyhow::{Context, Result};
use contextful_context::build::{build, hold, status, write_logs, BuildRequest};
use contextful_context::Store;
use contextful_core::pipeline::declare::collect;
use contextful_core::pipeline::model::{collect_models, duration_secs, ModelSpec, Receipt};
use contextful_core::run::RunError;
use contextful_core::time::Instant;
use contextful_policy::enforce::mask::Pepper;
use serde_json::json;
use std::path::{Path, PathBuf};

#[derive(clap::Args)]
#[command(args_conflicts_with_subcommands = true, subcommand_negates_reqs = true)]
pub struct BuildArgs {
    #[command(subcommand)]
    cmd: Option<BuildCmd>,
    /// The model to build: materialize, check its contract, run its tests, publish.
    #[arg(required = true)]
    model: Option<String>,
    #[command(flatten)]
    project: ProjectArgs,
    /// The project manifest; `pipelines/*.toml` beside it are read too. Absent, the project's `contextful.toml`.
    #[arg(long)]
    declaration: Option<PathBuf>,
    /// The site id the build's rows carry; replaces the manifest's `site_id` or `site_id_env`.
    #[arg(long)]
    site_id: Option<String>,
    /// The environment variable holding the site id; replaces the manifest's declaration.
    #[arg(long)]
    site_id_env: Option<String>,
    /// Print the receipt as JSON: model, build id, row count, watermark.
    #[arg(long)]
    json: bool,
    /// Read every input at this availability bound and record it as the watermark's `horizon`.
    #[arg(long)]
    available_as_of: Option<String>,
}

#[derive(clap::Subcommand)]
enum BuildCmd {
    /// Inspect the last build attempt and the build readers see.
    Status {
        model: String,
        #[command(flatten)]
        project: ProjectArgs,
        #[arg(long)]
        declaration: Option<PathBuf>,
        #[arg(long)]
        json: bool,
    },
    /// Hold a published build against collection for a duration.
    Hold {
        /// How long the hold lasts: an integer followed by `s`, `m`, `h` or `d`.
        #[arg(long = "for", value_name = "N[smhd]", value_parser = parse_duration)]
        duration: u64,
        model: String,
        build: String,
        #[command(flatten)]
        project: ProjectArgs,
        #[arg(long)]
        declaration: Option<PathBuf>,
        /// The placing principal; absent, the operator's login name.
        #[arg(long)]
        by: Option<String>,
        /// Print the receipt as JSON.
        #[arg(long)]
        json: bool,
    },
}

fn parse_duration(s: &str) -> std::result::Result<u64, String> {
    duration_secs(s).ok_or_else(|| format!("`{s}` is not an integer above zero followed by s, m, h or d"))
}

pub(crate) fn now(flag: &Option<String>) -> Result<Instant> {
    match flag {
        Some(s) => Ok(Instant::parse(s)?),
        None => {
            let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH)?.as_nanos();
            Ok(Instant::from_unix_nanos(i128::try_from(nanos)?)?)
        }
    }
}

/// The declared model named `id` (`run.model.unknown-model`).
fn model(declaration: &Path, id: &str) -> Result<ModelSpec> {
    let files = manifests(declaration)?;
    let pipelines = collect(&files)?;
    let models = collect_models(&files, &pipelines)?;
    match models.iter().find(|m| m.spec.id == id) {
        Some(m) => {
            let mut spec = m.spec.clone();
            crate::model_source::resolve(&mut spec, &m.file)?;
            Ok(spec)
        },
        None => {
            let ids: Vec<&str> = models.iter().map(|m| m.spec.id.as_str()).collect();
            Err(RunError::ModelUndeclared(format!("no model `{id}` is declared; the declared models are [{}]", ids.join(", "))).into())
        }
    }
}

/// The declared model `id`, refused while the declared models hold a cycle
/// (`run.model.model-cycle`), before any store is read.
pub(crate) fn buildable(declaration: &Path, id: &str) -> Result<ModelSpec> {
    let spec = model(declaration, id)?;
    let files = manifests(declaration)?;
    let mut specs = Vec::new();
    for m in collect_models(&files, &collect(&files)?)? {
        let mut declared = m.spec;
        crate::model_source::resolve(&mut declared, &m.file)?;
        specs.push(declared);
    }
    crate::model_source::refuse_cycle(&specs)?;
    Ok(spec)
}

pub fn run(args: BuildArgs) -> Result<()> {
    match args.cmd {
        Some(BuildCmd::Status { model: id, project, declaration, json }) => {
            let l = project.locate(declaration)?;
            model(&l.declaration, &id)?;
            let store = Store::open(&l.project.dir, &l.project.name)?;
            let result = status(&store, &id, now(&project.now)?)?;
            write_logs(&store, &id)?;
            if json {
                println!("{result}");
            } else {
                println!("{id}: {} · published {}", result["last_build_status"].as_str().unwrap_or("absent"),
                    result["published_build_id"].as_str().unwrap_or("none"));
            }
            Ok(())
        }
        Some(BuildCmd::Hold { duration, model: id, build: build_id, project, declaration, by, json }) => {
            let l = project.locate(declaration)?;
            model(&l.declaration, &id)?;
            let principal = match by.or_else(|| std::env::var("USER").ok()).or_else(|| std::env::var("LOGNAME").ok()) {
                Some(p) => p,
                None => anyhow::bail!("no placing principal: pass `--by`"),
            };
            let store = Store::open(&l.project.dir, &l.project.name)?;
            let (receipt, record) = hold(&store, &id, &build_id, &principal, duration, now(&project.now)?)?;
            let word = match receipt {
                Receipt::Held => "Held",
                Receipt::Renewed => "Renewed",
            };
            if json {
                println!(
                    "{}",
                    json!({
                        "receipt": word,
                        "model": id,
                        "build_id": record.build_id,
                        "principal": record.principal,
                        "placed_at": record.placed_at,
                        "expires_at": record.expires_at,
                    })
                );
            } else {
                println!("{word} {id} build {} until {} by {}", record.build_id, record.expires_at, record.principal);
            }
            Ok(())
        }
        None => {
            let id = args.model.expect("clap requires a model without a subcommand");
            let l = args.project.locate(args.declaration)?;
            let spec = buildable(&l.declaration, &id)?;
            let text = if l.declaration.exists() { std::fs::read_to_string(&l.declaration)? } else { String::new() };
            let grouped = crate::export::publication::validate_job(&text,&id,std::slice::from_ref(&id))?;
            let _publication = if grouped {Some(crate::export::publication::lock(&l)?)} else {None};
            let site_id = site_id_for(&text, &l.declaration, args.site_id, args.site_id_env)?;
            let started_at = now(&args.project.now)?;
            let face = open_face(&l.project, &l.declaration, &text, Pepper::resolve(|k| std::env::var(k).ok()))
                .with_context(|| format!("opening the read face over `{}`", l.declaration.display()))?;
            // An injected instant fixes the build's completion too, so a replayed build is byte-stable.
            let completed_at = match &args.project.now {
                Some(_) => started_at,
                None => now(&None)?.max(started_at),
            };
            let available_as_of = args.available_as_of.as_deref().map(contextful_core::store::bound_time::Bound::parse).transpose()?;
            let built = build(&face, &BuildRequest { model: &spec, site_id: &site_id, started_at, completed_at, available_as_of })?;
            if grouped {
                if built.section.is_none() {anyhow::bail!("PublicationIncompatible: a publication member is a materialized build");}
                crate::export::publication::record_completion(face.store(),&text,&id,&built.build_id,serde_json::to_value(&built.watermark)?)?;
            }
            if args.json {
                println!(
                    "{}",
                    json!({
                        "model": built.model,
                        "build_id": built.build_id,
                        "rows": built.rows,
                        "watermark": built.watermark,
                        "published": built.section.is_some(),
                    })
                );
            } else {
                println!("{}: built {} · {} rows", built.model, built.build_id, built.rows);
            }
            Ok(())
        }
    }
}


/// The plan reference a build job's execution pins: the job, its target model and the
/// declaration's structure (`run.own.apply-retires-superseded`).
pub(crate) fn job_plan_ref(job: &str, spec: &ModelSpec, effective: &crate::effective::Effective) -> Result<String> {
    let structure = effective.structural_identity(std::slice::from_ref(&spec.id))?;
    Ok(contextful_core::run::journal::sha256_hex(&serde_json::to_vec(&("build-structure-v1", job, spec, structure))?))
}

/// The native maintenance adapter uses the same execution owner and durable history
/// as other host jobs. The model publisher remains the sole materialization path.
pub(crate) fn fire_job(
    located: &crate::project::Located, project: &ProjectArgs, job: &contextful_core::job::Job,
    local: &str, effective: &crate::effective::Effective, run_id: Option<String>, site_id: Option<String>, site_id_env: Option<String>,
) -> Result<()> {
    use contextful_core::run::ports::{Cancellation, ExecutionPort, OpenExecution, Outcome};
    use contextful_core::run::own::PlanPins;

    use contextful_core::run::journal::sha256_hex;
    crate::job::bind_declared(std::slice::from_ref(job), &effective.specs, &effective.models)?;
    let spec = effective.models.iter().find(|m| Some(&m.id) == job.target.as_ref()).context("build target is declared")?;
    // Publication bindings belong to local delivery, independently of the applied model.
    let grouped = crate::export::publication::validate_job(local,&job.name,std::slice::from_ref(&spec.id))?;
    let _publication = if grouped {Some(crate::export::publication::lock(located)?)} else {None};
    let face = effective.face(located, local, None, None)?;
    let site = site_id_for(local, &located.declaration, site_id, site_id_env)?;
    let wire = crate::run::wire_at(&located.project, &project.now)?;
    wire.engine.reap_orphans()?;
    let started_at = wire.engine.catalog.now()?;
    let run_id = run_id.unwrap_or_else(|| format!("build-job-{}", &sha256_hex(format!("{}:{}:{}", job.name, started_at, std::process::id()).as_bytes())[..20]));
    let pin = job_plan_ref(&job.name, spec, effective)?;
    let execution = wire.engine.open_execution(&OpenExecution {
        scope: contextful_engine::drive::job_scope(&job.name),
        pins: PlanPins { plan_ref: pin, identities: Default::default() }.into(),
        run_id, site_id: site.clone(), pid: std::process::id(), boot_id: crate::run::boot_id(),
        trace_id: None, connector: None, schedule: Default::default(),
    })?;
    let result = (|| -> Result<_> {
        Ok(contextful_context::build::build_guarded(&face, &BuildRequest { model: spec, site_id: &site, started_at, completed_at: now(&project.now)?.max(started_at), available_as_of: None }, &|| {
            contextful_engine::cancel::poll(wire.engine.catalog.as_ref(), execution.run_id(), execution.token());
            if execution.token().requested() {
                Err(contextful_context::ContextError::Catalog(contextful_core::run::Failure::new(contextful_core::run::FailureTag::Canceled, "build execution stopped before publication")))
            } else { Ok(()) }
        })?)
    })();
    let outcome = match &result {
        Ok(built) => Outcome::Success { rows: built.rows, bytes: 0, batches: 1 },
        Err(error) => Outcome::Failed(build_failure(error)),
    };
    execution.close(outcome)?;
    // The fire's runs close as `pipeline run`'s do (`store.push.after-run`).
    let settled = crate::sync::settle_fire(located, wire.clock.now());
    let built = result?;
    settled?;
    if grouped {
        if built.section.is_none() {anyhow::bail!("PublicationIncompatible: a publication member is a materialized build");}
        let store = Store::open(&located.project.dir,&located.project.name)?;
        crate::export::publication::record_completion(&store,local,&job.name,&built.build_id,serde_json::to_value(&built.watermark)?)?;
    }
    println!("{}: built {} · {} rows", built.model, built.build_id, built.rows);
    Ok(())
}


fn build_failure(error: &anyhow::Error) -> contextful_core::run::Failure {
    use contextful_core::run::{Failure, FailureTag};
    use contextful_context::{ContextError, read::ReadFault};
    let context = error.downcast_ref::<ContextError>().or_else(|| match error.downcast_ref::<ReadFault>() {
        Some(ReadFault::Store(e)) => Some(e), _ => None,
    });
    match context {
        Some(ContextError::Catalog(failure)) => failure.clone(),
        Some(ContextError::Run(_) | ContextError::Invalid(_) | ContextError::ColumnType { .. }) => Failure::deterministic(FailureTag::Permanent, format!("{error:#}")),
        _ => Failure::new(FailureTag::Storage, format!("{error:#}")),
    }
}
