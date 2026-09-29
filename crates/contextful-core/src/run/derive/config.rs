//! `run.select` and `run.bind`: the derive source's configuration, the machine's binding
//! of an engine name, and the checks each is held to before any unit runs.

use super::task::{Tasks, BUILT_IN_TASKS};
use crate::run::RunError;
use serde::Deserialize;
use serde_json::{json, Map, Value};
use std::collections::BTreeMap;

/// Units one run attempts: 25 rows (`run.select.rows-per-run`).
pub const ROWS_PER_RUN: i64 = 25;
/// Attempts one unit receives before its marker settles: 3 attempts (`run.select.attempts-per-unit`).
pub const ATTEMPTS_PER_UNIT: i64 = 3;
/// Wall clock the unit loop holds for `link_preview`: 300 s (`run.select.seconds-per-run`).
pub const LINK_SECONDS_PER_RUN: u64 = 300;

/// The keys a derive source reads.
pub const KEYS: [&str; 10] =
    ["engine", "source_table", "media_column", "parent_id_column", "task", "max_rows_per_run", "max_attempts", "max_seconds_per_run", "journal", "grant"];
/// Keys that belong to the machine's binding, never to the manifest (`run.bind.command-in-manifest`).
pub const MACHINE_KEYS: [&str; 4] = ["command", "preprocess", "env", "allow_hosts"];

/// What a derive pipeline does with a unit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Task {
    Transcribe,
    LinkPreview,
    /// A compiled task the embedding binary registered under this name (`run.bind.host-task`).
    Host(String),
}

impl Task {
    pub fn name(&self) -> &str {
        match self {
            Task::Transcribe => "transcribe",
            Task::LinkPreview => "link_preview",
            Task::Host(name) => name,
        }
    }

    pub fn is_host(&self) -> bool {
        matches!(self, Task::Host(_))
    }
}

/// A derive source's configuration, from `[pipeline.source.config]`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeriveConfig {
    pub engine: String,
    pub source_table: String,
    pub media_column: String,
    pub parent_id_column: String,
    pub task: Task,
    pub max_rows_per_run: i64,
    pub max_attempts: i64,
    pub max_seconds_per_run: Option<u64>,
}

fn int(cfg: &Map<String, Value>, key: &str) -> Option<i64> {
    cfg.get(key).and_then(Value::as_i64)
}

impl DeriveConfig {
    /// Parse a derive source's configuration for pipeline `pipeline_id`, with no host task registered.
    pub fn parse(pipeline_id: &str, config: &Value) -> Result<DeriveConfig, RunError> {
        DeriveConfig::parse_with(pipeline_id, config, &Tasks::default())
    }

