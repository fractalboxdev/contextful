//! `contextful seed` and the seeding pass `pipeline run` takes (`run.seed`).
//!
//! A declared `[pipeline.seed]` loads consumer-held history through the engine's seeding
//! loader before a table's live pull: one chunk names the whole export, its rows land in the
//! live table under the `<table>#seed` scope, and every later fire probes the export with one
//! HEAD and compares the answer with the fingerprint stamped at commit. `seed status` reads
//! each seed's ceiling, commit state and fingerprint match; `seed reset` clears a seed so the
//! next fire loads it again.

use crate::pipeline::{build_source, manifests, plan, seed_binding};
use crate::run::{boot_id, wire_at, ProjectArgs, StoreDestination};
use anyhow::{bail, Context, Result};
use clap::Subcommand;
use contextful_connectors::http::{HttpSource, Mediation};
use contextful_core::connector::meter::LimiterBinding;
use contextful_core::pipeline::declare::{collect, PipelineSpec, TableEntry};
use contextful_core::pipeline::seed::{probe, SeedBlock, SeedCeiling, SeedProbe};
use contextful_core::pipeline::transform::Chain;
use contextful_core::ports::Clock;
use contextful_core::run::advance::CursorKind;
use contextful_core::run::backfill::{Bound, Window};
use contextful_core::run::journal::sha256_hex;
use contextful_core::run::plan::{ConnectorSpec, CursorSpec, Plan, NATIVE_WORLD};
use contextful_core::run::record::RunStatus;
use contextful_engine::seed::{FingerprintProbe, SeedLoad, SeedOutcome};
use contextful_engine::{Engine, RunSpec};
use contextful_outbound::Limiter;
use std::path::PathBuf;
use std::sync::Arc;

/// The one chunk an HTTP seed plans: the whole export.
const EXPORT_CHUNK: &str = "export";

#[derive(Subcommand)]
pub enum SeedCmd {
    /// Print each seeded table's ceiling, commit state and fingerprint match.
    Status {
        /// Read this pipeline's seed alone; absent, every declared seed.
        pipeline: Option<String>,
        #[command(flatten)]
        project: ProjectArgs,
        #[arg(long)]
        declaration: Option<PathBuf>,
    },
    /// Clear a seed's chunk plan and commit stamp, so the next `pipeline run` loads it again.
    Reset {
        pipeline: String,
        /// Clear this table's seed alone, by declared or destination name; absent, every table.
        #[arg(long)]
        table: Option<String>,
        #[command(flatten)]
        project: ProjectArgs,
        #[arg(long)]
        declaration: Option<PathBuf>,
    },
}

/// A seed source probed with one HEAD of its table URL (`run.seed.fingerprint`).
struct HttpHead(HttpSource);

impl FingerprintProbe for HttpHead {
    fn fingerprint(&self) -> Option<String> {
        self.0.fingerprint()
    }
}

/// What a seed source's requests pass: the resolver, and the limiter binding its declared
/// quota names, metered against the same clock as the live source.
pub(crate) struct Metered {
    pub binding: Option<LimiterBinding>,
    pub resolver: Arc<contextful_outbound::Resolver>,
    pub clock: Arc<dyn Clock + Send + Sync>,
}

impl Metered {
    /// The limiter for run `run_id`; `None` when the seed source declares no quota.
    fn limiter(&self, run_id: &str) -> Result<Option<Arc<Limiter>>> {
        Ok(self.binding.clone().map(|b| Limiter::new(b, self.resolver.clone(), run_id, self.clock.clone()).map(Arc::new)).transpose()?)
    }
}

/// Surrender `limiter`'s unspent permits and print its events (`connector.meter.report-delivery`).
fn finish(table: &str, limiter: Option<&Arc<Limiter>>) {
    if let Some(limiter) = limiter {
        limiter.finish();
        for entry in limiter.audit() {
            eprintln!("{table}: {entry}");
        }
    }
}

/// The HTTP source the seed of `spec` reads for table `t`, its run tagged `run_id` and its
/// requests metered by `limiter`.
fn seed_source(spec: &PipelineSpec, seed: &SeedBlock, t: &TableEntry, metered: &Metered, run_id: Option<&str>, limiter: Option<Arc<Limiter>>) -> Result<HttpSource> {
    let config = build_source(spec, "seed source", &seed.source)?;
    let mediation = Mediation { limiter, run_id: run_id.map(str::to_string), ..Mediation::default() };
    Ok(HttpSource::mediated(config, t.name(), metered.resolver.clone(), mediation)?)
}

