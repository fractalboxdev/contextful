//! The derive source and its exec driver: each pull recomputes the outstanding set from
//! the parent table and the pipeline's own output, runs the operator's argv chain over
//! each unit's media with no shell and a cleared environment, and hands the passages and
//! markers to the land path.

use contextful_core::connector::reference::Template;
use contextful_core::run::derive::config::{Binding, DeriveConfig, OutputFormat, StepSpec};
use contextful_core::run::derive::cues::{parse, passages};
use contextful_core::run::derive::emit::{
    document_status, marker_row, passage_rows, revived, select, Derivation, Unit, UnitStatus, DERIVATION_KEY, OUTPUT_COLUMNS,
};
use contextful_core::run::derive::exec::{
    engine_id, excerpt, expand, is_url, Condition, StepFiles, CAPTURED_OUTPUT_BYTES, CHAIN_DEADLINE_SECS,
};
use contextful_core::run::derive::task::{host_revived, host_rows, landing_order, select_host, DeriveTask, Derived, HostUnit};
use contextful_core::run::journal::sha256_hex;
use contextful_core::run::ports::{Cancellation, PullRequest, Row, Source, TableReader};
use contextful_core::run::{Failure, FailureTag, RunError};
use contextful_outbound::Resolver;
use std::collections::BTreeMap;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

/// The source's registered name.
pub const NAME: &str = "derive";
const WAIT_TICK: Duration = Duration::from_millis(10);

fn refused(e: RunError) -> Failure {
    Failure::deterministic(FailureTag::Config, e.to_string())
}

/// One resolved chain step: the binary it runs, its digest, and its argument template.
#[derive(Debug, Clone)]
pub struct Step {
    pub binary: PathBuf,
    pub digest: String,
    pub args: Vec<String>,
    pub output_path: Option<String>,
    pub output_format: Option<OutputFormat>,
    pub when: Option<Condition>,
}

fn search_path(name: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path).map(|d| d.join(name)).find(|p| executable(p))
}

fn executable(p: &Path) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        p.is_file() && std::fs::metadata(p).is_ok_and(|m| m.permissions().mode() & 0o111 != 0)
    }
    #[cfg(not(unix))]
    {
        p.is_file()
    }
}

/// Resolve one step's binary: a path-form `command[0]` is pinned by its `sha256`, a bare
/// name is found on the search path (`run.exec.missing-binary`, `run.exec.unpinned-path`,
/// `run.exec.digest-mismatch`).
pub fn resolve_step(engine: &str, label: &str, spec: &StepSpec, cwd: &Path) -> Result<Step, RunError> {
    let argv = spec.argv(engine)?;
    let (program, args) = argv.split_first().ok_or_else(|| RunError::Invalid(format!("engine `{engine}` step `{label}` names no command")))?;
    let path_form = program.contains('/');
    let binary = if path_form { cwd.join(program) } else { search_path(program).unwrap_or_default() };
    if !executable(&binary) {
        return Err(RunError::DeriveBinaryMissing(format!("engine `{engine}` step `{label}`: `{program}` is not an executable file on this machine")));
    }
    let bytes = std::fs::read(&binary).map_err(|e| RunError::DeriveBinaryMissing(format!("engine `{engine}` step `{label}`: `{program}`: {e}")))?;
    let digest = sha256_hex(&bytes);
    if path_form {
        match &spec.sha256 {
            None => {
                return Err(RunError::DeriveUnpinnedPath(format!(
                    "engine `{engine}` step `{label}` runs `{program}` with no `sha256`; its bytes digest to `{digest}`"
                )))
            }
            Some(pin) if *pin != digest => {
                return Err(RunError::DeriveDigestMismatch(format!("engine `{engine}` step `{label}`: `{program}` digests to `{digest}`, pinned `{pin}`")))
            }
            Some(_) => {}
        }
    }
    let when = match spec.when.as_deref() {
        None => None,
        Some(name) => Some(Condition::parse(name).ok_or_else(|| {
            RunError::DeriveStepConditionUnknown(format!(
                "engine `{engine}` step `{label}` runs `when = \"{name}\"`; the conditions are `media_is_url` and `engine_requires_pcm16_wav`"
            ))
        })?),
    };
    Ok(Step { binary, digest, args: args.to_vec(), output_path: spec.output_path.clone(), output_format: spec.output_format, when })
}

