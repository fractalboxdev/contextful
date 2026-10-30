//! `surface.fire`'s job blocks: the closed kind union a `[[job]]` block names, and the
//! store-driven kind's declaration. A block selects among kinds the engine names and bodies
//! the embedding binary registers; it never carries a command (`surface.fire.job-kind-unknown`).

use crate::memory::declare::{MemoryDeclarations, Shape};
use crate::pipeline::declare::PipelineSpec;
use crate::run::drive::{ItemPolicy, StoreInput};
use crate::run::RunError;
use crate::run::sweep::Sweep;
use serde::Deserialize;
use std::collections::BTreeMap;

/// The closed union of job kinds, beside the pipeline-run kind.
pub const KINDS: [&str; 9] = ["sweep", "build", "fold", "rebuild-catalog", "sync-push", "validate", STORE_DRIVEN, SYNTHESIZE, PLAN];

/// The kind lowering a compiled node plan onto one host execution
/// (`surface.fire.plan-job`).
pub const PLAN: &str = "plan";

/// The kind running one synthesis pass from a source table into the claims table it
/// targets (`read.synthesize.cadence`).
pub const SYNTHESIZE: &str = "synthesize";

/// The kind whose per-row body is compiled code the embedding binary registers.
pub const STORE_DRIVEN: &str = "store-driven";

/// The maintenance kind running one fold pass over its target (`store.fold.job-pass`).
pub const FOLD: &str = "fold";

/// The maintenance kind pushing the store to its `[sync]` bucket (`store.push.job-push`).
pub const SYNC_PUSH: &str = "sync-push";

/// The maintenance kinds no fire runs, each with the command that does the work instead
/// (`surface.fire.kind-undispatched`).
pub const UNDISPATCHED: [(&str, &str); 3] = [
    ("sweep", "no command runs a sweep job"),
    ("rebuild-catalog", "`contextful context rebuild-catalog` repairs a lost derived catalog"),
    ("validate", "`contextful pipeline validate` checks a declaration before apply"),
];

/// Keys that hand a block an argument vector or a host command.
const COMMAND_KEYS: [&str; 5] = ["command", "argv", "args", "exec", "cmd"];

/// The refusals of job-block validation. `Display` begins with the identifier.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum JobError {
    /// A job block naming a kind outside the union, or carrying a command. (`surface.fire.job-kind-unknown`)
    #[error("JobKindUnknown: {0}")]
    JobKindUnknown(String),
    /// A store-driven job block declaring no positive `max_in_flight`. (`surface.fire.store-driven-concurrency`)
    #[error("JobConcurrencyUnset: {0}")]
    JobConcurrencyUnset(String),
    /// A store-driven job block naming a body the binary does not register. (`surface.fire.store-driven-body`)
    #[error("JobBodyUnregistered: {0}")]
    JobBodyUnregistered(String),
    /// A store-driven body naming a delegate the daemon's configuration does not bind. (`surface.delegate.binding`)
    #[error("JobDelegateUnbound: {0}")]
    JobDelegateUnbound(String),
    /// A job block naming a maintenance kind no fire runs. (`surface.fire.kind-undispatched`)
    #[error("JobKindUndispatched: {0}")]
    JobKindUndispatched(String),
    /// A job target binding to no produced table or declared model. (`surface.fire.target-unbound`)
    #[error("JobTargetUnbound: {0}")]
    JobTargetUnbound(String),
    /// A refusal of the run contract a job target raises.
    #[error(transparent)]
    Run(#[from] RunError),
    /// A block that does not parse, or lacks a key its kind requires.
    #[error("{0}")]
    Invalid(String),
}

