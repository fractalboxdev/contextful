//! `run.select` and `run.bind`: the derive source's configuration, the machine's binding
//! of an engine name, and the checks each is held to before any unit runs.

use super::task::{Tasks, BUILT_IN_TASKS};
use crate::connector::meter::LimiterDeclaration;
use crate::connector::reference::{check_material, Template};
use crate::connector::ConnectorError;
use crate::read::filter::Filter;
use crate::run::RunError;
use crate::time::Instant;
use serde::Deserialize;
use serde_json::{json, Map, Value};
use std::cmp::Ordering;
use std::collections::BTreeMap;

/// Units one run attempts: 25 rows (`run.select.rows-per-run`).
pub const ROWS_PER_RUN: i64 = 25;
/// Attempts one unit receives before its marker settles: 3 attempts (`run.select.attempts-per-unit`).
pub const ATTEMPTS_PER_UNIT: i64 = 3;
/// Wall clock the unit loop holds for `link_preview`: 300 s (`run.select.seconds-per-run`).
pub const LINK_SECONDS_PER_RUN: u64 = 300;
/// Parent rows selection scans in memory before streaming: 1000000 rows (`run.select.parent-scan`).
pub const PARENT_SCAN_ROWS: u64 = 1_000_000;

/// The keys a derive source reads.
pub const KEYS: [&str; 16] = [
    "engine", "source_table", "media_column", "parent_id_column", "task", "max_rows_per_run", "max_attempts", "max_seconds_per_run", "journal", "grant",
    "sample_rate", "since", "filter", "order_by", "endpoint", "model",
];
/// The keys naming a host task's model endpoint, read only beside a task declaring `pure`
/// false (`run.bind.model-keys`).
pub const MODEL_KEYS: [&str; 2] = ["endpoint", "model"];
/// The keys only a host task reads: which of its units a pass selects (`run.select.sample`,
/// `run.select.since`, `run.select.filter`, `run.select.order`).
pub const HOST_KEYS: [&str; 4] = ["sample_rate", "since", "filter", "order_by"];
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

/// The share of a host task's units a pass selects, in `(0, 1]` (`run.select.sample`).
#[derive(Debug, Clone, Copy, PartialEq, PartialOrd)]
pub struct SampleRate(f64);

/// [`SampleRate::new`] constructs no NaN, so equality is reflexive.
impl Eq for SampleRate {}

impl SampleRate {
    /// The rate `r`, when `0 < r <= 1`.
    pub fn new(r: f64) -> Option<SampleRate> {
        (r > 0.0 && r <= 1.0).then_some(SampleRate(r))
    }

    pub fn get(self) -> f64 {
        self.0
    }

    /// Whether a pass keeps unit `key` of task `task`: the first 8 bytes of
    /// SHA-256(`key` ‖ `task`), read as a big-endian integer `h`, satisfy `h / 2^64 < r`.
    /// The comparison is exact: for an integer `h` it holds when `h < ceil(r × 2^64)`.
    pub fn keeps(self, key: &str, task: &str) -> bool {
        use sha2::{Digest, Sha256};
        let digest = Sha256::new().chain_update(key.as_bytes()).chain_update(task.as_bytes()).finalize();
        let mut head = [0u8; 8];
        head.copy_from_slice(&digest[..8]);
        let threshold = (self.0 * 18_446_744_073_709_551_616.0).ceil() as u128;
        u128::from(u64::from_be_bytes(head)) < threshold
    }
}

/// The parent rows a host task selects: equality and membership conditions over its key
/// and declared columns, in the shape of a ranked read's filter (`run.select.filter`).
#[derive(Debug, Clone, PartialEq, Default)]
pub struct ParentFilter(pub Filter);

/// [`Filter::parse`] reads each number through `as_f64`, which yields no NaN, so equality
/// is reflexive.
impl Eq for ParentFilter {}

/// The order a host task's outstanding units take before the row budget cuts them
/// (`run.select.order`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OrderBy {
    pub column: String,
    pub descending: bool,
}