/// The plan a seed chunk of table `t` runs against: the live plan's pipeline, table and
/// content hash under the seed source's connector, walking the export in order.
fn seed_plan(spec: &PipelineSpec, seed: &SeedBlock, t: &TableEntry) -> Result<Plan> {
    let connector = ConnectorSpec {
        id: seed.source.name.clone(),
        version: env!("CARGO_PKG_VERSION").to_string(),
        world: NATIVE_WORLD.to_string(),
        command: vec![format!("builtin:{}", seed.source.name)],
    };
    let mut plan = plan(spec, t.name(), connector)?;
    plan.cursor_kind = CursorKind::OpaqueToken;
    plan.spec.cursor = CursorSpec { kind: Some(CursorKind::OpaqueToken.name().to_string()), field: None, allowed_lateness: None };
    plan.validate()?;
    Ok(plan)
}

/// Run the seed of table `t` before its live pull: plan the export chunk when the seed holds
/// none, then drive [`contextful_engine::seed::Seeding::run_next`] until the seed commits or
/// skips. Answers whether the live pull proceeds; `false` while another worker holds the
/// seed's chunk. Each opened run id joins `attempted`.
#[allow(clippy::too_many_arguments)]
pub(crate) fn drive<J, B>(
    engine: &Engine<J, B>,
    spec: &PipelineSpec,
    seed: &SeedBlock,
    t: &TableEntry,
    run_id: &str,
    site_id: &str,
    metered: &Metered,
    dest: &mut StoreDestination,
    attempted: &mut Vec<String>,
) -> Result<bool>
where
    J: contextful_core::run::ports::JournalStore,
    B: contextful_core::run::ports::BlobStore,
{
    let table = spec.table_name(t.name());
    let seeding = engine.seeding();
    if seeding.state(&spec.id, &table)?.chunks.is_empty() {
        let window = Window::new(Bound::Text(String::new()), Bound::Text(seed.below.render()))?;
        seeding.plan(&spec.id, &table, &[(EXPORT_CHUNK.to_string(), window)])?;
    }
    let plan = seed_plan(spec, seed, t)?;
    dest.store.validate_source_plan(&plan, dest.normalize)?;
    let load = SeedLoad {
        ceiling: SeedCeiling::new(spec.destination_decl(t).order_by(), seed.below.clone()),
        live_kind: if spec.incremental.is_some() { CursorKind::Monotonic } else { CursorKind::OpaqueToken },
        live_field: spec.incremental.clone(),
        normalize: dest.normalize,
    };
    let artifact = sha256_hex(serde_json::to_string(&seed.source).unwrap_or_default().as_bytes());
    let probe_limiter = metered.limiter(&format!("{run_id}.seed-probe"))?;
    let head = HttpHead(seed_source(spec, seed, t, metered, None, probe_limiter.clone())?);
    let shape = Chain { ops: spec.transforms.clone(), table: table.clone() };
    let outcome = (|| -> Result<bool> {
        for n in 0..=seeding.state(&spec.id, &table)?.chunks.len() {
            let run = format!("{run_id}.seed-{n}");
            let limiter = metered.limiter(&run)?;
            let mut source = seed_source(spec, seed, t, metered, Some(&run), limiter.clone())?;
            let trace = crate::telemetry::RunTrace::open();
            let spec_run = RunSpec { connector: plan.connector_pin(&artifact), plan: plan.clone(), run_id: run.clone(), site_id: site_id.to_string(), pid: std::process::id(), boot_id: boot_id(), trace_id: Some(trace.trace_id()) };
            let opened = engine.catalog.run(&run)?.is_none();
            let outcome = seeding.run_next(&load, &spec_run, &mut source, &shape, dest, &head);
            finish(&table, limiter.as_ref());
            let outcome = outcome?;
            if let SeedOutcome::Ran(row) = &outcome {
                trace.close(row);
            }
            if opened && engine.catalog.run(&run)?.is_some() {
                attempted.push(run.clone());
            }
            match outcome {
                SeedOutcome::Ran(row) if row.status == RunStatus::Success => {
                    println!("{table}: {run} seeded · {} rows in {} batches", row.rows, row.batches);
                    if seeding.state(&spec.id, &table)?.committed() {
                        return Ok(true);
                    }
                }
                SeedOutcome::Ran(row) => bail!(
                    "the seed failed as {} in run `{run}`: {}",
                    row.error_kind.map(|k| k.name()).unwrap_or("unknown"),
                    row.error_message.unwrap_or_default()
                ),
                SeedOutcome::Unavailable(warning) => {
                    eprintln!("warning: {warning}");
                    return Ok(true);
                }
                SeedOutcome::Unchanged | SeedOutcome::Committed => return Ok(true),
                SeedOutcome::Idle => return Ok(false),
            }
        }
        Ok(seeding.state(&spec.id, &table)?.committed())
    })();
    finish(&table, probe_limiter.as_ref());
    outcome
}