/// A binding's chain, resolved once per run: its steps, its environment and its identity.
pub struct Chain {
    pub name: String,
    pub preprocess: Vec<Step>,
    pub engine: Step,
    pub env: Vec<(String, Template)>,
    pub deadline: Duration,
    pub max_output: u64,
    pub id: String,
}

impl Chain {
    pub fn resolve(name: &str, b: &Binding, cwd: &Path) -> Result<Chain, RunError> {
        let spec = b.engine.as_ref().ok_or_else(|| RunError::Invalid(format!("`[derive.{name}]` declares no `engine` step")))?;
        let engine = resolve_step(name, "engine", spec, cwd)?;
        let preprocess = b.preprocess.iter().enumerate().map(|(i, s)| resolve_step(name, &format!("preprocess-{i}"), s, cwd)).collect::<Result<Vec<_>, _>>()?;
        let mut env = Vec::new();
        for (k, v) in &b.env {
            contextful_core::run::derive::config::check_env_name(k)?;
            env.push((k.clone(), Template::parse(v).map_err(|e| RunError::Invalid(e.to_string()))?));
        }
        let steps: Vec<(String, Vec<String>)> = preprocess.iter().chain(std::iter::once(&engine)).map(|s| (s.digest.clone(), s.args.clone())).collect();
        Ok(Chain {
            name: name.to_string(),
            preprocess,
            engine,
            env,
            deadline: Duration::from_secs(b.timeout_secs.unwrap_or(CHAIN_DEADLINE_SECS)),
            max_output: b.max_output_bytes.unwrap_or(CAPTURED_OUTPUT_BYTES),
            id: engine_id(name, &steps),
        })
    }
}

/// Why a unit's chain produced no document: a typed error the unit lands as a marker, or
/// a run stop, which lands nothing and charges no attempt (`run.emit.canceled-unit`).
#[derive(Debug)]
pub enum ChainError {
    Unit(RunError),
    Canceled,
}

impl From<RunError> for ChainError {
    fn from(e: RunError) -> ChainError {
        ChainError::Unit(e)
    }
}

impl std::fmt::Display for ChainError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ChainError::Unit(e) => e.fmt(f),
            ChainError::Canceled => f.write_str("the run was stopped; the step's process group is reaped"),
        }
    }
}

impl std::error::Error for ChainError {}

/// Read a pipe to its end, keeping bytes while the streams' shared `total` is under `cap`
/// and counting the rest.
fn drain(mut pipe: impl Read + Send + 'static, cap: u64, total: Arc<AtomicU64>) -> std::thread::JoinHandle<Vec<u8>> {
    std::thread::spawn(move || {
        let mut kept = Vec::new();
        let mut buf = [0u8; 8192];
        while let Ok(n) = pipe.read(&mut buf) {
            if n == 0 {
                break;
            }
            let before = total.fetch_add(n as u64, Ordering::SeqCst);
            let room = cap.saturating_sub(before) as usize;
            kept.extend_from_slice(&buf[..n.min(room)]);
        }
        kept
    })
}

#[cfg(unix)]
fn kill_group(pgid: u32) {
    if let Ok(pgid) = libc::pid_t::try_from(pgid) {
        // SAFETY: `kill` takes plain integers; a negative pid addresses the process group.
        unsafe {
            libc::kill(-pgid, libc::SIGKILL);
        }
    }
}

#[cfg(not(unix))]
fn kill_group(_pgid: u32) {}