impl OrderBy {
    /// Compare two units by their cells under the column, nulls and absent cells last in
    /// either direction, ties broken by unit key ascending. Booleans sort before numbers and
    /// numbers before text; numbers compare by value and text by bytes.
    pub fn compare(&self, a: (&str, Option<&Value>), b: (&str, Option<&Value>)) -> Ordering {
        fn rank(v: &Value) -> Option<(u8, &Value)> {
            match v {
                Value::Bool(_) => Some((0, v)),
                Value::Number(_) => Some((1, v)),
                Value::String(_) => Some((2, v)),
                _ => None,
            }
        }
        let cells = match (a.1.and_then(rank), b.1.and_then(rank)) {
            (None, None) => Ordering::Equal,
            (None, Some(_)) => Ordering::Greater,
            (Some(_), None) => Ordering::Less,
            (Some((ra, va)), Some((rb, vb))) => {
                let ascending = ra.cmp(&rb).then_with(|| match (va, vb) {
                    (Value::Bool(x), Value::Bool(y)) => x.cmp(y),
                    (Value::Number(x), Value::Number(y)) => match (x.as_i64(), y.as_i64()) {
                        (Some(x), Some(y)) => x.cmp(&y),
                        _ => x.as_f64().unwrap_or(0.0).total_cmp(&y.as_f64().unwrap_or(0.0)),
                    },
                    (Value::String(x), Value::String(y)) => x.as_bytes().cmp(y.as_bytes()),
                    _ => Ordering::Equal,
                });
                if self.descending { ascending.reverse() } else { ascending }
            }
        };
        cells.then_with(|| a.0.as_bytes().cmp(b.0.as_bytes()))
    }
}

/// The model endpoint a host task declaring `pure` false reaches: the OpenAI-compatible
/// base URL `/chat/completions` hangs off, and the model it serves (`run.bind.model-endpoint`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelEndpoint {
    pub endpoint: String,
    pub model: String,
}