    /// Parse a derive source's configuration for pipeline `pipeline_id`, resolving `task`
    /// against the built-in tasks and those `tasks` registers (`run.bind.unknown-task`).
    pub fn parse_with(pipeline_id: &str, config: &Value, tasks: &Tasks) -> Result<DeriveConfig, RunError> {
        let empty = Map::new();
        let cfg = config.as_object().unwrap_or(&empty);
        if let Some(k) = cfg.keys().find(|k| MACHINE_KEYS.contains(&k.as_str())) {
            return Err(RunError::DeriveCommandInManifest(format!(
                "pipeline `{pipeline_id}` sets `{k}` in `[pipeline.source.config]`; what an engine executes belongs to the machine's `[derive.<name>]` block"
            )));
        }
        if let Some(k) = cfg.keys().find(|k| ["output_table", "pipeline_id", "output_pipeline"].contains(&k.as_str())) {
            return Err(RunError::DeriveForeignOutputTable(format!(
                "pipeline `{pipeline_id}` sets `{k}`; a derive source anti-joins against its own pipeline's output, fixed at build"
            )));
        }
        if let Some(k) = cfg.keys().find(|k| !KEYS.contains(&k.as_str())) {
            return Err(RunError::PipelineUnknownConfigKey(format!("the `derive` source reads no key `{k}`; it reads {}", KEYS.join(", "))));
        }
        let required = |key: &str| -> Result<String, RunError> {
            match cfg.get(key).and_then(Value::as_str).map(str::trim) {
                Some(v) if !v.is_empty() => Ok(v.to_string()),
                _ => Err(RunError::DeriveConfigKeyMissing(format!("pipeline `{pipeline_id}` declares no `{key}`"))),
            }
        };
        let task = match cfg.get("task").and_then(Value::as_str) {
            None | Some("transcribe") => Task::Transcribe,
            Some("link_preview") => Task::LinkPreview,
            Some(name) if tasks.get(name).is_some() => Task::Host(name.to_string()),
            Some(other) => {
                let registered = tasks.names();
                return Err(RunError::DeriveUnknownTask(format!(
                    "task `{other}` is none of the built-in tasks {} or the registered tasks {}",
                    BUILT_IN_TASKS.join(", "),
                    if registered.is_empty() { "(none)".to_string() } else { registered.join(", ") }
                )));
            }
        };
        let source_table = required("source_table")?;
        let parent_id_column = required("parent_id_column")?;
        // A host task reads the parent columns it declares and binds no engine (`run.bind.driver-mismatch`).
        let (engine, media_column) = if task.is_host() {
            if let Some(engine) = cfg.get("engine") {
                return Err(RunError::DeriveDriverMismatch(format!(
                    "pipeline `{pipeline_id}` names engine {engine} for host task `{}`, which is compiled code and binds no engine",
                    task.name()
                )));
            }
            let media = cfg.get("media_column").and_then(Value::as_str).map(str::trim).unwrap_or_default().to_string();
            (String::new(), media)
        } else {
            (required("engine")?, required("media_column")?)
        };
        if cfg.get("journal").and_then(Value::as_bool) == Some(true) {
            return Err(RunError::DeriveJournaledPull(format!(
                "pipeline `{pipeline_id}` journals its pulls; a derive pipeline re-reads the store each tick and records no pull"
            )));
        }
        let positive_or = |v: Option<i64>, default: i64| v.filter(|n| *n > 0).unwrap_or(default);
        let seconds = int(cfg, "max_seconds_per_run").filter(|n| *n > 0).map(|n| n as u64);
        Ok(DeriveConfig {
            engine,
            source_table,
            media_column,
            parent_id_column,
            task: task.clone(),
            max_rows_per_run: positive_or(int(cfg, "max_rows_per_run"), ROWS_PER_RUN),
            max_attempts: positive_or(int(cfg, "max_attempts"), ATTEMPTS_PER_UNIT),
            max_seconds_per_run: seconds.or(match &task {
                Task::LinkPreview => Some(LINK_SECONDS_PER_RUN),
                Task::Transcribe | Task::Host(_) => None,
            }),
        })
    }
}

/// How a cue document an engine writes is spelled.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum OutputFormat {
    Srt,
    Vtt,
}

impl OutputFormat {
    pub fn name(self) -> &'static str {
        match self {
            OutputFormat::Srt => "srt",
            OutputFormat::Vtt => "vtt",
        }
    }
}

/// One step of an exec chain.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StepSpec {
    /// An argument array; a string refuses (`run.exec.shell-command`).
    pub command: Value,
    #[serde(default)]
    pub sha256: Option<String>,
    #[serde(default)]
    pub output_path: Option<String>,
    #[serde(default)]
    pub output_format: Option<OutputFormat>,
    #[serde(default)]
    pub when: Option<String>,
}

impl StepSpec {
    /// Every key of the step, as a derivation key reads it.
    fn params(&self) -> Value {
        json!({
            "command": self.command, "sha256": self.sha256, "output_path": self.output_path,
            "output_format": self.output_format.map(OutputFormat::name), "when": self.when,
        })
    }

    /// The step's argument array.
    pub fn argv(&self, engine: &str) -> Result<Vec<String>, RunError> {
        match &self.command {
            Value::Array(items) if !items.is_empty() && items.iter().all(Value::is_string) => {
                Ok(items.iter().filter_map(|v| v.as_str().map(str::to_string)).collect())
            }
            Value::String(_) => Err(RunError::DeriveShellCommand(format!(
                "engine `{engine}` gives `command` as one string; a step is an argument array run with no shell"
            ))),
            _ => Err(RunError::Invalid(format!("engine `{engine}`: `command` is a non-empty array of strings"))),
        }
    }
}

/// A `[derive.<name>]` block: the machine's half of a binding.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Binding {
    #[serde(default = "exec")]
    pub driver: String,
    #[serde(default)]
    pub timeout_secs: Option<u64>,
    #[serde(default)]
    pub max_output_bytes: Option<u64>,
    /// Advisory: the adapter, not this key, writes a row's zone.
    #[serde(default)]
    pub zone: Option<String>,
    /// The directory local media resolves under, relative to the working directory.
    #[serde(default)]
    pub media_root: Option<String>,
    #[serde(default)]
    pub preprocess: Vec<StepSpec>,
    #[serde(default)]
    pub engine: Option<StepSpec>,
    /// Variable name to value template, the only environment a step receives.
    #[serde(default)]
    pub env: BTreeMap<String, String>,
    #[serde(default)]
    pub allow_hosts: Vec<String>,
    #[serde(default)]
    pub allow_image_hosts: Vec<String>,
    #[serde(default)]
    pub request_timeout_secs: Option<u64>,
}