/// One `[[job]]` block as written.
#[derive(Debug, Clone, Deserialize)]
struct Block {
    name: String,
    kind: String,
    #[serde(default)]
    schedule: Option<String>,
    #[serde(default)]
    target: Option<String>,
    #[serde(default)]
    after: Option<String>,
    /// A registered body's name, or `{ delegate = "<name>" }` (`surface.delegate.binding`).
    #[serde(default)]
    body: Option<toml::Value>,
    #[serde(default)]
    statement: Option<String>,
    #[serde(default)]
    as_of: Option<String>,
    #[serde(default)]
    max_in_flight: Option<toml::Value>,
    #[serde(default)]
    tables: Vec<String>,
    #[serde(default)]
    isolated_items: Option<ItemPolicy>,
    #[serde(default)]
    sweep: Option<Sweep>,
    #[serde(default)]
    source: Option<String>,
    #[serde(default)]
    endpoint: Option<String>,
    #[serde(default)]
    model: Option<String>,
    #[serde(default)]
    plan: Option<String>,
    #[serde(flatten)]
    other: BTreeMap<String, toml::Value>,
}

#[derive(Deserialize)]
struct Manifest {
    #[serde(default)]
    job: Vec<toml::Value>,
}

/// A store-driven job's declaration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoreDriven {
    pub input: StoreInput,
    pub max_in_flight: usize,
    /// The tables the body's rows land in.
    pub tables: Vec<String>,
    pub isolated_items: Option<ItemPolicy>,
    /// One run per origin (`run.sweep.origins`).
    pub sweep: Option<Box<Sweep>>,
}

/// A synthesize job's declaration: the source table a pass reads and the inference
/// endpoint and model it extracts through; the job's `target` names the claims table.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Synthesis {
    pub source: String,
    pub endpoint: String,
    pub model: String,
}

/// A plan job's declaration: the compiled plan file, relative to its manifest's directory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlanJob {
    pub path: String,
}

/// A job's kind: one of the union, with the store-driven kind's declaration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum JobKind {
    /// A maintenance kind the engine names, spelled as the union spells it.
    Maintenance(String),
    StoreDriven(StoreDriven),
    Synthesize(Synthesis),
    Plan(PlanJob),
}

/// One validated job block.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Job {
    pub name: String,
    pub schedule: Option<String>,
    pub target: Option<String>,
    /// Build after this pipeline, under its dependent-run head schedule.
    pub after: Option<String>,
    pub kind: JobKind,
}

/// Refuse a job of a kind no fire runs, naming the command that does its work.
fn undispatched(name: &str, kind: &str) -> Result<(), JobError> {
    match UNDISPATCHED.iter().find(|(k, _)| *k == kind) {
        Some((_, instead)) => Err(JobError::JobKindUndispatched(format!("job `{name}` names kind `{kind}`, which no fire runs; {instead}"))),
        None => Ok(()),
    }
}

/// Refuse each `[[job]]` block of `manifest` naming a kind of [`UNDISPATCHED`]; an
/// applied snapshot holding one still parses and arms it nowhere
/// (`surface.fire.kind-undispatched`).
pub fn refuse_undispatched(manifest: &str) -> Result<(), JobError> {
    let parsed: Manifest = toml::from_str(manifest).map_err(|e| JobError::Invalid(format!("the manifest does not parse: {e}")))?;
    for block in &parsed.job {
        let name = block.get("name").and_then(|v| v.as_str()).unwrap_or("(unnamed)");
        if let Some(kind) = block.get("kind").and_then(|v| v.as_str()) {
            undispatched(name, kind)?;
        }
    }
    Ok(())
}

impl Job {
    /// Refuse this job when its kind is one no fire runs (`surface.fire.kind-undispatched`).
    pub fn refuse_undispatched(&self) -> Result<(), JobError> {
        undispatched(&self.name, self.kind_name())
    }

    /// Whether `pipeline serve` arms this job on its schedule: the kinds a `job fire`
    /// child runs to completion (`surface.fire.scheduled-maintenance`).
    pub fn dispatched(&self) -> bool {
        match &self.kind {
            JobKind::StoreDriven(_) | JobKind::Synthesize(_) => true,
            JobKind::Maintenance(kind) => matches!(kind.as_str(), "build" | FOLD | SYNC_PUSH),
            JobKind::Plan(_) => false,
        }
    }

    /// The kind as the block spells it.
    pub fn kind_name(&self) -> &str {
        match &self.kind {
            JobKind::Maintenance(k) => k,
            JobKind::StoreDriven(_) => STORE_DRIVEN,
            JobKind::Synthesize(_) => SYNTHESIZE,
            JobKind::Plan(_) => PLAN,
        }
    }

