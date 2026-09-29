//! `contextful pipeline plan`, `apply` and `serve`: the local control plane.
//!
//! `apply` validates the pipelines it converges and claims the next `manifest@v<N>.toml`
//! in the project's snapshot directory (`surface.apply.local-claim`); it dispatches nothing
//! (`run.declare.apply-fires-nothing`). `serve` arms the applied version's schedules and
//! dispatches each due pipeline as a `pipeline run --applied <N>` child process through the
//! engine's scheduler, which holds the deployment's cadence lease.

use crate::pipeline::{check, manifests};
use crate::project::Located;
use crate::run::{boot_id, wire_at, ProjectArgs};
use anyhow::{bail, Context, Result};
use contextful_context::project::Project;
use contextful_core::pipeline::declare::{collect, ManifestFile, PipelineSpec};
use contextful_core::run::derive::task::Tasks;
use contextful_core::surface::arm::{Schedule, TICK_INTERVAL_MS};
use contextful_core::surface::control::{snapshot_file, DEFAULT_POLL};
use contextful_core::surface::dispatch::DEFAULT_POOL;
use contextful_core::surface::SurfaceError;
use contextful_engine::control::{ControlError, SnapshotDir};
use contextful_engine::scheduler::{Dispatch, Entry, LeaseState, Scheduler};
use serde::Serialize;
use serde_json::json;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;

/// Claims an apply retries after losing the pointer's compare-and-swap before it gives up.
const CLAIM_ATTEMPTS: usize = 8;

/// The `[control]` block of the project manifest.
struct ControlConfig {
    snapshots: SnapshotDir,
    pool: usize,
    poll: Schedule,
}

fn control_config(text: &str, project: &Project) -> Result<ControlConfig> {
    let value: toml::Value = if text.is_empty() { toml::Value::Table(Default::default()) } else { toml::from_str(text)? };
    let block = value.get("control").cloned().unwrap_or(toml::Value::Table(Default::default()));
    let block = block.as_table().context("`[control]` is a table")?;
    if block.contains_key("url") {
        bail!("`[control] url` names a control URL; this build reconciles from a local `snapshot_dir` alone");
    }
    let dir = match block.get("snapshot_dir") {
        Some(v) => project.dir.join(v.as_str().context("`[control] snapshot_dir` is a path string")?),
        None => project.dir.join(".contextful/control").join(&project.name),
    };
    let pool = match block.get("pool") {
        Some(v) => usize::try_from(v.as_integer().context("`[control] pool` is an integer")?)
            .ok()
            .filter(|p| *p >= 1)
            .context("`[control] pool` is at least 1")?,
        None => DEFAULT_POOL,
    };
    let poll = block.get("poll").map(|v| v.as_str().context("`[control] poll` is a schedule string")).transpose()?.unwrap_or(DEFAULT_POLL);
    Ok(ControlConfig { snapshots: SnapshotDir::open(&dir), pool, poll: Schedule::parse(poll)? })
}

fn located(project: &ProjectArgs, declaration: Option<PathBuf>) -> Result<(Located, String, ControlConfig)> {
    let l = project.locate(declaration)?;
    let text = if l.declaration.exists() { std::fs::read_to_string(&l.declaration)? } else { String::new() };
    let control = control_config(&text, &l.project)?;
    Ok((l, text, control))
}

/// The manifest file of applied version `version`, read by `pipeline run --applied`.
pub(crate) fn snapshot_manifest(project: &Project, text: &str, version: u64) -> Result<ManifestFile> {
    let control = control_config(text, project)?;
    Ok(ManifestFile { path: snapshot_file(version), text: control.snapshots.read(version)? })
}

/// The applied version and the specifications it holds, by id; none before the first apply.
fn applied(snaps: &SnapshotDir) -> Result<(Option<u64>, BTreeMap<String, PipelineSpec>)> {
    let Some(version) = snaps.current()? else { return Ok((None, BTreeMap::new())) };
    let text = snaps.read(version)?;
    let specs = collect(&[ManifestFile { path: snapshot_file(version), text }])
        .map_err(|e| SurfaceError::ControlSnapshotUnreadable(format!("{}: {e}", snapshot_file(version))))?;
    Ok((Some(version), specs.into_iter().map(|d| (d.spec.id.clone(), d.spec)).collect()))
}

/// The snapshot document: every specification as a `[[pipeline]]` block, sorted by id.
fn render(specs: &BTreeMap<String, PipelineSpec>) -> Result<String> {
    #[derive(Serialize)]
    struct Doc<'a> {
        pipeline: Vec<&'a PipelineSpec>,
    }
    let body = toml::to_string(&Doc { pipeline: specs.values().collect() })
        .map_err(|e| SurfaceError::ApplyValidationRefused(format!("a specification holds a value a snapshot cannot carry: {e}")))?;
    Ok(format!("# An applied snapshot, claimed by `contextful pipeline apply`; immutable once claimed.\n\n{body}"))
}

