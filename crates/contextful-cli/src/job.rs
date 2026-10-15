//! `contextful job` — the manifest's `[[job]]` blocks on the command line.
//!
//! `validate` holds every block to the closed kind union and the row bodies the binary
//! registers, and compiles each plan job's plan. `fire` runs one store-driven job once: it
//! admits the job's credential, reads the input statement through the read face under it at
//! the job's `as_of`, runs the registered body per row through the engine, and lands the
//! emitted rows, one table run per output table. A plan job's fire lowers its compiled plan
//! onto one execution (`surface.fire.plan-job`).

use crate::admit::AdmitArgs;
use crate::run::{boot_id, site_id_for, wire_at, ProjectArgs, StoreDestination};
use anyhow::{bail, Context, Result};
use clap::Subcommand;
use contextful_connectors::derive::Staged;
use contextful_context::{node, Store};
use contextful_core::job::{bind_targets, parse_jobs, Job, JobKind, PlanJob, StoreDriven, Targets};
use contextful_core::run::nodes::NodePlan;
use contextful_engine::nodes::{CommandConnectors, PlanFire};
use contextful_core::run::advance::CursorKind;
use contextful_core::run::drive::{Bodies, Emitted, InputSet, RowBody};
use contextful_core::surface::delegate::BODY_PREFIX;
use std::sync::Arc;
use contextful_core::run::effect::{EffectAdmission, EffectScope, EmissionSummary, RecordedBodyPlan, RecordedEffect};
use contextful_core::run::journal::sha256_hex;
use contextful_core::run::plan::{ConnectorSpec, CursorSpec, Plan, PlanSpec, NATIVE_WORLD};
use contextful_core::run::ports::{Landed, Source, Unshaped};
use contextful_core::run::record::RunStatus;
use contextful_core::run::retry::Schedule;
use contextful_core::run::{Failure, FailureTag};
use contextful_core::store::bound_time::{Bound, Bounds};
use contextful_engine::drive::{Drive, PARKED_POLL};
use contextful_engine::drive::sweep::SweepFire;
use contextful_context::read::face::Carried;
use contextful_core::run::drive::StoreInput;
use contextful_core::run::sweep::stamp_origin;
use contextful_core::time::Instant;
use contextful_engine::RunSpec;
use contextful_policy::enforce::session::Request;
use std::path::PathBuf;

/// The canonical writer prepares and admits effects without owning execution or journal state.
struct StoreEffects(Store);

impl EffectAdmission for StoreEffects {
    fn identity(&self, effect: &RecordedEffect) -> Result<String, Failure> {
        self.0.effect_recording_identity(&effect.table, effect.normalize).map_err(store_error)
    }
    fn prepare(&self, effect: &RecordedEffect, rows: Vec<contextful_core::run::ports::Row>, types: contextful_core::run::ports::Types, scope: &EffectScope) -> Result<serde_json::Value, Failure> {
        let batch = contextful_context::land::Batch { rows, types:types.into_iter().collect() };
        let prepared = self.0.prepare_effect_recording(&effect.table, &batch, effect.normalize, &scope.key().execution_id, scope).map_err(store_error)?;
        serde_json::from_slice(&prepared.encode().map_err(store_error)?).map_err(|_| Failure::deterministic(FailureTag::SchemaIncompatible, "prepared body result does not encode"))
    }
    fn admit(&self, effect: &RecordedEffect, scope: &EffectScope, payload: &serde_json::Value) -> Result<EmissionSummary, Failure> {
        let bytes = serde_json::to_vec(payload).map_err(|_| Failure::deterministic(FailureTag::SchemaIncompatible, "prepared body result does not encode"))?;
        self.0.admit_effect_recording(&effect.table, &bytes, effect.normalize, scope).map_err(store_error)?.summary().map_err(store_error)
    }
}

fn store_error(error: contextful_context::ContextError) -> Failure { Failure::deterministic(FailureTag::Permanent, error.to_string()) }
fn engine_error(error: contextful_engine::EngineError) -> Failure {
    match error {
        contextful_engine::EngineError::Failure(failure) => failure,
        other => Failure::new(FailureTag::Storage, other.to_string()),
    }
}