    /// The tables one synthesize fire reads and writes, which its fire credential grants
    /// (`surface.dispatch.fire-reach`); `None` for any other kind, whose reach the
    /// declaration alone does not fix.
    pub fn reach(&self, memory: &MemoryDeclarations) -> Option<Reach> {
        let (JobKind::Synthesize(synthesis), Some(target)) = (&self.kind, &self.target) else { return None };
        let mut read = vec![synthesis.source.clone(), target.clone()];
        // The one entities table synthesis resolves entities against.
        if let Some(entities) = memory.of_shape(Shape::Entities).filter(|t| !read.contains(&t.name)) {
            read.push(entities.name.clone());
        }
        Some(Reach { read, write: vec![target.clone()] })
    }
}

/// The tables one fire reads and writes.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Reach {
    pub read: Vec<String>,
    pub write: Vec<String>,
}

/// Every `[[job]]` block of `manifest`, validated; `registered` answers whether the
/// embedding binary registers a body name.
pub fn parse_jobs(manifest: &str, registered: &dyn Fn(&str) -> bool) -> Result<Vec<Job>, JobError> {
    let parsed: Manifest = toml::from_str(manifest).map_err(|e| JobError::Invalid(format!("the manifest does not parse: {e}")))?;
    let mut names = std::collections::BTreeSet::new();
    let mut jobs = Vec::new();
    for value in parsed.job {
        let job = check(value, registered)?;
        if !names.insert(job.name.clone()) {
            return Err(JobError::Invalid(format!("job `{}` is declared more than once", job.name)));
        }
        jobs.push(job);
    }
    Ok(jobs)
}

fn check(value: toml::Value, registered: &dyn Fn(&str) -> bool) -> Result<Job, JobError> {
    let name = value.get("name").and_then(|v| v.as_str()).unwrap_or("(unnamed)").to_string();
    if let Some(key) = COMMAND_KEYS.iter().find(|k| value.get(**k).is_some()) {
        return Err(JobError::JobKindUnknown(format!("job `{name}` carries `{key}`; a job names a kind of the union and never a command")));
    }
    let block: Block = value.try_into().map_err(|e| JobError::Invalid(format!("job `{name}`: {e}")))?;
    if !KINDS.contains(&block.kind.as_str()) {
        return Err(JobError::JobKindUnknown(format!("job `{name}` names kind `{}`; the union is {}", block.kind, KINDS.join(", "))));
    }
    if block.after.is_some() && (block.kind != "build" || block.schedule.is_some()) {
        return Err(JobError::Invalid(format!("job `{name}`: `after` requires a build without an independent schedule")));
    }
    let kind = match block.kind.as_str() {
        STORE_DRIVEN => JobKind::StoreDriven(store_driven(&block, registered)?),
        SYNTHESIZE => JobKind::Synthesize(synthesis(&block)?),
        PLAN => JobKind::Plan(PlanJob {
            path: block.plan.clone().filter(|p| !p.trim().is_empty()).ok_or_else(|| JobError::Invalid(format!("job `{name}` of kind `{PLAN}` names no `plan` file")))?,
        }),
        _ => JobKind::Maintenance(block.kind.clone()),
    };
    if !matches!(kind, JobKind::Synthesize(_)) {
        let carried = [("source", &block.source), ("endpoint", &block.endpoint), ("model", &block.model)];
        if let Some((key, _)) = carried.into_iter().find(|(_, v)| v.is_some()) {
            return Err(JobError::Invalid(format!("job `{name}` carries unknown key `{key}`")));
        }
    }
    if !matches!(kind, JobKind::Plan(_)) && block.plan.is_some() {
        return Err(JobError::Invalid(format!("job `{name}` carries unknown key `plan`")));
    }
    if !matches!(kind, JobKind::StoreDriven(_)) && block.isolated_items.is_some() {
        return Err(JobError::Invalid(format!("job `{name}` carries unknown key `isolated_items`")));
    }
    if !matches!(kind, JobKind::StoreDriven(_)) && block.sweep.is_some() {
        return Err(JobError::Invalid(format!("job `{name}` carries unknown key `sweep`")));
    }
    if let Some(key) = block.other.keys().next() {
        return Err(JobError::Invalid(format!("job `{name}` carries unknown key `{key}`")));
    }
    Ok(Job { name: block.name, schedule: block.schedule, target: block.target, after: block.after, kind })
}