#[derive(Serialize)]
struct Change {
    id: String,
    action: &'static str,
    content_hash: Option<String>,
    applied_hash: Option<String>,
    schedule: Option<String>,
}

fn diff(declared: &BTreeMap<String, PipelineSpec>, applied: &BTreeMap<String, PipelineSpec>) -> Vec<Change> {
    let ids: std::collections::BTreeSet<&String> = declared.keys().chain(applied.keys()).collect();
    ids.into_iter()
        .map(|id| {
            let (d, a) = (declared.get(id), applied.get(id));
            let (dh, ah) = (d.map(|s| s.content_hash()), a.map(|s| s.content_hash()));
            let action = match (&dh, &ah) {
                (Some(_), None) => "add",
                (None, _) => "remove",
                (Some(x), Some(y)) if x == y => "unchanged",
                _ => "change",
            };
            Change { id: id.clone(), action, content_hash: dh, applied_hash: ah, schedule: d.or(a).and_then(|s| s.schedule.clone()) }
        })
        .collect()
}

fn sigil(action: &str) -> char {
    match action {
        "add" => '+',
        "remove" => '-',
        "change" => '~',
        _ => '=',
    }
}

fn declared(declaration: &Path) -> Result<BTreeMap<String, PipelineSpec>> {
    Ok(collect(&manifests(declaration)?)?.into_iter().map(|d| (d.spec.id.clone(), d.spec)).collect())
}

/// `pipeline plan`: the declared set against the applied snapshot; reads only.
pub(crate) fn plan(project: &ProjectArgs, declaration: Option<PathBuf>, as_json: bool) -> Result<()> {
    let (l, _, control) = located(project, declaration)?;
    let (version, applied) = applied(&control.snapshots)?;
    let changes = diff(&declared(&l.declaration)?, &applied);
    if as_json {
        println!("{}", serde_json::to_string_pretty(&json!({ "applied": version, "pipelines": changes }))?);
        return Ok(());
    }
    println!("applied: {}", version.map(|v| format!("v{v}")).unwrap_or_else(|| "none".into()));
    for c in &changes {
        println!("{} {}{}", sigil(c.action), c.id, c.schedule.as_deref().map(|s| format!(" ({s})")).unwrap_or_default());
    }
    Ok(())
}

/// `pipeline apply [id]`: validate what converges, then claim the next version. Losing the
/// pointer's compare-and-swap reloads the winner and reapplies onto it
/// (`surface.apply.version-race`).
pub(crate) fn apply(project: &ProjectArgs, declaration: Option<PathBuf>, id: Option<&str>, tasks: &Tasks) -> Result<()> {
    let (l, _, control) = located(project, declaration)?;
    let declared = declared(&l.declaration)?;
    if let Some(id) = id {
        if !declared.contains_key(id) && !applied(&control.snapshots)?.1.contains_key(id) {
            bail!("no pipeline `{id}` is declared or applied");
        }
    }
    for _ in 0..CLAIM_ATTEMPTS {
        let (version, base) = applied(&control.snapshots)?;
        let mut target = base.clone();
        match id {
            Some(id) => match declared.get(id) {
                Some(spec) => {
                    target.insert(id.to_string(), spec.clone());
                }
                None => {
                    target.remove(id);
                }
            },
            None => target = declared.clone(),
        }
        let changes: Vec<Change> = diff(&target, &base).into_iter().filter(|c| c.action != "unchanged").collect();
        for c in changes.iter().filter(|c| c.action != "remove") {
            let spec = &target[&c.id];
            check(spec, &l.declaration, tasks)
                .map_err(|e| SurfaceError::ApplyValidationRefused(format!("pipeline `{}`: {e:#}", spec.id)))?;
        }
        if changes.is_empty() {
            match version {
                Some(v) => println!("unchanged at v{v}"),
                None => println!("nothing declared to apply"),
            }
            return Ok(());
        }
        match control.snapshots.claim(version, &render(&target)?) {
            Ok(v) => {
                for c in &changes {
                    println!("{} {}", sigil(c.action), c.id);
                }
                println!("applied v{v}");
                return Ok(());
            }
            Err(ControlError::Surface(e @ SurfaceError::ManifestVersionConflict(_))) => eprintln!("{e}"),
            Err(e) => return Err(e.into()),
        }
    }
    bail!("{} claims in a row lost to concurrent applies", CLAIM_ATTEMPTS)
}

/// A unit dispatched as a child `pipeline run --applied <N>` of this binary.
struct ChildDispatch {
    exe: PathBuf,
    project: String,
    declaration: Option<PathBuf>,
    now: Option<String>,
}