/// A derive source's configuration, from `[pipeline.source.config]`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeriveConfig {
    pub engine: String,
    pub source_table: String,
    pub media_column: String,
    pub parent_id_column: String,
    pub task: Task,
    /// The shared quota a link preview declares, when it has one.
    pub grant: Option<LimiterDeclaration>,
    pub max_rows_per_run: i64,
    pub max_attempts: i64,
    pub max_seconds_per_run: Option<u64>,
    /// The share of a host task's units each pass selects; absent, every unit (`run.select.sample`).
    pub sample_rate: Option<SampleRate>,
    /// The earliest `_ingested_at` at which a host task selects a parent row (`run.select.since`).
    pub since: Option<Instant>,
    /// The parent rows a host task selects; absent, every row (`run.select.filter`).
    pub filter: Option<ParentFilter>,
    /// The order a host task's outstanding units are cut in; absent, scan order (`run.select.order`).
    pub order_by: Option<OrderBy>,
    /// The model endpoint a host task declaring `pure` false reaches; absent, the run binds no
    /// client (`run.bind.model-endpoint`).
    pub model: Option<ModelEndpoint>,
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
        if let Some(k) = cfg.keys().find(|k| !task.is_host() && HOST_KEYS.contains(&k.as_str())) {
            return Err(RunError::PipelineUnknownConfigKey(format!(
                "task `{}` reads no key `{k}`; `sample_rate`, `since`, `filter` and `order_by` select a host task's units",
                task.name()
            )));
        }
        let sample_rate = cfg
            .get("sample_rate")
            .map(|v| {
                v.as_f64().and_then(SampleRate::new).ok_or_else(|| {
                    RunError::PipelineSpecInvalid(format!("pipeline `{pipeline_id}` declares `sample_rate = {v}`; a sample rate is a number in (0, 1]"))
                })
            })
            .transpose()?;
        let since = cfg
            .get("since")
            .map(|v| {
                v.as_str().and_then(|s| Instant::parse(s).ok()).ok_or_else(|| {
                    RunError::PipelineSpecInvalid(format!("pipeline `{pipeline_id}` declares `since = {v}`; `since` is an RFC 3339 instant"))
                })
            })
            .transpose()?;
        let model = model_endpoint(pipeline_id, cfg, &task, tasks)?;
        if task != Task::LinkPreview && cfg.contains_key("grant") {
            return Err(RunError::DeriveUnmeteredGrant(format!(
                "pipeline `{pipeline_id}` declares `grant` for {}, which does not use the shared-quota client", task.name()
            )));
        }
        let grant = cfg.get("grant").map(|value| {
            let block = value.as_object().ok_or_else(|| RunError::Invalid(format!("pipeline `{pipeline_id}` grant is a table")))?;
            if let Some(key) = block.keys().find(|key| !["quota", "class", "usage_headers"].contains(&key.as_str())) {
                return Err(RunError::PipelineUnknownConfigKey(format!("pipeline `{pipeline_id}` grant reads no key `{key}`")));
            }
            let text = |key: &str| {
                block.get(key).and_then(Value::as_str).filter(|value| !value.is_empty()).ok_or_else(|| RunError::Invalid(format!("pipeline `{pipeline_id}` grant names no string `{key}`")))
            };
            let headers = match block.get("usage_headers") {
                None => Vec::new(),
                Some(Value::Array(values)) => values
                    .iter()
                    .map(|value| value.as_str().map(str::to_string).ok_or_else(|| RunError::Invalid(format!("pipeline `{pipeline_id}` grant usage_headers is a list of strings"))))
                    .collect::<Result<Vec<_>, _>>()?,
                Some(_) => return Err(RunError::Invalid(format!("pipeline `{pipeline_id}` grant usage_headers is a list of strings"))),
            };
            LimiterDeclaration::new(text("quota")?, text("class")?, &headers).map_err(|error| RunError::Invalid(error.to_string()))
        }).transpose()?;
        let source_table = required("source_table")?;
        let parent_id_column = required("parent_id_column")?;
        // A host task's selection names its key or a column it declares (`run.select.filter`, `run.select.order`).
        let declared: Vec<String> = match &task {
            Task::Host(name) => std::iter::once(parent_id_column.clone())
                .chain(tasks.get(name).map(|t| t.columns()).unwrap_or_default())
                .collect(),
            _ => Vec::new(),
        };
        let undeclared = |key: &str, column: &str| {
            RunError::PipelineSpecInvalid(format!(
                "pipeline `{pipeline_id}` declares `{key}` over `{column}`, which task `{}` neither keys on nor declares; it reads {}",
                task.name(),
                declared.join(", ")
            ))
        };
        let filter = match cfg.get("filter") {
            None => None,
            Some(v) => {
                let filter = Filter::parse(Some(v), None)
                    .map_err(|e| RunError::PipelineSpecInvalid(format!("pipeline `{pipeline_id}` declares `filter = {v}`: {e}")))?;
                if !v.is_object() {
                    return Err(RunError::PipelineSpecInvalid(format!("pipeline `{pipeline_id}` declares `filter = {v}`; a filter is a table")));
                }
                if let Some(column) = filter.columns().find(|c| !declared.iter().any(|d| d == c)) {
                    return Err(undeclared("filter", column));
                }
                Some(ParentFilter(filter))
            }
        };
        let order_by = match cfg.get("order_by") {
            None => None,
            Some(v) => {
                let words: Vec<&str> = v.as_str().map(|s| s.split_whitespace().collect()).unwrap_or_default();
                let descending = match words.as_slice() {
                    [_] => Some(false),
                    [_, direction] if direction.eq_ignore_ascii_case("asc") => Some(false),
                    [_, direction] if direction.eq_ignore_ascii_case("desc") => Some(true),
                    _ => None,
                };
                let Some(descending) = descending else {
                    return Err(RunError::PipelineSpecInvalid(format!(
                        "pipeline `{pipeline_id}` declares `order_by = {v}`; `order_by` is a column, then `asc` or `desc`"
                    )));
                };
                if !declared.iter().any(|d| d == words[0]) {
                    return Err(undeclared("order_by", words[0]));
                }
                Some(OrderBy { column: words[0].to_string(), descending })
            }
        };
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
            grant,
            max_rows_per_run: positive_or(int(cfg, "max_rows_per_run"), ROWS_PER_RUN),
            max_attempts: positive_or(int(cfg, "max_attempts"), ATTEMPTS_PER_UNIT),
            max_seconds_per_run: seconds.or(match &task {
                Task::LinkPreview => Some(LINK_SECONDS_PER_RUN),
                Task::Transcribe | Task::Host(_) => None,
            }),
            sample_rate,
            since,
            filter,
            order_by,
            model,
        })
    }
}