/// A synthesize block names its claims table as `target`, and its `source`, `endpoint`
/// and `model` (`read.synthesize.cadence`).
fn synthesis(block: &Block) -> Result<Synthesis, JobError> {
    let name = &block.name;
    let required = |key: &str, value: &Option<String>| {
        value.clone().filter(|v| !v.trim().is_empty()).ok_or_else(|| JobError::Invalid(format!("job `{name}` of kind `{SYNTHESIZE}` declares no `{key}`")))
    };
    required("target", &block.target)?;
    Ok(Synthesis { source: required("source", &block.source)?, endpoint: required("endpoint", &block.endpoint)?, model: required("model", &block.model)? })
}

fn store_driven(block: &Block, registered: &dyn Fn(&str) -> bool) -> Result<StoreDriven, JobError> {
    let name = &block.name;
    let max_in_flight = match &block.max_in_flight {
        Some(toml::Value::Integer(n)) if *n > 0 => usize::try_from(*n).map_err(|_| JobError::JobConcurrencyUnset(format!("job `{name}`: `max_in_flight = {n}` does not fit")))?,
        Some(other) => {
            return Err(JobError::JobConcurrencyUnset(format!("job `{name}` declares `max_in_flight = {other}`; it takes a positive integer")));
        }
        None => {
            return Err(JobError::JobConcurrencyUnset(format!("job `{name}` declares no `max_in_flight`; a store-driven job states its concurrency, and none defaults")));
        }
    };
    let body = match &block.body {
        Some(toml::Value::String(body)) if !body.trim().is_empty() => {
            if !registered(body) {
                return Err(JobError::JobBodyUnregistered(format!("job `{name}` names body `{body}`, which this binary does not register")));
            }
            body.clone()
        }
        Some(toml::Value::Table(table)) => {
            let delegate = table
                .get("delegate")
                .and_then(|v| v.as_str())
                .filter(|d| !d.trim().is_empty() && table.len() == 1)
                .ok_or_else(|| JobError::Invalid(format!("job `{name}`: `body` is a registered body's name or `{{ delegate = \"<name>\" }}`")))?;
            let body = format!("{}{delegate}", crate::surface::delegate::BODY_PREFIX);
            if !registered(&body) {
                return Err(JobError::JobDelegateUnbound(format!("job `{name}` names delegate `{delegate}`, which `[control.delegates]` does not bind")));
            }
            body
        }
        _ => return Err(JobError::Invalid(format!("job `{name}` names no `body`"))),
    };
    let statement = block.statement.clone().filter(|s| !s.trim().is_empty()).ok_or_else(|| JobError::Invalid(format!("job `{name}` declares no `statement`")))?;
    if let Some(as_of) = &block.as_of {
        crate::store::bound_time::Bound::parse(as_of).map_err(|e| JobError::Invalid(format!("job `{name}`: `as_of`: {e}")))?;
    }
    if block.tables.is_empty() {
        return Err(JobError::Invalid(format!("job `{name}` declares no output `tables`")));
    }
    if let Some(policy) = &block.isolated_items { policy.validate().map_err(|e| JobError::Invalid(e.to_string()))?; }
    if let Some(sweep) = &block.sweep {
        if block.isolated_items.is_some() {
            return Err(JobError::Invalid(format!("job `{name}` declares both `sweep` and `isolated_items`")));
        }
        // A declared `as_of` holds the sweep's end at validation (`run.sweep.origin-after-open`).
        let declared = block.as_of.as_deref().map(crate::time::Instant::parse).transpose().map_err(|e| JobError::Invalid(format!("job `{name}`: `as_of`: {e}")))?;
        match declared {
            Some(as_of) => sweep.origins_at(as_of).map(drop),
            None => sweep.origins().map(drop),
        }
        .map_err(|e| JobError::Invalid(format!("{e} (job `{name}`)")))?;
    }
    Ok(StoreDriven { input: StoreInput { body, statement, as_of: block.as_of.clone(), sweep: None, structural_identity: None }, max_in_flight, tables: block.tables.clone(), isolated_items:block.isolated_items.clone(), sweep: block.sweep.clone().map(Box::new) })
}

