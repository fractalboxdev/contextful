//! The derive source and its exec driver: each pull recomputes the outstanding set from
//! the parent table and the pipeline's own output, runs the operator's argv chain over
//! each unit's media with no shell and a cleared environment, and hands the passages and
//! markers to the land path.

use contextful_core::connector::reference::Template;
use contextful_core::run::derive::config::{Binding, DeriveConfig, OutputFormat, StepSpec};
use contextful_core::run::derive::cues::{parse, passages};
use contextful_core::run::derive::emit::{marker_row, passage_rows, select, Unit, UnitStatus};
use contextful_core::run::derive::exec::{
    engine_id, excerpt, expand, StepFiles, CAPTURED_OUTPUT_BYTES, CHAIN_DEADLINE_SECS,
};
use contextful_core::run::journal::sha256_hex;
use contextful_core::run::ports::{Cancellation, PullRequest, Row, Source, TableReader};
use contextful_core::run::{Failure, FailureTag, RunError};
use contextful_runtime::Resolver;
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
    pub when: Option<String>,
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
    Ok(Step { binary, digest, args: args.to_vec(), output_path: spec.output_path.clone(), output_format: spec.output_format, when: spec.when.clone() })
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

/// Read a pipe to its end, keeping at most `cap` bytes and counting the rest.
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

/// Why one unit's chain failed.
pub type UnitError = RunError;

/// Run one step to completion within `deadline`, returning its standard output.
fn run_step(label: &str, step: &Step, files: &StepFiles, env: &[(String, String)], max_output: u64, deadline: Instant, cancel: &dyn Cancellation) -> Result<Vec<u8>, UnitError> {
    let args: Vec<String> = step.args.iter().map(|a| expand(a, files)).collect();
    let mut cmd = Command::new(&step.binary);
    cmd.args(&args).env_clear().envs(env.iter().map(|(k, v)| (k, v))).stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped());
    #[cfg(unix)]
    std::os::unix::process::CommandExt::process_group(&mut cmd, 0);
    let mut child = cmd.spawn().map_err(|e| RunError::DeriveBinaryMissing(format!("step `{label}`: {e}")))?;
    let pgid = child.id();
    let (out_n, err_n) = (Arc::new(AtomicU64::new(0)), Arc::new(AtomicU64::new(0)));
    let out = drain(child.stdout.take().ok_or_else(|| RunError::Invalid("no stdout".into()))?, max_output, out_n.clone());
    let err = drain(child.stderr.take().ok_or_else(|| RunError::Invalid("no stderr".into()))?, max_output, err_n.clone());
    let status = loop {
        let produced = out_n.load(Ordering::SeqCst).max(err_n.load(Ordering::SeqCst));
        if produced > max_output {
            kill_group(pgid);
            let _ = child.wait();
            // The drains keep reading until the pipes close, so the child never blocks on a full pipe.
            let _ = (out.join(), err.join());
            return Err(RunError::DeriveOutputCap(format!(
                "step `{label}` wrote {} bytes past its {max_output}-byte bound; it is stopped",
                out_n.load(Ordering::SeqCst).max(err_n.load(Ordering::SeqCst))
            )));
        }
        if Instant::now() >= deadline || cancel.requested() {
            kill_group(pgid);
            let _ = child.wait();
            return Err(RunError::DeriveStepTimeout(format!("step `{label}` was running when the chain's deadline passed; its process group is reaped")));
        }
        match child.try_wait() {
            Ok(Some(s)) => break s,
            Ok(None) => std::thread::sleep(WAIT_TICK),
            Err(e) => return Err(RunError::Invalid(format!("waiting on step `{label}`: {e}"))),
        }
    };
    // Whatever the step left in its group ends with it.
    kill_group(pgid);
    let stdout = out.join().unwrap_or_default();
    let stderr = err.join().unwrap_or_default();
    if !status.success() {
        return Err(RunError::DeriveStepExit(format!("step `{label}` exited {status}: {}", excerpt(&stderr))));
    }
    Ok(stdout)
}