/// A host task's `endpoint` and `model`: both beside a task declaring `pure` false, else
/// neither (`run.bind.model-keys`).
fn model_endpoint(pipeline_id: &str, cfg: &Map<String, Value>, task: &Task, tasks: &Tasks) -> Result<Option<ModelEndpoint>, RunError> {
    let Some(key) = MODEL_KEYS.into_iter().find(|k| cfg.contains_key(*k)) else { return Ok(None) };
    let reaches = match task {
        Task::Host(name) => tasks.get(name).is_some_and(|t| !t.pure()),
        Task::Transcribe | Task::LinkPreview => false,
    };
    if !reaches {
        return Err(RunError::PipelineUnknownConfigKey(format!(
            "task `{}` reads no key `{key}`; `endpoint` and `model` name the model a host task declaring `pure` false reaches, and a pure or built-in task reaches none",
            task.name()
        )));
    }
    let text = |key: &str| match cfg.get(key).and_then(Value::as_str).map(str::trim) {
        Some(v) if !v.is_empty() => Ok(v.to_string()),
        _ => Err(RunError::PipelineSpecInvalid(format!(
            "pipeline `{pipeline_id}` declares no string `{key}`; a model endpoint names both `endpoint` and `model`"
        ))),
    };
    Ok(Some(ModelEndpoint { endpoint: text("endpoint")?, model: text("model")? }))
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
    pub endpoint_host: Option<String>,
    /// Header name to value template the exec proxy attaches to each request toward
    /// `endpoint_host` (`connector.attach.exec-proxy-attach`).
    #[serde(default)]
    pub attach: BTreeMap<String, String>,
    #[serde(default)]
    pub request_timeout_secs: Option<u64>,
    #[serde(default)]
    pub max_document_bytes: Option<u64>,
    #[serde(default)]
    pub max_probe_bytes: Option<u64>,
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
            "endpoint_host": self.endpoint_host,
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
    if b.driver == "fetch" {
        let forbidden = [
            ("env", !b.env.is_empty()),
            ("preprocess", !b.preprocess.is_empty()),
            ("engine", b.engine.is_some()),
            ("max_output_bytes", b.max_output_bytes.is_some()),
        ];
        if let Some((key, _)) = forbidden.into_iter().find(|(_, present)| *present) {
            return Err(RunError::DeriveFetchBindingKey(format!("fetch engine `{}` declares `{key}`", config.engine)));
        }
    }
    if let Some(host) = &b.endpoint_host {
        let bare = !host.is_empty()
            && !host.contains(['/', '?', '#', ':', '@'])
            && !host.chars().any(char::is_whitespace)
            && url::Host::parse(host).is_ok();
        if !bare {
            return Err(RunError::DeriveEndpointHostNotBare(format!("engine `{}` declares endpoint_host `{host}` with a path, query, port, scheme or invalid host", config.engine)));
        }
    }
    exec_attach(&config.engine, b)?;
    Ok(b)
}