/// The tables a manifest's jobs may target: every destination table its pipelines and
/// store-driven jobs produce, and the separately declared SQL models.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Targets {
    pub produced: std::collections::BTreeSet<String>,
    pub models: std::collections::BTreeSet<String>,
}

impl Targets {
    pub fn new(specs: &[PipelineSpec], jobs: &[Job], models: &[crate::pipeline::model::ModelSpec]) -> Targets {
        let mut t = Targets { produced: Default::default(), models: models.iter().map(|m| m.id.clone()).collect() };
        for spec in specs {
            for entry in &spec.tables {
                let decl = entry.decl();
                let name = spec.table_name(&decl.name);
                t.produced.insert(name);
            }
        }
        for job in jobs {
            if let JobKind::StoreDriven(d) = &job.kind {
                t.produced.extend(d.tables.iter().cloned());
            }
        }
        t
    }
}

/// `name` folded as destination names fold: each non-alphanumeric character to `_`, each
/// ASCII uppercase letter lowered (`run.declare.table-name`).
fn fold_spelling(name: &str) -> String {
    name.chars().map(|c| if c.is_ascii_alphanumeric() { c.to_ascii_lowercase() } else { '_' }).collect()
}

/// Bind each `fold` and `build` target to what the manifest produces
/// (`surface.fire.target-unbound`). A target folding onto a produced table it does not spell
/// raises `PipelineUnboundTableName` with the expected spelling (`run.declare.unbound-table-name`).
pub fn bind_targets(jobs: &[Job], targets: &Targets) -> Result<(), JobError> {
    for job in jobs {
        let Some(target) = &job.target else {
            if job.kind_name() == "build" { return Err(JobError::JobTargetUnbound(format!("build job `{}` requires a model target", job.name))); }
            continue;
        };
        let (set, what) = match job.kind_name() {
            "fold" => (&targets.produced, "a produced table"),
            "build" => (&targets.models, "a declared model"),
            _ => continue,
        };
        if set.contains(target) {
            continue;
        }
        let folded = fold_spelling(target);
        if folded != *target && set.contains(&folded) {
            return Err(RunError::PipelineUnboundTableName(format!("job `{}` targets `{target}`; the fold produces `{folded}`", job.name)).into());
        }
        let known: Vec<&str> = set.iter().map(String::as_str).collect();
        return Err(JobError::JobTargetUnbound(format!(
            "job `{}` ({}) targets `{target}`, which is not {what}; the manifest declares [{}]",
            job.name,
            job.kind_name(),
            known.join(", ")
        )));
    }
    Ok(())
}


/// Add explicitly dependent build jobs to the existing pipeline units. Models do not
/// infer dependencies from SQL and job-to-job dependencies are not admitted.
pub fn dependent_jobs(specs: &[PipelineSpec], jobs: &[Job]) -> Result<crate::pipeline::declare::DependentRuns, JobError> {
    dependent_jobs_joining(specs, jobs, &|_| Vec::new())
}

/// [`dependent_jobs`] over pipeline units ordered by the tables each derive pipeline's host
/// task joins, as `joins` answers them (`run.select.join-order`).
pub fn dependent_jobs_joining(specs: &[PipelineSpec], jobs: &[Job], joins: &dyn Fn(&PipelineSpec) -> Vec<String>) -> Result<crate::pipeline::declare::DependentRuns, JobError> {
    let mut runs = crate::pipeline::declare::dependent_runs_joining(specs.iter(), joins).map_err(|e| JobError::Invalid(e.to_string()))?;
    for job in jobs {
        let Some(parent) = &job.after else { continue };
        if !specs.iter().any(|p| &p.id == parent) {
            return Err(JobError::Invalid(format!("job `{}`: `after` names no declared pipeline `{parent}`", job.name)));
        }
        let id = format!("job:{}", job.name);
        let head = runs.head_of.get(parent).unwrap_or(parent).clone();
        runs.head_of.insert(id.clone(), head.clone());
        runs.steps.entry(head).or_default().push(id);
    }
    Ok(runs)
}