pub fn run(cmd: SeedCmd) -> Result<()> {
    match cmd {
        SeedCmd::Status { pipeline, project, declaration } => {
            let l = project.locate(declaration)?;
            let declared = collect(&manifests(&l.declaration)?)?;
            let w = wire_at(&l.project, &project.now)?;
            let vars: std::collections::BTreeMap<String, String> = std::env::vars().collect();
            let resolver = Arc::new(contextful_outbound::assemble(&vars, w.clock.clone())?);
            let clock: Arc<dyn Clock + Send + Sync> = w.clock.clone();
            let mut printed = false;
            for d in declared.iter().filter(|d| pipeline.as_deref().is_none_or(|p| p == d.spec.id)) {
                let Some(seed) = d.spec.seed_block()? else { continue };
                let metered = Metered { binding: seed_binding(&d.spec, &l.declaration)?, resolver: resolver.clone(), clock: clock.clone() };
                for t in &d.spec.tables {
                    let table = d.spec.table_name(t.name());
                    let state = w.engine.seeding().state(&d.spec.id, &table)?;
                    let commit = if state.committed() {
                        "committed".to_string()
                    } else {
                        let done = state.chunks.iter().filter(|c| c.status == contextful_core::run::backfill::ChunkStatus::Done).count();
                        format!("uncommitted ({done} of {} chunks done)", state.chunks.len())
                    };
                    let matched = match state.fingerprint.as_ref() {
                        None => "unstamped",
                        Some(stamped) => {
                            let limiter = metered.limiter("seed-status")?;
                            let observed = HttpHead(seed_source(&d.spec, &seed, t, &metered, None, limiter.clone())?).fingerprint();
                            finish(&table, limiter.as_ref());
                            match probe(&table, Some(stamped.as_deref()), observed.as_deref()) {
                                Ok(SeedProbe::Unchanged) => "match",
                                Ok(SeedProbe::Load) | Ok(SeedProbe::Unavailable(_)) => "unavailable",
                                Err(_) => "moved",
                            }
                        }
                    };
                    println!("{} {table}: ceiling {} · {commit} · fingerprint {matched}", d.spec.id, seed.below.render());
                    printed = true;
                }
            }
            if !printed {
                match pipeline {
                    Some(p) => bail!("pipeline `{p}` declares no `[pipeline.seed]`"),
                    None => println!("no pipeline declares a `[pipeline.seed]`"),
                }
            }
            Ok(())
        }
        SeedCmd::Reset { pipeline, table, project, declaration } => {
            let l = project.locate(declaration)?;
            let declared = collect(&manifests(&l.declaration)?)?;
            let d = declared.iter().find(|d| d.spec.id == pipeline).with_context(|| format!("no pipeline `{pipeline}` is declared"))?;
            if d.spec.seed_block()?.is_none() {
                bail!("pipeline `{pipeline}` declares no `[pipeline.seed]`");
            }
            let tables: Vec<String> = d
                .spec
                .tables
                .iter()
                .map(|t| (t.name(), d.spec.table_name(t.name())))
                .filter(|(name, destination)| table.as_deref().is_none_or(|want| want == *name || want == destination.as_str()))
                .map(|(_, destination)| destination)
                .collect();
            if tables.is_empty() {
                bail!("pipeline `{pipeline}` declares no table `{}`", table.unwrap_or_default());
            }
            let w = wire_at(&l.project, &project.now)?;
            for destination in tables {
                let removed = w.engine.seeding().reset(&pipeline, &destination)?;
                println!("{destination}: seed reset · {} chunk(s) cleared", removed.len());
            }
            Ok(())
        }
    }
}