/// Hold an exec binding's credentials to the loopback proxy: no `env` value embeds a
/// reference (`run.exec.env-credential`), and an `attach` table sits only beside an exec
/// binding's `endpoint_host` (`run.bind.attach-unbound`). Answers the parsed `attach`
/// templates, each embedding a reference (`connector.attach.literal-attach-value`).
pub fn exec_attach(engine: &str, b: &Binding) -> Result<Vec<(String, Template)>, RunError> {
    for (name, value) in &b.env {
        if Template::parse(value).is_ok_and(|t| t.has_reference()) {
            return Err(RunError::DeriveEnvCredential(format!(
                "engine `{engine}` puts a `secret://` reference in env `{name}`; a step reaches its vendor through the host's loopback proxy, which attaches `[derive.{engine}.attach]`"
            )));
        }
    }
    if b.attach.is_empty() {
        return Ok(Vec::new());
    }
    if b.driver == "fetch" || b.endpoint_host.is_none() {
        return Err(RunError::DeriveAttachUnbound(format!(
            "engine `{engine}` declares `attach` with {}; the proxy attaches credentials only toward an exec engine's `endpoint_host`",
            if b.driver == "fetch" { "driver `fetch`" } else { "no `endpoint_host`" }
        )));
    }
    let mut attach = Vec::new();
    for (header, value) in &b.attach {
        if header.is_empty() || !header.bytes().all(|c| c.is_ascii_alphanumeric() || b"!#$%&'*+-.^_`|~".contains(&c)) {
            return Err(RunError::Invalid(format!("engine `{engine}`: `attach` key `{header}` is not a header name")));
        }
        let t = check_material(&format!("derive.{engine}.attach.{header}"), value).map_err(|e| RunError::Invalid(e.to_string()))?;
        if !t.has_reference() {
            return Err(RunError::Invalid(
                ConnectorError::SecretLiteralAttachValue(format!("`derive.{engine}.attach.{header}` embeds no `${{secret://<name>}}` reference; a constant header belongs in the step's request"))
                    .to_string(),
            ));
        }
        attach.push((header.clone(), t));
    }
    Ok(attach)
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

/// Refuse a derive table naming the reserved `kind` column in its key, `order_by`,
/// `cluster_by` or `partition_by` (`run.emit.reserved-discriminator`).
pub fn check_reserved_discriminator(table: &crate::store::declare::TableDecl) -> Result<(), RunError> {
    let named = table.primary_key().iter().chain(table.order_by.iter()).chain(table.cluster_by()).chain(table.partition_by());
    if named.into_iter().any(|c| c == super::emit::KIND) {
        return Err(RunError::DeriveReservedDiscriminator(format!(
            "derive table `{}` declares column `kind`, which the tier writes to tell passages from markers",
            table.name
        )));
    }
    Ok(())
}

/// Hold a built-in derive pipeline to its one output table: a second table records per-unit
/// state outside the anti-join's view (`run.emit.failure-off-table`).
pub fn check_single_output(pipeline_id: &str, tables: &[&str]) -> Result<(), RunError> {
    match tables {
        [_] => Ok(()),
        [] => Err(RunError::Invalid(format!("derive pipeline `{pipeline_id}` declares no table; it writes one output table"))),
        [output, rest @ ..] => Err(RunError::DeriveFailureOffTable(format!(
            "derive pipeline `{pipeline_id}` declares {} beside its output table `{output}`; markers, failures and passages land in that one table",
            rest.iter().map(|t| format!("`{t}`")).collect::<Vec<_>>().join(", ")
        ))),
    }
}

/// The key every derive output table declares (`run.emit.primary-key`).
pub const DERIVE_PRIMARY_KEY: [&str; 3] = ["unit_ref", super::emit::DERIVATION_KEY, "cue_seq"];

/// Refuse a derive output table whose key is not [`DERIVE_PRIMARY_KEY`], or that names the
/// reserved `kind` column (`run.emit.primary-key`, `run.emit.reserved-discriminator`).
pub fn check_output_table(table: &crate::store::declare::TableDecl) -> Result<(), RunError> {
    check_reserved_discriminator(table)?;
    if table.primary_key() != DERIVE_PRIMARY_KEY {
        return Err(RunError::DerivePrimaryKeyMissing(format!(
            "derive table `{}` declares `primary_key = {:?}`; a derive table keys on [\"unit_ref\", \"derivation_key\", \"cue_seq\"]",
            table.name,
            table.primary_key()
        )));
    }
    Ok(())
}
