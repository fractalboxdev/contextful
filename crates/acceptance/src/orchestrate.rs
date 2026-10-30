//! The in-tree test filler of the orchestrator role (`run.orchestrate`): a provider that runs
//! one `contextful pipeline step` per table pull or chunk, or one `contextful job fire` per
//! store-driven job, records each step's commit pointer, retries no step, and replays a step
//! whose process died before it recorded a pointer. [`suite`] holds the scenarios every filler
//! of the role passes.

pub mod suite;

use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::time::{Duration, Instant};

/// The unit one provider step runs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Unit {
    /// One pull of the named table.
    Table(String),
    /// The chunk of the given ordinal in the pipeline's chunk plan.
    Chunk(u32),
    /// One fire of the named store-driven job; its delegate calls run inside the step.
    Job(String),
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
    env: Vec<(String, String)>,
    job_args: Vec<String>,
    records: Vec<(String, Record)>,
}

impl Filler {
    pub fn new(bin: &Path, root: &Path, pipeline: &str, project: &str, site: &str) -> Filler {
        Filler {
            bin: bin.to_path_buf(),
            root: root.to_path_buf(),
            pipeline: pipeline.into(),
            project: project.into(),
            site: site.into(),
            env: Vec::new(),
            job_args: Vec::new(),
            records: Vec::new(),
        }
    }

    /// The filler over `target`, its environment and job arguments included.
    pub fn over(target: &suite::Target) -> Filler {
        let mut filler = Filler::new(&target.bin, &target.root, &target.pipeline, &target.project, &target.site);
        filler.env = target.env.clone();
        filler.job_args = target.job_args.clone();
        filler
    }

    /// A pipeline unit replays under its step id and `pipeline step` names the attempt. `job fire`
    /// names no attempt, so invocation `k` past the first of a job unit fires as `<id>.replay-<k>`.
    fn command(&self, id: &str, unit: &Unit, now: &str) -> Command {
        let invocation = self.record(id).map_or(0, |r| r.invocations) + 1;
        let run = match unit {
            Unit::Job(_) if invocation > 1 => format!("{id}.replay-{invocation}"),
            _ => id.to_string(),
        };
        let mut command = Command::new(&self.bin);
        match unit {
            Unit::Table(table) => command.args(["pipeline", "step", &self.pipeline, "--table", table]),
            Unit::Chunk(n) => command.args(["pipeline", "step", &self.pipeline, "--chunk", &n.to_string()]),
            Unit::Job(name) => command.args(["job", "fire", name]).args(&self.job_args),
        };
        command
            .args(["--run-id", &run, "--project", &self.project, "--site-id", &self.site, "--now", now])
            .current_dir(&self.root)
            .env_remove("CARGO_TARGET_DIR")
            .env_remove("CONTEXTFUL_NODE_ID")
            .env_remove("CONTEXTFUL_SECRETS_BACKEND")
            .envs(self.env.iter().map(|(k, v)| (k.as_str(), v.as_str())));
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

    /// Record a finished invocation of step `id`: a completed one records the pointer its last
    /// stdout line prints, a failed one its standard error. Answers the pointer or the failure.
    fn finish(&mut self, id: &str, out: &Output) -> Result<String, String> {
        let record = self.record_mut(id);
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

    /// Start step `id` at `now` and wait, for at most 60 s, until `reached` holds.
    fn spawn_until(&mut self, id: &str, unit: &Unit, now: &str, piped: bool, reached: &dyn Fn() -> bool) -> Child {
        let mut command = self.command(id, unit, now);
        if piped {
            command.stdout(Stdio::piped()).stderr(Stdio::piped());
        } else {
            command.stdout(Stdio::null()).stderr(Stdio::null());
        }
        let child = command.spawn().expect("the step process starts");
        self.record_mut(id).invocations += 1;
        let deadline = Instant::now() + Duration::from_secs(60);
        while !reached() {
            assert!(Instant::now() < deadline, "step `{id}` never reached its point");
            std::thread::sleep(Duration::from_millis(20));
        }
        child
    }

    /// Invoke step `id` once at `now`, a first run or a replay. The filler never invokes a failed
    /// step again on its own.
    pub fn step(&mut self, id: &str, unit: &Unit, now: &str) -> Result<String, String> {
        let out: Output = self.command(id, unit, now).output().expect("the step process starts");
        self.record_mut(id).invocations += 1;
        self.finish(id, &out)
    }

    /// Invoke step `id` at `now` and kill its process once `reached` holds, within 60 s. The
    /// record counts the invocation and holds no pointer, so the provider invokes it again.
    pub fn step_killed(&mut self, id: &str, unit: &Unit, now: &str, reached: impl Fn() -> bool) {
        let mut child = self.spawn_until(id, unit, now, false, &reached);
        child.kill().expect("the step process dies");
        child.wait().expect("the step process is reaped");
    }

    /// Invoke step `id` at `now`, run `meanwhile` once `reached` holds, then wait for the step
    /// to return and record it as [`Filler::step`] does.
    pub fn step_during(&mut self, id: &str, unit: &Unit, now: &str, reached: impl Fn() -> bool, meanwhile: impl FnOnce()) -> Result<String, String> {
        let child = self.spawn_until(id, unit, now, true, &reached);
        meanwhile();
        let out = child.wait_with_output().expect("the step process is reaped");
        self.finish(id, &out)
    }
}

impl suite::Orchestrator for Filler {
    fn step(&mut self, id: &str, unit: &Unit, now: &str) -> Result<String, String> {
        Filler::step(self, id, unit, now)
    }

    fn step_killed(&mut self, id: &str, unit: &Unit, now: &str, reached: &dyn Fn() -> bool) {
        Filler::step_killed(self, id, unit, now, reached)
    }

    fn step_during(&mut self, id: &str, unit: &Unit, now: &str, reached: &dyn Fn() -> bool, meanwhile: &mut dyn FnMut()) -> Result<String, String> {
        Filler::step_during(self, id, unit, now, reached, meanwhile)
    }

    fn invocations(&self, id: &str) -> u32 {
        self.record(id).map_or(0, |r| r.invocations)
    }
}
