//! `surface.fire`'s job blocks: the closed kind union a `[[job]]` block names, and the
//! store-driven kind's declaration. A block selects among kinds the engine names and bodies
//! the embedding binary registers; it never carries a command (`surface.fire.job-kind-unknown`).

use crate::pipeline::declare::PipelineSpec;
use crate::run::drive::StoreInput;
use serde::Deserialize;
use std::collections::BTreeMap;

/// The closed union of job kinds, beside the pipeline-run kind.
pub const KINDS: [&str; 7] = ["sweep", "build", "fold", "rebuild-catalog", "sync-push", "validate", STORE_DRIVEN];

/// The kind whose per-row body is compiled code the embedding binary registers.
pub const STORE_DRIVEN: &str = "store-driven";

/// Keys that hand a block an argument vector or a host command.
const COMMAND_KEYS: [&str; 5] = ["command", "argv", "args", "exec", "cmd"];

/// The refusals of job-block validation. `Display` begins with the identifier.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum JobError {
    /// A kind outside the union, an argument vector or a host command. (`surface.fire.job-kind-unknown`)
    #[error("JobKindUnknown: {0}")]
    JobKindUnknown(String),
    /// A store-driven block declaring no positive `max_in_flight`. (`surface.fire.store-driven-concurrency`)
    #[error("JobConcurrencyUnset: {0}")]
    JobConcurrencyUnset(String),
    /// A store-driven block naming a body the binary does not register. (`surface.fire.store-driven-body`)
    #[error("JobBodyUnregistered: {0}")]
    JobBodyUnregistered(String),
    /// A `fold` target naming no produced table, or a `build` target naming no declared model.
    /// (`surface.fire.target-unbound`)
    #[error("JobTargetUnbound: {0}")]
    JobTargetUnbound(String),
    /// A target naming a produced table in a spelling the fold does not produce.
    /// (`run.declare.unbound-table-name`)
    #[error("PipelineUnboundTableName: {0}")]
    PipelineUnboundTableName(String),
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
    body: Option<String>,
    #[serde(default)]
    statement: Option<String>,
    #[serde(default)]
    as_of: Option<String>,
    #[serde(default)]
    max_in_flight: Option<toml::Value>,
    #[serde(default)]
    tables: Vec<String>,
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
}

/// A job's kind: one of the union, with the store-driven kind's declaration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum JobKind {
    /// A maintenance kind the engine names, spelled as the union spells it.
    Maintenance(String),
    StoreDriven(StoreDriven),
}

/// One validated job block.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Job {
    pub name: String,
    pub schedule: Option<String>,
    pub target: Option<String>,
    pub kind: JobKind,
}

impl Job {
    /// The kind as the block spells it.
    pub fn kind_name(&self) -> &str {
        match &self.kind {
            JobKind::Maintenance(k) => k,
            JobKind::StoreDriven(_) => STORE_DRIVEN,
        }
    }
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
    let kind = if block.kind == STORE_DRIVEN { JobKind::StoreDriven(store_driven(&block, registered)?) } else { JobKind::Maintenance(block.kind.clone()) };
    if let Some(key) = block.other.keys().next() {
        return Err(JobError::Invalid(format!("job `{name}` carries unknown key `{key}`")));
    }
    Ok(Job { name: block.name, schedule: block.schedule, target: block.target, kind })
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
    let body = block.body.clone().filter(|b| !b.trim().is_empty()).ok_or_else(|| JobError::Invalid(format!("job `{name}` names no `body`")))?;
    if !registered(&body) {
        return Err(JobError::JobBodyUnregistered(format!("job `{name}` names body `{body}`, which this binary does not register")));
    }
    let statement = block.statement.clone().filter(|s| !s.trim().is_empty()).ok_or_else(|| JobError::Invalid(format!("job `{name}` declares no `statement`")))?;
    if let Some(as_of) = &block.as_of {
        crate::store::bound_time::Bound::parse(as_of).map_err(|e| JobError::Invalid(format!("job `{name}`: `as_of`: {e}")))?;
    }
    if block.tables.is_empty() {
        return Err(JobError::Invalid(format!("job `{name}` declares no output `tables`")));
    }
    Ok(StoreDriven { input: StoreInput { body, statement, as_of: block.as_of.clone() }, max_in_flight, tables: block.tables.clone() })
}

/// The tables a manifest's jobs may target: every destination table its pipelines and
/// store-driven jobs produce, and the declared models among them, the produced tables
/// declaring their `columns` contract.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Targets {
    pub produced: std::collections::BTreeSet<String>,
    pub models: std::collections::BTreeSet<String>,
}

impl Targets {
    pub fn new(specs: &[PipelineSpec], jobs: &[Job]) -> Targets {
        let mut t = Targets::default();
        for spec in specs {
            for entry in &spec.tables {
                let decl = entry.decl();
                let name = spec.table_name(&decl.name);
                if decl.columns.as_ref().is_some_and(|c| !c.is_empty()) {
                    t.models.insert(name.clone());
                }
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
        let Some(target) = &job.target else { continue };
        let (set, what) = match job.kind_name() {
            "fold" => (&targets.produced, "a produced table"),
            "build" => (&targets.models, "a declared model (a produced table declaring its `columns`)"),
            _ => continue,
        };
        if set.contains(target) {
            continue;
        }
        let folded = fold_spelling(target);
        if folded != *target && set.contains(&folded) {
            return Err(JobError::PipelineUnboundTableName(format!("job `{}` targets `{target}`; the fold produces `{folded}`", job.name)));
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