/// Refuse a binary whose bytes no longer match the digest resolved at run start.
fn verify(label: &str, step: &Step) -> Result<(), RunError> {
    let bytes = std::fs::read(&step.binary).map_err(|e| RunError::DeriveBinaryMissing(format!("step `{label}`: `{}`: {e}", step.binary.display())))?;
    let digest = sha256_hex(&bytes);
    if digest != step.digest {
        return Err(RunError::DeriveDigestMismatch(format!(
            "step `{label}`: `{}` digests to `{digest}`, resolved at run start as `{}`",
            step.binary.display(),
            step.digest
        )));
    }
    Ok(())
}

/// Run one step to completion within `deadline`, returning its standard output.
fn run_step(label: &str, step: &Step, files: &StepFiles, env: &[(String, String)], max_output: u64, deadline: Instant, cancel: &dyn Cancellation) -> Result<Vec<u8>, ChainError> {
    if cancel.requested() {
        return Err(ChainError::Canceled);
    }
    verify(label, step)?;
    let args: Vec<String> = step.args.iter().map(|a| expand(a, files)).collect();
    let mut cmd = Command::new(&step.binary);
    cmd.args(&args).env_clear().envs(env.iter().map(|(k, v)| (k, v))).stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped());
    #[cfg(unix)]
    std::os::unix::process::CommandExt::process_group(&mut cmd, 0);
    let mut child = cmd.spawn().map_err(|e| RunError::DeriveBinaryMissing(format!("step `{label}`: {e}")))?;
    let pgid = child.id();
    // One counter spans both streams: the bound is on standard output and error together.
    let produced = Arc::new(AtomicU64::new(0));
    let out = drain(child.stdout.take().ok_or_else(|| RunError::Invalid("no stdout".into()))?, max_output, produced.clone());
    let err = drain(child.stderr.take().ok_or_else(|| RunError::Invalid("no stderr".into()))?, max_output, produced.clone());
    let over = |n: u64| RunError::DeriveOutputCap(format!("step `{label}` wrote {n} bytes past its {max_output}-byte bound; it is stopped"));
    // Stopping reaps the group before the drains join, so they end once the pipes close.
    let stop = |child: &mut std::process::Child| {
        kill_group(pgid);
        let _ = child.wait();
    };
    let status = loop {
        if produced.load(Ordering::SeqCst) > max_output {
            stop(&mut child);
            let _ = (out.join(), err.join());
            return Err(over(produced.load(Ordering::SeqCst)).into());
        }
        if cancel.requested() {
            stop(&mut child);
            let _ = (out.join(), err.join());
            return Err(ChainError::Canceled);
        }
        if Instant::now() >= deadline {
            stop(&mut child);
            let _ = (out.join(), err.join());
            return Err(RunError::DeriveStepTimeout(format!("step `{label}` was running when the chain's deadline passed; its process group is reaped")).into());
        }
        match child.try_wait() {
            Ok(Some(s)) => break s,
            Ok(None) => std::thread::sleep(WAIT_TICK),
            Err(e) => return Err(RunError::Invalid(format!("waiting on step `{label}`: {e}")).into()),
        }
    };
    // Whatever the step left in its group ends with it.
    kill_group(pgid);
    let stdout = out.join().unwrap_or_default();
    let stderr = err.join().unwrap_or_default();
    let total = produced.load(Ordering::SeqCst);
    if total > max_output {
        return Err(over(total).into());
    }
    if !status.success() {
        return Err(RunError::DeriveStepExit(format!("step `{label}` exited {status}: {}", excerpt(&stderr))).into());
    }
    Ok(stdout)
}

/// The leading bytes of a local input, enough to read a WAV header's `fmt ` chunk.
fn head(input: &Path) -> Option<Vec<u8>> {
    let mut buf = Vec::with_capacity(4096);
    std::fs::File::open(input).ok()?.take(4096).read_to_end(&mut buf).ok()?;
    Some(buf)
}