impl Dispatch for ChildDispatch {
    fn fire(&self, id: &str, version: u64) -> Result<String, String> {
        let mut cmd = Command::new(&self.exe);
        cmd.args(["pipeline", "run", id, "--applied", &version.to_string(), "--project", &self.project]);
        if let Some(d) = &self.declaration {
            cmd.arg("--declaration").arg(d);
        }
        if let Some(now) = &self.now {
            cmd.args(["--now", now]);
        }
        let out = cmd.output().map_err(|e| format!("{id}: starting `{}`: {e}", self.exe.display()))?;
        let (stdout, stderr) = (String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
        for line in stdout.lines().chain(stderr.lines()) {
            eprintln!("[{id}] {line}");
        }
        let last = |t: &str| t.lines().last().unwrap_or_default().to_string();
        if out.status.success() {
            Ok(last(&stdout))
        } else {
            Err(last(&stderr))
        }
    }
}

/// Arm `scheduler` from the applied snapshot: every entry declaring a schedule, an
/// unreadable one held back by name. `false` when no version is applied.
fn arm(scheduler: &mut Scheduler, snaps: &SnapshotDir) -> Result<bool> {
    let (version, specs) = applied(snaps)?;
    let Some(version) = version else { return Ok(false) };
    if version == scheduler.version() && !scheduler.armed().is_empty() {
        return Ok(true);
    }
    let mut entries = Vec::new();
    for spec in specs.values() {
        let Some(text) = &spec.schedule else { continue };
        match Schedule::parse(text) {
            Ok(schedule) => entries.push(Entry { id: spec.id.clone(), schedule }),
            Err(e) => eprintln!("pipeline `{}`: {e}; the entry stays unarmed", spec.id),
        }
    }
    scheduler.arm(version, entries)?;
    eprintln!("armed v{version}: {} scheduled pipeline(s)", scheduler.armed().len());
    Ok(true)
}

/// `pipeline serve [--cycle]`.
pub(crate) fn serve(project: &ProjectArgs, declaration: Option<PathBuf>, cycle: bool) -> Result<()> {
    let explicit = declaration.clone();
    let (l, _, control) = located(project, declaration)?;
    let w = wire_at(&l.project, &project.now)?;
    let dispatch = Arc::new(ChildDispatch {
        exe: std::env::current_exe()?,
        project: l.project.name.clone(),
        declaration: explicit,
        now: project.now.clone(),
    });
    let holder = format!("{}:{}", boot_id(), std::process::id());
    let mut scheduler = Scheduler::new(w.engine.catalog.clone(), dispatch, &l.project.name, &holder, control.pool);
    if cycle {
        return serve_cycle(&mut scheduler, &control);
    }
    let tick = std::time::Duration::from_millis(TICK_INTERVAL_MS);
    let mut next_poll = w.clock.now();
    loop {
        let now = w.clock.now();
        if now >= next_poll {
            // A failed poll leaves the armed set running (`surface.reconcile.fail-static`).
            if let Err(e) = arm(&mut scheduler, &control.snapshots) {
                eprintln!("{e:#}; the armed set stays in place");
            }
            next_poll = control.poll.next_after(now);
        }
        match scheduler.beat() {
            Ok(beat) => {
                if let LeaseState::HeldBy(h) = &beat.lease {
                    eprintln!("the cadence lease of `{}` is held by `{h}`; nothing dispatches", l.project.name);
                }
                for id in &beat.started {
                    eprintln!("fire {id}: started");
                }
            }
            Err(e) => eprintln!("beat: {e}"),
        }
        for f in scheduler.ended() {
            match f.result {
                Ok(line) => eprintln!("fire {}: done · {line}", f.id),
                Err(line) => eprintln!("fire {}: failed · {line}", f.id),
            }
        }
        std::thread::sleep(tick);
    }
}

/// One-shot evaluation (`surface.fire.cycle`).
fn serve_cycle(scheduler: &mut Scheduler, control: &ControlConfig) -> Result<()> {
    if !arm(scheduler, &control.snapshots)? {
        return Err(SurfaceError::CycleControlSourceUnresolved(format!(
            "`{}` holds no applied version; run `contextful pipeline apply` first",
            control.snapshots.root().display()
        ))
        .into());
    }
    let beat = scheduler.beat()?;
    if let LeaseState::HeldBy(holder) = &beat.lease {
        println!("{}", serde_json::to_string_pretty(&json!({ "fired": [], "failed": [], "pending": [], "armed": 0, "held_by": holder }))?);
        return Ok(());
    }
    let ended = scheduler.drain();
    let mut fired: Vec<&str> = ended.iter().filter(|f| f.result.is_ok()).map(|f| f.id.as_str()).collect();
    let mut failed: Vec<&str> = ended.iter().filter(|f| f.result.is_err()).map(|f| f.id.as_str()).collect();
    fired.sort();
    failed.sort();
    let mut pending: Vec<&str> = beat.pending.iter().chain(&beat.held).map(String::as_str).collect();
    pending.sort();
    let next_due = scheduler.next_due()?.map(|t| t.to_rfc3339());
    scheduler.release()?;
    let answer = json!({ "fired": fired, "failed": failed, "pending": pending, "armed": scheduler.armed().len(), "next_due": next_due });
    println!("{}", serde_json::to_string_pretty(&answer)?);
    Ok(())
}
