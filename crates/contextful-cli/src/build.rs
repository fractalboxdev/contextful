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

fn now(flag: &Option<String>) -> Result<Instant> {
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
        Some(m) => Ok(m.spec.clone()),
        None => {
            let ids: Vec<&str> = models.iter().map(|m| m.spec.id.as_str()).collect();
            Err(RunError::ModelUndeclared(format!("no model `{id}` is declared; the declared models are [{}]", ids.join(", "))).into())
        }
    }
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
            let spec = model(&l.declaration, &id)?;
            let text = if l.declaration.exists() { std::fs::read_to_string(&l.declaration)? } else { String::new() };
            let site_id = site_id_for(&text, &l.declaration, args.site_id, args.site_id_env)?;
            let started_at = now(&args.project.now)?;
            let face = open_face(&l.project, &l.declaration, &text, Pepper::resolve(|k| std::env::var(k).ok()))
                .with_context(|| format!("opening the read face over `{}`", l.declaration.display()))?;
            // An injected instant fixes the build's completion too, so a replayed build is byte-stable.
            let completed_at = match &args.project.now {
                Some(_) => started_at,
                None => now(&None)?.max(started_at),
            };
            let built = build(&face, &BuildRequest { model: &spec, site_id: &site_id, started_at, completed_at })?;
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