/// Run a unit's chain over `media` and return the engine's cue document.
pub fn run_chain(chain: &Chain, media: &Path, env: &[(String, String)], scratch: &Path, cancel: &dyn Cancellation) -> Result<String, ChainError> {
    let deadline = Instant::now() + chain.deadline;
    let mut input = media.to_path_buf();
    for (i, step) in chain.preprocess.iter().enumerate() {
        let label = format!("preprocess-{i}");
        if let Some(condition) = step.when {
            let name = input.to_string_lossy();
            let bytes = if is_url(&name) { None } else { head(&input) };
            if !condition.holds(&name, bytes.as_deref()) {
                continue;
            }
        }
        let stem = scratch.join(format!("step-{i}"));
        let output = step.output_path.as_deref().map(|p| p.replace("{output_stem}", &stem.to_string_lossy())).unwrap_or_else(|| stem.to_string_lossy().into_owned());
        let files = StepFiles { input: input.to_string_lossy().into_owned(), output: output.clone(), output_stem: stem.to_string_lossy().into_owned() };
        run_step(&label, step, &files, env, chain.max_output, deadline, cancel)?;
        if !Path::new(&output).is_file() {
            return Err(RunError::DeriveStepProducedNothing(format!("step `{label}` exited zero and wrote no `{output}`")).into());
        }
        input = PathBuf::from(output);
    }
    let stem = scratch.join("engine");
    let output = chain.engine.output_path.as_deref().map(|p| p.replace("{output_stem}", &stem.to_string_lossy()));
    let files = StepFiles {
        input: input.to_string_lossy().into_owned(),
        output: output.clone().unwrap_or_default(),
        output_stem: stem.to_string_lossy().into_owned(),
    };
    let stdout = run_step("engine", &chain.engine, &files, env, chain.max_output, deadline, cancel)?;
    let bytes = match output {
        Some(path) => std::fs::read(&path).map_err(|_| RunError::DeriveStepProducedNothing(format!("the engine exited zero and wrote no `{path}`")))?,
        None => stdout,
    };
    Ok(String::from_utf8(bytes).map_err(|_| RunError::Invalid("the engine's cue document is not UTF-8".into()))?)
}

/// The derive source of one pipeline.
pub struct DeriveSource {
    pub pipeline_id: String,
    pub config: DeriveConfig,
    pub binding: Binding,
    /// The pipeline's own output table, the anti-join's other side.
    pub output_table: String,
    /// The output table's declared columns, which its derivation key reads.
    pub output_schema: serde_json::Value,
    pub reader: Box<dyn TableReader>,
    pub resolver: Arc<Resolver>,
    /// Where relative media paths and path-form binaries resolve.
    pub cwd: PathBuf,
}

impl DeriveSource {
    /// What this pipeline's rows derive under on this machine: the resolved chain's id, the
    /// binding's parameters and the output table's declared columns.
    pub fn derivation(&self) -> Result<Derivation, RunError> {
        let chain = Chain::resolve(&self.config.engine, &self.binding, &self.cwd)?;
        Ok(self.derivation_of(&chain))
    }

    fn derivation_of(&self, chain: &Chain) -> Derivation {
        Derivation { engine_id: chain.id.clone(), binding: self.binding.derivation_params(), output_schema: self.output_schema.clone() }
    }

