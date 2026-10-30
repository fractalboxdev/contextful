//! The in-tree test filler of the orchestrator role (`run.orchestrate`): a provider that runs
//! one `contextful pipeline step` per table pull or chunk, records each step's commit pointer,
//! retries no step, and replays a step whose process died before it recorded a pointer.

use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::time::{Duration, Instant};

/// The unit one provider step runs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Unit {
    /// One pull of the named table.
    Table(String),
    /// The chunk of the given ordinal in the pipeline's chunk plan.
    Chunk(u32),
}

/// What the filler holds for one step id.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Record {
    /// The commit pointer the step printed; `None` until an invocation completes.
    pub pointer: Option<String>,
    /// The step's failure, recorded once and never retried.
    pub failure: Option<String>,
    /// Invocations of the step: the first and every replay.
    pub invocations: u32,
}

/// A provider over one project's pipeline.
pub struct Filler {
    bin: PathBuf,
    root: PathBuf,
    pipeline: String,
    project: String,
    site: String,
    records: Vec<(String, Record)>,
}

impl Filler {
    pub fn new(bin: &Path, root: &Path, pipeline: &str, project: &str, site: &str) -> Filler {
        Filler { bin: bin.to_path_buf(), root: root.to_path_buf(), pipeline: pipeline.into(), project: project.into(), site: site.into(), records: Vec::new() }
    }

    fn command(&self, id: &str, unit: &Unit, now: &str) -> Command {
        let mut command = Command::new(&self.bin);
        command.args(["pipeline", "step", &self.pipeline]);
        match unit {
            Unit::Table(table) => command.args(["--table", table]),
            Unit::Chunk(n) => command.args(["--chunk", &n.to_string()]),
        };
        command
            .args(["--run-id", id, "--project", &self.project, "--site-id", &self.site, "--now", now])
            .current_dir(&self.root)
            .env_remove("CARGO_TARGET_DIR")
            .env_remove("CONTEXTFUL_NODE_ID")
            .env_remove("CONTEXTFUL_SECRETS_BACKEND");
        command
    }

    fn record_mut(&mut self, id: &str) -> &mut Record {
        if !self.records.iter().any(|(held, _)| held == id) {
            self.records.push((id.to_string(), Record::default()));
        }
        &mut self.records.iter_mut().find(|(held, _)| held == id).expect("the record was just inserted").1
    }

    /// The record of step `id`.
    pub fn record(&self, id: &str) -> Option<&Record> {
        self.records.iter().find(|(held, _)| held == id).map(|(_, r)| r)
    }

    /// Invoke step `id` once at `now`, a first run or a replay. A completed invocation records the pointer its last stdout
    /// line prints; a failed one records its standard error, and the filler never invokes it again
    /// on its own. Answers the pointer or the failure.
    pub fn step(&mut self, id: &str, unit: &Unit, now: &str) -> Result<String, String> {
        let out: Output = self.command(id, unit, now).output().expect("the step process starts");
        let record = self.record_mut(id);
        record.invocations += 1;
        if out.status.success() {
            let pointer = String::from_utf8_lossy(&out.stdout).lines().last().unwrap_or_default().to_string();
            record.pointer = Some(pointer.clone());
            Ok(pointer)
        } else {
            let failure = String::from_utf8_lossy(&out.stderr).to_string();
            record.failure = Some(failure.clone());
            Err(failure)
        }
    }

    /// Invoke step `id` at `now` and kill its process once `reached` holds, within 60 s. The
    /// record counts the invocation and holds no pointer, so the provider invokes it again.
    pub fn step_killed(&mut self, id: &str, unit: &Unit, now: &str, reached: impl Fn() -> bool) {
        let mut child = self.command(id, unit, now).stdout(Stdio::null()).stderr(Stdio::null()).spawn().expect("the step process starts");
        self.record_mut(id).invocations += 1;
        let deadline = Instant::now() + Duration::from_secs(60);
        while !reached() {
            assert!(Instant::now() < deadline, "step `{id}` never reached its kill point");
            std::thread::sleep(Duration::from_millis(20));
        }
        child.kill().expect("the step process dies");
        child.wait().expect("the step process is reaped");
    }
}