/// Run a unit's chain over `media` and return the engine's cue document.
pub fn run_chain(chain: &Chain, media: &Path, env: &[(String, String)], scratch: &Path, cancel: &dyn Cancellation) -> Result<String, UnitError> {
    let deadline = Instant::now() + chain.deadline;
    let mut input = media.to_path_buf();
    for (i, step) in chain.preprocess.iter().enumerate() {
        let label = format!("preprocess-{i}");
        let stem = scratch.join(format!("step-{i}"));
        let output = step.output_path.as_deref().map(|p| p.replace("{output_stem}", &stem.to_string_lossy())).unwrap_or_else(|| stem.to_string_lossy().into_owned());
        let files = StepFiles { input: input.to_string_lossy().into_owned(), output: output.clone(), output_stem: stem.to_string_lossy().into_owned() };
        run_step(&label, step, &files, env, chain.max_output, deadline, cancel)?;
        if !Path::new(&output).is_file() {
            return Err(RunError::DeriveStepProducedNothing(format!("step `{label}` exited zero and wrote no `{output}`")));
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
    String::from_utf8(bytes).map_err(|_| RunError::Invalid("the engine's cue document is not UTF-8".into()))
}

/// The derive source of one pipeline.
pub struct DeriveSource {
    pub pipeline_id: String,
    pub config: DeriveConfig,
    pub binding: Binding,
    /// The pipeline's own output table, the anti-join's other side.
    pub output_table: String,
    pub reader: Box<dyn TableReader>,
    pub resolver: Arc<Resolver>,
    /// Where relative media paths and path-form binaries resolve.
    pub cwd: PathBuf,
}

impl DeriveSource {
    /// Resolve media to a local file: an address declines unless a step handles it, and a
    /// value that is neither refuses for that unit alone.
    fn media_path(&self, media: &str) -> Result<PathBuf, UnitError> {
        if media.starts_with("http://") || media.starts_with("https://") {
            if !self.binding.preprocess.iter().any(|s| s.when.as_deref() == Some("media_is_url")) {
                return Err(RunError::DeriveRemoteUrlUnsupported(format!(
                    "engine `{}` reads local files; declare a preprocess step with `when = \"media_is_url\"` to fetch an address",
                    self.config.engine
                )));
            }
            return Ok(PathBuf::from(media));
        }
        let p = self.cwd.join(media);
        if std::fs::File::open(&p).is_err() {
            return Err(RunError::DeriveMediaUnreadable(format!("media `{media}` is neither an address nor a readable local file")));
        }
        Ok(p)
    }

    /// Derive one unit into its rows.
    fn derive_unit(&self, chain: &Chain, env: &[(String, String)], unit: &Unit, cancel: &dyn Cancellation) -> Vec<Row> {
        let outcome = self.media_path(&unit.media).and_then(|media| {
            let mut nonce = [0u8; 8];
            let _ = getrandom::fill(&mut nonce);
            let scratch = std::env::temp_dir().join(format!("contextful-derive-{}", nonce.iter().map(|b| format!("{b:02x}")).collect::<String>()));
            std::fs::create_dir_all(&scratch).map_err(|e| RunError::Invalid(e.to_string()))?;
            let doc = run_chain(chain, &media, env, &scratch, cancel);
            let _ = std::fs::remove_dir_all(&scratch);
            doc
        });
        match outcome {
            Ok(doc) => {
                let parsed = parse(&doc);
                for d in &parsed.defects {
                    eprintln!("{}: {d}", unit.key);
                }
                let ps = passages(&parsed.cues);
                if ps.is_empty() {
                    vec![marker_row(unit, UnitStatus::Empty, None, false, &chain.id)]
                } else {
                    passage_rows(unit, &ps, &chain.id)
                }
            }
            Err(e) => vec![marker_row(unit, UnitStatus::Failed, Some(&e.to_string()), true, &chain.id)],
        }
    }
}

impl Source for DeriveSource {
    fn pull(&mut self, _request: &PullRequest, cancel: &dyn Cancellation) -> Result<Vec<u8>, Failure> {
        let parents = self.reader.rows(&self.config.source_table)?;
        let derived = self.reader.rows(&self.output_table)?;
        let sel = select(&parents, &derived, &self.config);
        for e in &sel.incomplete {
            eprintln!("{}: {e}", self.pipeline_id);
        }
        let chain = Chain::resolve(&self.config.engine, &self.binding, &self.cwd).map_err(refused)?;
        let mut env = Vec::new();
        for (k, t) in &chain.env {
            env.push((k.clone(), self.resolver.render(t)?.reveal().to_string()));
        }
        let started = Instant::now();
        let budget = self.config.max_seconds_per_run.map(Duration::from_secs);
        let mut rows = Vec::new();
        for unit in &sel.outstanding {
            if cancel.requested() || budget.is_some_and(|b| started.elapsed() >= b) {
                break;
            }
            rows.extend(self.derive_unit(&chain, &env, unit, cancel));
        }
        serde_json::to_vec(&serde_json::json!({ "rows": rows, "more": false })).map_err(|e| Failure::new(FailureTag::Permanent, e.to_string()))
    }
}