    /// Resolve media to a local file under the media root: an address declines unless a step
    /// handles it, a path escaping the root refuses, and a value that is neither refuses, each
    /// for that unit alone.
    fn media_path(&self, media: &str) -> Result<PathBuf, RunError> {
        if is_url(media) {
            if !self.binding.preprocess.iter().any(|s| s.when.as_deref() == Some("media_is_url")) {
                return Err(RunError::DeriveRemoteUrlUnsupported(format!(
                    "engine `{}` reads local files; declare a preprocess step with `when = \"media_is_url\"` to fetch an address",
                    self.config.engine
                )));
            }
            return Ok(PathBuf::from(media));
        }
        let unreadable = || RunError::DeriveMediaUnreadable(format!("media `{media}` is neither an address nor a readable local file"));
        let declared = self.cwd.join(self.binding.media_root.as_deref().unwrap_or("."));
        let root = declared.canonicalize().map_err(|e| RunError::DeriveMediaUnreadable(format!("media root `{}`: {e}", declared.display())))?;
        // Canonicalizing resolves `..` and every symbolic link, so containment is checked on the file itself.
        let p = root.join(media).canonicalize().map_err(|_| unreadable())?;
        if !p.starts_with(&root) {
            return Err(RunError::DeriveMediaOutsideRoot(format!("media `{media}` resolves outside the media root `{}`", root.display())));
        }
        if !p.is_file() || std::fs::File::open(&p).is_err() {
            return Err(unreadable());
        }
        Ok(p)
    }

    /// Derive one unit into its rows; a run stop yields none.
    fn derive_unit(&self, chain: &Chain, env: &[(String, String)], unit: &Unit, cancel: &dyn Cancellation) -> Result<Vec<Row>, Failure> {
        let outcome = self.media_path(&unit.media).map_err(ChainError::Unit).and_then(|media| {
            let scratch = tempfile::Builder::new().prefix("contextful-derive-").tempdir().map_err(|e| RunError::Invalid(format!("scratch directory: {e}")))?;
            run_chain(chain, &media, env, scratch.path(), cancel)
        });
        Ok(match outcome {
            Ok(doc) => {
                let parsed = parse(&doc);
                for d in &parsed.defects {
                    eprintln!("{}: {d}", unit.key);
                }
                match document_status(&doc, &parsed) {
                    UnitStatus::Empty => vec![marker_row(unit, UnitStatus::Empty, None, false, &chain.id)],
                    UnitStatus::Ok => passage_rows(unit, &passages(&parsed.cues), &chain.id),
                    _ => vec![marker_row(unit, UnitStatus::Unavailable, None, true, &chain.id)],
                }
            }
            Err(ChainError::Canceled) => return Err(Failure::canceled(format!("stopped while deriving `{}`; its process group is reaped", unit.key))),
            Err(ChainError::Unit(e)) => vec![marker_row(unit, UnitStatus::Failed, Some(&e.to_string()), true, &chain.id)],
        })
    }
}

impl Source for DeriveSource {
    fn pull(&mut self, _request: &PullRequest, cancel: &dyn Cancellation) -> Result<Vec<u8>, Failure> {
        let chain = Chain::resolve(&self.config.engine, &self.binding, &self.cwd).map_err(refused)?;
        let derivation = self.derivation_of(&chain);
        let parents = self.reader.rows(&self.config.source_table, &[&self.config.parent_id_column, &self.config.media_column, DERIVATION_KEY])?;
        let derived = self.reader.rows(&self.output_table, &OUTPUT_COLUMNS)?;
        let sel = select(&parents, &derived, &self.config, &derivation);
        for e in &sel.incomplete {
            eprintln!("{}: {e}", self.pipeline_id);
        }
        let mut env = Vec::new();
        for (k, t) in &chain.env {
            env.push((k.clone(), self.resolver.render(t)?.reveal().to_string()));
        }
        let started = Instant::now();
        let budget = self.config.max_seconds_per_run.map(Duration::from_secs);
        let mut derived_units = Vec::new();
        for unit in &sel.outstanding {
            if cancel.requested() {
                return Err(Failure::canceled("stopped between units"));
            }
            if budget.is_some_and(|b| started.elapsed() >= b) {
                break;
            }
            derived_units.push((unit, self.derive_unit(&chain, &env, unit, cancel)?));
        }
        // A concurrent tick may have settled a unit under its key while this one derived it.
        let landed = if derived_units.is_empty() { Vec::new() } else { self.reader.rows(&self.output_table, &OUTPUT_COLUMNS)? };
        let mut rows = Vec::new();
        for (unit, unit_rows) in derived_units {
            match revived(unit, &landed, self.config.max_attempts) {
                Some(e) => eprintln!("{}: {e}", self.pipeline_id),
                None => rows.extend(unit_rows),
            }
        }
        serde_json::to_vec(&serde_json::json!({ "rows": rows, "more": false })).map_err(|e| Failure::new(FailureTag::Permanent, e.to_string()))
    }
}