#[derive(Subcommand)]
pub enum JobCmd {
    /// Check every `[[job]]` block against the kind union and the registered row bodies.
    Validate {
        #[arg(long, default_value = "contextful.toml")]
        declaration: PathBuf,
    },
    /// Fire one store-driven or plan job once, resuming its pending execution when one holds.
    Fire {
        name: String,
        #[command(flatten)]
        project: ProjectArgs,
        /// The project manifest holding the job block; absent, the project's `contextful.toml`.
        #[arg(long)]
        declaration: Option<PathBuf>,
        /// Fire the job in immutable applied snapshot N.
        #[arg(long)]
        applied: Option<u64>,
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
            cursor: CursorSpec { kind: Some(CursorKind::OpaqueToken.name().to_string()), field: None, ..CursorSpec::default() },
            retry: None,
            // The rows are the body's recorded output; the land re-derives them on a resume.
            journal: false,
            redact: Vec::new(),
            redaction: Vec::new(),
        },
        content_hash: driven.input.plan_ref(),
        cursor_kind: CursorKind::OpaqueToken,
        schedule: Schedule::default(),
    };
    plan.validate()?;
    Ok(plan)
}

/// Raw and prepared outputs share each declared table's attempt and status accounting.
fn land_tables(driven: &StoreDriven, name: &str, run_id: &str, site_id: &str, mut run: impl FnMut(&str, &RunSpec) -> Result<contextful_core::run::record::RunRow, Failure>) -> Result<Landed, Failure> {
    let mut total = Landed::default();
    for table in &driven.tables {
        let plan = output_plan(name, table, driven).map_err(|e| Failure::deterministic(FailureTag::Permanent, e.to_string()))?;
        let connector = plan.connector_pin(&plan.content_hash);
        let spec = RunSpec { plan, connector, run_id:format!("{run_id}.{table}"), site_id:site_id.into(), pid:std::process::id(), boot_id:boot_id(), trace_id:None };
        let row = run(table, &spec)?;
        if row.status != RunStatus::Success {
            return Err(Failure::new(row.error_kind.unwrap_or(FailureTag::Storage), format!("table `{table}` closed {} in run `{}`: {}", row.status, row.run_id, row.error_message.unwrap_or_default())));
        }
        total.rows += row.rows;
        total.bytes += row.bytes;
    }
    Ok(total)
}