fn exec() -> String {
    "exec".into()
}

impl Binding {
    /// The binding less its bounds and advisory `zone`: the parameters a derivation key reads
    /// (`run.emit.derivation-key`). A bound decides whether a unit finishes, not what it derives.
    pub fn derivation_params(&self) -> Value {
        json!({
            "driver": self.driver,
            "media_root": self.media_root,
            "preprocess": self.preprocess.iter().map(StepSpec::params).collect::<Vec<_>>(),
            "engine": self.engine.as_ref().map(StepSpec::params),
            "env": self.env,
            "allow_hosts": self.allow_hosts,
            "allow_image_hosts": self.allow_image_hosts,
        })
    }
}

/// Every `[derive.<name>]` block of a manifest.
pub fn bindings(toml_text: &str) -> Result<BTreeMap<String, Binding>, RunError> {
    let v: toml::Value = toml::from_str(toml_text).map_err(|e| RunError::Invalid(format!("the manifest: {}", e.message())))?;
    let Some(table) = v.get("derive").and_then(toml::Value::as_table) else { return Ok(BTreeMap::new()) };
    table
        .iter()
        .map(|(name, block)| {
            let b: Binding = block.clone().try_into().map_err(|e: toml::de::Error| RunError::Invalid(format!("`[derive.{name}]`: {}", e.message())))?;
            Ok((name.clone(), b))
        })
        .collect()
}

/// Resolve the pipeline's engine against the machine's bindings and hold the pair to
/// the task (`run.bind.unbound-engine`, `run.bind.driver-mismatch`).
pub fn bind<'a>(pipeline_id: &str, config: &DeriveConfig, bindings: &'a BTreeMap<String, Binding>, manifest: &str) -> Result<&'a Binding, RunError> {
    let b = bindings.get(&config.engine).ok_or_else(|| {
        let bound: Vec<&str> = bindings.keys().map(String::as_str).collect();
        RunError::DeriveEngineUnbound(format!(
            "pipeline `{pipeline_id}` names engine `{}`, and `{manifest}` defines no `[derive.{}]` block; bound engines: {}",
            config.engine,
            config.engine,
            if bound.is_empty() { "none".to_string() } else { bound.join(", ") }
        ))
    })?;
    let fits = match config.task {
        Task::Transcribe => b.driver == "exec" || b.driver == "none",
        Task::LinkPreview => b.driver == "fetch",
        Task::Host(_) => false,
    };
    if !fits {
        return Err(RunError::DeriveDriverMismatch(format!(
            "engine `{}` runs driver `{}`, which does not serve task `{}`",
            config.engine,
            b.driver,
            config.task.name()
        )));
    }
    Ok(b)
}

/// Hold an environment allowlist name to ASCII alphanumerics and underscore, not leading
/// with a digit (`run.exec.env-name`).
pub fn check_env_name(name: &str) -> Result<(), RunError> {
    let ok = !name.is_empty() && name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_') && !name.as_bytes()[0].is_ascii_digit();
    if ok {
        Ok(())
    } else {
        Err(RunError::DeriveEnvNameInvalid(format!("`{name}` is not a variable name of ASCII alphanumerics and underscore leading with no digit")))
    }
}

/// The key every derive output table declares (`run.emit.primary-key`).
pub const DERIVE_PRIMARY_KEY: [&str; 3] = ["unit_ref", super::emit::DERIVATION_KEY, "cue_seq"];

/// Refuse a derive output table whose key is not [`DERIVE_PRIMARY_KEY`], or that names the
/// reserved `kind` column (`run.emit.primary-key`, `run.emit.reserved-discriminator`).
pub fn check_output_table(table: &crate::store::declare::TableDecl) -> Result<(), RunError> {
    let named = table.primary_key().iter().chain(table.order_by.iter()).chain(table.cluster_by()).chain(table.partition_by());
    if named.into_iter().any(|c| c == super::emit::KIND) {
        return Err(RunError::DeriveReservedDiscriminator(format!(
            "derive table `{}` declares column `kind`, which the tier writes to tell passages from markers",
            table.name
        )));
    }
    if table.primary_key() != DERIVE_PRIMARY_KEY {
        return Err(RunError::DerivePrimaryKeyMissing(format!(
            "derive table `{}` declares `primary_key = {:?}`; a derive table keys on [\"unit_ref\", \"derivation_key\", \"cue_seq\"]",
            table.name,
            table.primary_key()
        )));
    }
    Ok(())
}