/// A host task's derivation for one fire: every outstanding unit derived once, its rows
/// split per output table, the marker table landing last (`run.emit.marker-last`).
pub struct HostDerive {
    pub pipeline_id: String,
    pub config: DeriveConfig,
    pub task: Arc<dyn DeriveTask>,
    /// The store table each of the task's tables lands in, keyed by the task's name for it.
    pub tables: BTreeMap<String, String>,
    pub reader: Box<dyn TableReader>,
}

impl HostDerive {
    fn store_table(&self, table: &str) -> String {
        self.tables.get(table).cloned().unwrap_or_else(|| table.to_string())
    }

    /// Derive every outstanding unit and return each store table's rows in landing order:
    /// the content tables, then the marker table.
    pub fn stage(&self, cancel: &dyn Cancellation) -> Result<Vec<(String, Vec<Row>)>, Failure> {
        let task = self.task.as_ref();
        let name = self.config.task.name().to_string();
        let marker_table = self.store_table(&task.marker_table());
        let mut columns: Vec<String> = vec![self.config.parent_id_column.clone(), DERIVATION_KEY.to_string()];
        columns.extend(task.columns());
        let wanted: Vec<&str> = columns.iter().map(String::as_str).collect();
        let parents = self.reader.rows(&self.config.source_table, &wanted)?;
        let markers = self.reader.rows(&marker_table, &OUTPUT_COLUMNS)?;
        let sel = select_host(&parents, &markers, &self.config, task);
        for e in &sel.incomplete {
            eprintln!("{}: {e}", self.pipeline_id);
        }
        let started = Instant::now();
        let budget = self.config.max_seconds_per_run.map(Duration::from_secs);
        let mut derived: Vec<(&HostUnit, Derived)> = Vec::new();
        for unit in &sel.outstanding {
            if cancel.requested() {
                return Err(Failure::canceled("stopped between units"));
            }
            if budget.is_some_and(|b| started.elapsed() >= b) {
                break;
            }
            derived.push((unit, host_rows(unit, &name, task, task.derive(unit))));
        }
        // A concurrent tick may have settled a unit under its key while this one derived it.
        let landed = if derived.is_empty() { Vec::new() } else { self.reader.rows(&marker_table, &OUTPUT_COLUMNS)? };
        let mut per_table: BTreeMap<String, Vec<Row>> = BTreeMap::new();
        for (unit, rows) in derived {
            if let Some(e) = host_revived(unit, &landed, self.config.max_attempts) {
                eprintln!("{}: {e}", self.pipeline_id);
                continue;
            }
            for (table, rows) in rows {
                per_table.entry(table).or_default().extend(rows);
            }
        }
        Ok(landing_order(task).into_iter().map(|t| (self.store_table(&t), per_table.remove(&t).unwrap_or_default())).collect())
    }
}

/// A source handing over rows already derived: one table's share of a host task's fire.
/// Every pull hands over the same rows, so a retried step lands them again.
pub struct Staged(pub Vec<Row>);

impl Source for Staged {
    fn pull(&mut self, _request: &PullRequest, _cancel: &dyn Cancellation) -> Result<Vec<u8>, Failure> {
        serde_json::to_vec(&serde_json::json!({ "rows": self.0, "more": false })).map_err(|e| Failure::new(FailureTag::Permanent, e.to_string()))
    }
}