pub fn run(cmd: JobCmd, bodies: &Bodies) -> Result<()> {
    match cmd {
        JobCmd::Validate { declaration } => {
            let text = std::fs::read_to_string(&declaration).with_context(|| format!("reading the declaration `{}`", declaration.display()))?;
            let delegates = crate::delegate::declared(&text).with_context(|| declaration.display().to_string())?;
            let registered = |name: &str| bodies.get(name).is_some() || name.strip_prefix(BODY_PREFIX).is_some_and(|d| delegates.contains_key(d));
            let jobs = parse_jobs(&text, &registered).with_context(|| declaration.display().to_string())?;
            bind(&jobs, &declaration)?;
            for job in jobs {
                match &job.kind {
                    JobKind::StoreDriven(d) => {
                        crate::export::publication::validate_job(&text,&job.name,&d.tables)?;
                        println!("{}: valid ({}, body {}, max_in_flight {}, plan {})", job.name, job.kind_name(), d.input.body, d.max_in_flight, &d.input.plan_ref()[..16])
                    }
                    JobKind::Plan(declared) => {
                        let plan = compile_plan(&declaration, declared)?;
                        println!("{}: valid ({}, plan {} version {}, {} nodes)", job.name, job.kind_name(), plan.id, plan.version, plan.nodes.len())
                    }
                    JobKind::Maintenance(_) | JobKind::Synthesize(_) => println!("{}: valid ({})", job.name, job.kind_name()),
                }
            }
            Ok(())
        }
        JobCmd::Fire { name, project, declaration, applied, run_id, site_id, site_id_env, admit } => {
            let l = project.locate(declaration)?;
            let text = std::fs::read_to_string(&l.declaration).with_context(|| format!("reading the declaration `{}`", l.declaration.display()))?;
            let effective = crate::effective::Effective::resolve(&l, &text, applied)?;
            // Delegates bind in the daemon's own declaration, never the applied snapshot.
            let delegates = crate::delegate::declared(&text).with_context(|| l.declaration.display().to_string())?;
            let registered = |name: &str| bodies.get(name).is_some() || name.strip_prefix(BODY_PREFIX).is_some_and(|d| delegates.contains_key(d));
            let jobs = parse_jobs(&effective.main, &registered).with_context(|| l.declaration.display().to_string())?;
            let mut job = jobs.into_iter().find(|j| j.name == name).with_context(|| format!("no job `{name}` is declared"))?;
            if job.kind_name() == "build" {
                return crate::build::fire_job(&l, &project, &job, &text, &effective, run_id, site_id, site_id_env);
            }
            if let JobKind::Synthesize(synthesis) = &job.kind {
                let into = job.target.as_deref().context("a validated synthesize job names its target")?;
                return crate::memory::fire_synthesis(&l, &project, &admit, synthesis, into);
            }
            if let JobKind::Plan(declared) = &job.kind {
                return fire_plan(&l, &project, &text, &job.name, declared, run_id, site_id, site_id_env);
            }
            bind_declared(std::slice::from_ref(&job), &effective.specs, &effective.models)?;
            let JobKind::StoreDriven(driven) = &mut job.kind else {
                bail!("job `{name}` is kind `{}`; `job fire` fires a store-driven job", job.kind_name());
            };
            // Every origin of a sweep inherits the pinned structure from the driven input.
            driven.input.structural_identity = Some(effective.structural_identity(&driven.tables)?);
            let driven: &StoreDriven = driven;
            // Delivery bindings stay local; the applied job supplies the output table set.
            let grouped = crate::export::publication::validate_job(&text,&name,&driven.tables)?;
            let _publication = if grouped {Some(crate::export::publication::lock(&l)?)} else {None};
            let body: Arc<dyn RowBody> = match driven.input.body.strip_prefix(BODY_PREFIX) {
                Some(delegate) => {
                    let binding = delegates.get(delegate).cloned().with_context(|| format!("JobDelegateUnbound: job `{name}` names delegate `{delegate}`, which `[control.delegates]` does not bind"))?;
                    // Submit mode refuses before any execution opens, so no item receives a
                    // receipt for a mode this build does not dispatch.
                    if binding.mode == contextful_core::surface::delegate::Mode::Submit {
                        bail!("delegate `{delegate}` binds `mode = \"submit\"`, which this build does not dispatch; bind `call`");
                    }
                    Arc::new(crate::delegate::DelegateBody {
                        binding,
                        job: name.clone(),
                        tables: driven.tables.clone(),
                        version: applied,
                        entity: driven.isolated_items.as_ref().map(|p| (p.entity_namespace.clone(), p.entity_key.clone())),
                        transport: contextful_outbound::egress::system(),
                        key: crate::worker::worker_key()?,
                    })
                }
                None => bodies.get(&driven.input.body).context("a validated body is registered")?,
            };
            // A delegate takes one request per item attempt; the item budget is the only
            // retry layer (`surface.delegate.call-mode`, `run.items.retry-budget`).
            let delegated = driven.input.body.starts_with(BODY_PREFIX);
            let site_id = site_id_for(&text, &l.declaration, site_id, site_id_env)?;
            let (authority, _) = admit.admit(project.project.as_deref(), "a store-driven job")?;
            let store = crate::project::with_erasure_keys(Store::open_declared(&l.project.dir, &l.project.name, &l.declaration)?, &l.project, admit.public_key.as_deref(), admit.keyset.as_deref())?;
            let recorded_plan = body.recorded().map(|body| RecordedBodyPlan::compile(body.effects(), &driven.tables)).transpose()?;
            let effects = StoreEffects(store.clone());
            if let Some(plan) = &recorded_plan {
                for effect in plan.effects() { effects.identity(effect)?; }
            } else {
                for table in &driven.tables { store.validate_writer_recording(table, None)?; }
            }
            let face = effective.face(&l, &text, admit.public_key.as_deref(), admit.keyset.as_deref())?;
            let w = wire_at(&l.project, &project.now)?;
            for reaped in w.engine.reap_orphans()? {
                eprintln!("{reaped}: reaped as partial_failure, its owner lease lapsed");
            }
            let run_id = run_id.unwrap_or_else(|| format!("job-{}", &sha256_hex(format!("{}{}", w.clock.now().unix_nanos(), std::process::id()).as_bytes())[..12]));

            let statement = driven.input.statement.clone();
            let mut read = |as_of: &str| -> Result<InputSet, Failure> {
                let refused = |e: &dyn std::fmt::Display| Failure::deterministic(FailureTag::Permanent, e.to_string());
                let bound = Bound::parse(as_of).map_err(|e| refused(&e))?;
                let bounds = Bounds { as_of: Some(bound), valid_as_of: None, available_as_of: None, ..Bounds::default() };
                let session = face.session(&authority, &Request { zone: None }, bounds).map_err(|e| refused(&e))?;
                let (response, snapshots) = face.input(&session, &statement, bounds).map_err(|e| refused(&e))?;
                InputSet::from_response(as_of, snapshots, &response.columns, response.rows, response.truncated).map_err(|e| refused(&e))
            };

            let decls = effective.tables.clone();
            let (node, _) = node::resolve(&store, |k| std::env::var(k).ok())?;
            let mut dest = StoreDestination { store, decls, node, author: None, normalize: None, relational_parts: Default::default(), schema_diffs: Vec::new() };
            let engine = &w.engine;
            let drive = Drive {
                job: name.clone(), input: driven.input.clone(), body: body.as_ref(), max_in_flight: driven.max_in_flight,
                run_id: run_id.clone(), site_id: site_id.clone(), pid: std::process::id(), boot_id: boot_id(), schedule: if delegated { Schedule::single_attempt() } else { Schedule::default() }, poll: PARKED_POLL,
            };
            if let Some(sweep) = &driven.sweep {
                if recorded_plan.is_some() {
                    bail!("job `{name}` declares a sweep, and a sweep runs a body without a recorded result projection");
                }
                let fire = SweepFire { drive, sweep: (**sweep).clone() };
                let mut read_origin = |input: &StoreInput, as_of: &str, carried_at: Option<Instant>| -> Result<InputSet, Failure> {
                    let refused = |e: &dyn std::fmt::Display| Failure::deterministic(FailureTag::Permanent, e.to_string());
                    let origin = input.sweep.as_ref().map(|pin| pin.origin.as_str()).ok_or_else(|| refused(&"a sweep origin carries its pin"))?;
                    // Each origin reads with `available_as_of` equal to itself (`run.sweep.origins`).
                    let bounds = Bounds { as_of: Some(Bound::parse(as_of).map_err(|e| refused(&e))?), available_as_of: Some(Bound::parse(origin).map_err(|e| refused(&e))?), ..Bounds::default() };
                    let carried: Carried = carried_at.map(|at| driven.tables.iter().map(|t| (t.clone(), Bound { at, inclusive: true })).collect()).unwrap_or_default();
                    let session = face.session_carried(&authority, &Request { zone: None }, bounds, &carried).map_err(|e| refused(&e))?;
                    let (response, snapshots) = face.input(&session, &input.statement, bounds).map_err(|e| refused(&e))?;
                    InputSet::from_response(as_of, snapshots, &response.columns, response.rows, response.truncated).map_err(|e| refused(&e))
                };
                let mut land_origin = |input: &StoreInput, origin_run: &str, emitted: &Emitted| -> Result<Landed, Failure> {
                    if let Some(table) = emitted.keys().find(|t| !driven.tables.contains(t)) {
                        return Err(Failure::deterministic(FailureTag::Permanent, format!("body `{}` emitted rows for `{table}`, which job `{name}` does not declare in `tables`", driven.input.body)));
                    }
                    let origin = input.sweep.as_ref().map(|pin| pin.origin.clone()).unwrap_or_default();
                    let mut stamped = emitted.clone();
                    for (table, rows) in stamped.iter_mut() {
                        if let Some(decl) = dest.decls.iter().find(|d| &d.name == table) {
                            stamp_origin(decl, rows, &origin);
                        }
                    }
                    land_tables(driven, &name, origin_run, &site_id, |table, spec| {
                        dest.store.validate_writer_plan(&spec.plan, dest.normalize).map_err(|e| Failure::deterministic(FailureTag::Permanent, e.to_string()))?;
                        let mut source: Box<dyn Source> = Box::new(Staged(stamped.get(table).cloned().unwrap_or_default(), 0));
                        engine.run_with(spec, &mut source, &Unshaped, &mut dest).map_err(|e| Failure::new(FailureTag::Storage, e.to_string()))
                    })
                };
                let swept = engine.sweep(&fire, &mut read_origin, &mut land_origin)?;
                for row in &swept.origins {
                    let input = row.input.clone().unwrap_or_default();
                    println!(
                        "{name}: origin {} {} {} · {} rows landed from {} input rows",
                        input.origin.unwrap_or_default(), row.run_id, row.status, row.rows, input.rows
                    );
                }
                let summary = format!("{name}: sweep {} {} · {} origins", swept.sweep.execution_id, swept.sweep.status, swept.origins.len());
                if swept.sweep.status != RunStatus::Success {
                    bail!("{summary} — {}", swept.sweep.error_message.unwrap_or_default());
                }
                println!("{summary}");
                return Ok(());
            }
            if let Some(policy) = &driven.isolated_items {
                let mut complete = |row:&contextful_core::run::record::RunRow| {
                    if grouped {
                        crate::export::publication::record_source(face.store(),&text,&name,row).map_err(|e|Failure::new(FailureTag::Storage,e.to_string()))?;
                    }
                    Ok(())
                };
                let report = if let Some(plan) = &recorded_plan {
                    engine.drive_items_recorded_with_completion(&drive, policy, plan, &effects, &mut read, &mut |id, emitted, owner| {
                        land_tables(driven, &name, &format!("{run_id}.item-{id}"), &site_id, |table, spec| {
                            dest.normalize = plan.effects().find(|effect| effect.table == table).and_then(|effect| effect.normalize);
                            engine.run_emissions(spec, owner, emitted.get(table).map(Vec::as_slice).unwrap_or_default(), &mut dest).map_err(engine_error)
                        })
                    },&mut complete)?
                } else {
                    engine.drive_items_admitted_with_completion(&drive, policy, &mut read, &mut |id, emitted, owner| {
                        if emitted.keys().any(|table| !driven.tables.contains(table)) {
                            return Err(Failure::deterministic(FailureTag::Permanent, "isolated body emitted an undeclared table"));
                        }
                        land_tables(driven, &name, &format!("{run_id}.item-{id}"), &site_id, |table, spec| {
                            dest.store.validate_writer_plan(&spec.plan, dest.normalize).map_err(|e| Failure::deterministic(FailureTag::Permanent, e.to_string()))?;
                            let mut source = Staged(emitted.get(table).cloned().unwrap_or_default(), 0);
                            engine.run_body_rows(spec, owner, &mut source, &mut dest).map_err(engine_error)
                        })
                    },&mut complete)?
                };
                let (mut successful, mut failed, mut parked, mut deferred) = (0, 0, 0, 0);
                for item in &report.items {
                    use contextful_engine::drive::ItemStatus;
                    let status = match &item.status {
                        ItemStatus::Succeeded(_) => { successful += 1; "success" },
                        ItemStatus::Failed(_) => { failed += 1; "failed" },
                        ItemStatus::Parked => { parked += 1; "parked" },
                        ItemStatus::Deferred => { deferred += 1; "deferred" },
                    };
                    println!("{name}: item {} execution {} {status}", item.logical_id, item.execution_id);
                }
                let summary = format!("{name}: {successful} successful items, {failed} failed, {parked} parked, {deferred} deferred");
                if !report.succeeded() { bail!("{summary}"); }
                println!("{summary}");
                return Ok(());
            }
            let row = if let Some(plan) = &recorded_plan {
                let mut land = |emitted: &contextful_core::run::drive::PreparedEmitted, owner: &contextful_engine::drive::BodyOwner| -> Result<Landed, Failure> {
                    let landed = land_tables(driven, &name, &run_id, &site_id, |table, spec| {
                        dest.normalize = plan.effects().find(|effect| effect.table == table).and_then(|effect| effect.normalize);
                        engine.run_emissions(spec, owner, emitted.get(table).map(Vec::as_slice).unwrap_or_default(), &mut dest).map_err(engine_error)
                    })?;
                    if grouped {
                        let row = engine.catalog.run(&run_id)?.ok_or_else(||Failure::new(FailureTag::Storage,"publication producer run is absent"))?;
                        crate::export::publication::record_source(face.store(),&text,&name,&row).map_err(|e|Failure::new(FailureTag::Storage,e.to_string()))?;
                    }
                    Ok(landed)
                };
                engine.drive_recorded(&drive, plan, &effects, &mut read, &mut land)?
            } else {
            let mut land = |emitted: &Emitted| -> Result<Landed, Failure> {
                if let Some(table) = emitted.keys().find(|t| !driven.tables.contains(t)) {
                    return Err(Failure::deterministic(FailureTag::Permanent, format!("body `{}` emitted rows for `{table}`, which job `{name}` does not declare in `tables`", driven.input.body)));
                }
                let landed = land_tables(driven, &name, &run_id, &site_id, |table, spec| {
                    dest.store.validate_writer_plan(&spec.plan, dest.normalize).map_err(|e| Failure::deterministic(FailureTag::Permanent, e.to_string()))?;
                    let mut source: Box<dyn Source> = Box::new(Staged(emitted.get(table).cloned().unwrap_or_default(), 0));
                    engine.run_with(spec, &mut source, &Unshaped, &mut dest).map_err(|e| Failure::new(FailureTag::Storage, e.to_string()))
                })?;
                if grouped {
                    let row = engine.catalog.run(&run_id)?.ok_or_else(||Failure::new(FailureTag::Storage,"publication producer run is absent"))?;
                    crate::export::publication::record_source(face.store(),&text,&name,&row).map_err(|e|Failure::new(FailureTag::Storage,e.to_string()))?;
                }
                Ok(landed)
            };

            w.engine.drive(&drive, &mut read, &mut land)?
            };
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

/// The compiled plan a plan job names, read relative to its manifest's directory.
fn compile_plan(declaration: &std::path::Path, declared: &PlanJob) -> Result<NodePlan> {
    let path = declaration.parent().unwrap_or(std::path::Path::new(".")).join(&declared.path);
    let bytes = std::fs::read(&path).with_context(|| format!("reading the plan `{}`", path.display()))?;
    NodePlan::compile(&bytes).with_context(|| path.display().to_string())
}

/// Fire one plan job: compile its plan and lower it onto one execution under
/// `job:<name>`, each step's connector command running beside the manifest
/// (`surface.fire.plan-job`, `run.compile.lowering`).
#[allow(clippy::too_many_arguments)]
fn fire_plan(
    l: &crate::project::Located, project: &ProjectArgs, text: &str, name: &str, declared: &PlanJob,
    run_id: Option<String>, site_id: Option<String>, site_id_env: Option<String>,
) -> Result<()> {
    let plan = compile_plan(&l.declaration, declared)?;
    let site_id = site_id_for(text, &l.declaration, site_id, site_id_env)?;
    let w = wire_at(&l.project, &project.now)?;
    for reaped in w.engine.reap_orphans()? {
        eprintln!("{reaped}: reaped as partial_failure, its owner lease lapsed");
    }
    let run_id = run_id.unwrap_or_else(|| format!("plan-{}", &sha256_hex(format!("{}{}", w.clock.now().unix_nanos(), std::process::id()).as_bytes())[..12]));
    let cwd = l.declaration.parent().filter(|d| !d.as_os_str().is_empty()).map(std::path::Path::to_path_buf).unwrap_or_else(|| ".".into());
    let fire = PlanFire { job: name.to_string(), plan: &plan, run_id, site_id, pid: std::process::id(), boot_id: boot_id(), poll: PARKED_POLL };
    let row = w.engine.run_plan(&fire, &CommandConnectors { cwd })?;
    if row.status != RunStatus::Success {
        bail!("{name}: {} {} — {}", row.run_id, row.status, row.error_message.unwrap_or_default());
    }
    let partial: Vec<String> = row.failed_branches.iter().map(|b| format!("{} {}", b.label, b.tag.name())).collect();
    if partial.is_empty() {
        println!("{name}: {} success · plan {} version {}", row.run_id, plan.id, plan.version);
    } else {
        println!("{name}: {} success · plan {} version {} · partial join, failed branches: {}", row.run_id, plan.id, plan.version, partial.join(", "));
    }
    Ok(())
}

/// Bind each job's target to the tables the manifest's pipelines and store-driven jobs
/// produce (`surface.fire.target-unbound`).
fn bind(jobs: &[Job], declaration: &std::path::Path) -> Result<()> {
    let specs: Vec<_> = contextful_core::pipeline::declare::collect(&crate::pipeline::manifests(declaration)?)?.into_iter().map(|d| d.spec).collect();
    let models = crate::model_source::collect(&crate::pipeline::manifests(declaration)?)?;
    bind_declared(jobs, &specs, &models).with_context(|| declaration.display().to_string())?;
    Ok(())
}


pub(crate) fn bind_declared(jobs: &[Job], specs: &[contextful_core::pipeline::declare::PipelineSpec], models: &[contextful_core::pipeline::model::ModelSpec]) -> Result<()> {
    let targets = Targets::new(specs, jobs, models);
    bind_targets(jobs, &targets)?;
    contextful_core::job::dependent_jobs(specs, jobs)?;
    Ok(())
}
