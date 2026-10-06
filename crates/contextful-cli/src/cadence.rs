//! `contextful pipeline plan`, `apply` and `serve`: the local control plane.
//!
//! `apply` validates the pipelines it converges and claims the next `manifest@v<N>.toml`
//! in the project's snapshot directory (`surface.apply.local-claim`); it dispatches nothing
//! (`run.declare.apply-fires-nothing`). `serve` arms the applied version's schedules and
//! dispatches each due pipeline as a `pipeline run --applied <N>` child process through the
//! engine's scheduler, which holds the deployment's cadence lease.
//!
//! The control source is the snapshot directory, or a loopback `[control] url` serving
//! `manifest@current` and each `manifest@v<N>.toml` beneath it (`surface.reconcile.loopback-only`).

use crate::pipeline::{check, manifests};
use crate::project::Located;
use crate::run::{boot_id, wire_at, ProjectArgs};
use anyhow::{bail, Context, Result};
use contextful_context::project::Project;
use contextful_core::pipeline::declare::{collect, dependent_runs, ManifestFile, PipelineSpec};
use contextful_core::pipeline::transform::TransformOp;
use contextful_core::run::derive::task::Tasks;
use contextful_core::surface::arm::{Schedule, Trigger, TICK_INTERVAL_MS, WAKE_ANSWER_SECS};
use contextful_core::surface::control::{admit_loopback, control_url, parse_pointer, poll_schedule, snapshot_file, source_file, POINTER_FILE};
use contextful_core::surface::edit::check_document;
use contextful_core::surface::dispatch::{CHILD_GRACE_SECS, DEFAULT_POOL};
use contextful_core::surface::SurfaceError;
use contextful_engine::control::{ControlError, Draft, SnapshotDir};
use contextful_engine::scheduler::{Beat, Dispatch, Entry, Fired, LeaseState, Scheduler};
use contextful_engine::worker::{Relay, WorkerDispatch};
use contextful_outbound::egress::{system, Outbound, Transport};
use serde::Serialize;
use serde_json::json;
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::io::Read;
use std::process::{Child, ChildStderr, ChildStdout, Command, ExitStatus, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use url::Url;

/// Claims an apply retries after losing the pointer's compare-and-swap before it gives up.
const CLAIM_ATTEMPTS: usize = 8;

/// Wall clock one control-URL read may take.
const CONTROL_READ_TIMEOUT: Duration = Duration::from_secs(10);

/// Largest pointer or snapshot body a control-URL read takes.
const CONTROL_READ_MAX_BYTES: u64 = 16 * 1024 * 1024;

/// Interval at which a dispatch thread and a stopping serve poll a child's exit.
const CHILD_POLL: Duration = Duration::from_millis(20);

/// A loopback control plane serving the pointer and each version beneath one URL.
struct ControlUrl {
    base: Url,
    addrs: Vec<SocketAddr>,
    transport: Arc<dyn Transport>,
}

impl ControlUrl {
    /// Parse `text` and admit its host only when every address it resolves to is loopback.
    fn open(text: &str) -> Result<ControlUrl, SurfaceError> {
        let base = control_url(text)?;
        let host = base.host_str().unwrap_or_default().to_string();
        let port = base.port_or_known_default().unwrap_or(80);
        let transport = system();
        let addrs = transport.resolve(&host, port).unwrap_or_default();
        admit_loopback(&host, &addrs)?;
        Ok(ControlUrl { base, addrs, transport })
    }

    /// The body of `file`; `None` on `404`. Any other status, a redirect included, raises
    /// `ControlSnapshotUnreadable`: a poll follows no redirect and routes through no proxy.
    fn get(&self, file: &str) -> Result<Option<String>, SurfaceError> {
        let url = source_file(&self.base, file);
        let unreadable = |why: String| SurfaceError::ControlSnapshotUnreadable(format!("{url}: {why}"));
        let request = Outbound {
            method: "GET",
            url: &url,
            addrs: &self.addrs,
            headers: &[],
            body: &[],
            direct: true,
            timeout: CONTROL_READ_TIMEOUT,
            max_body: CONTROL_READ_MAX_BYTES,
            read_body: true,
        };
        let answer = self.transport.send(&request).map_err(|e| unreadable(format!("{e:?}")))?;
        match answer.status {
            200 => String::from_utf8(answer.body).map(Some).map_err(|_| unreadable("the body is not UTF-8".into())),
            404 => Ok(None),
            status => Err(unreadable(format!("answered `{status}`"))),
        }
    }
}

/// Where applied versions come from.
enum Source {
    Dir(SnapshotDir),
    Url(ControlUrl),
}

impl Source {
    /// The applied version, or `None` before the first apply.
    fn current(&self) -> Result<Option<u64>> {
        match self {
            Source::Dir(d) => Ok(d.current()?),
            Source::Url(u) => Ok(u.get(POINTER_FILE)?.map(|body| parse_pointer(&body)).transpose()?),
        }
    }

    /// The text of applied version `version`.
    fn read(&self, version: u64) -> Result<String> {
        match self {
            Source::Dir(d) => Ok(d.read(version)?),
            Source::Url(u) => Ok(u.get(&snapshot_file(version))?.ok_or_else(|| {
                SurfaceError::ControlSnapshotUnreadable(format!("{}: answered `404`", source_file(&u.base, &snapshot_file(version))))
            })?),
        }
    }

    fn describe(&self) -> String {
        match self {
            Source::Dir(d) => format!("`{}`", d.root().display()),
            Source::Url(u) => format!("`{}`", u.base),
        }
    }
}

/// The `[control]` block of the project manifest.
struct ControlConfig {
    source: Source,
    pool: usize,
    poll: Schedule,
    trigger: Trigger,
    /// Worker base URLs; empty, each unit runs as a child process.
    workers: Vec<String>,
    /// The URL whose `/awake/:token` route `serve` binds for its workers.
    relay: Option<Url>,
}

/// Read `[control] workers` and `[control] relay`: a relay is required once a worker is listed.
fn worker_config(block: &toml::value::Table) -> Result<(Vec<String>, Option<Url>)> {
    let workers = match block.get("workers") {
        Some(v) => v
            .as_array()
            .context("`[control] workers` is an array of worker URLs")?
            .iter()
            .map(|w| {
                let text = w.as_str().context("`[control] workers` is an array of worker URLs")?;
                let url = Url::parse(text).with_context(|| format!("`[control] workers` entry `{text}` is a URL"))?;
                if !matches!(url.scheme(), "http" | "https") || url.host_str().is_none() {
                    bail!("`[control] workers` entry `{text}` is an http or https URL with a host");
                }
                Ok(text.trim_end_matches('/').to_string())
            })
            .collect::<Result<Vec<_>>>()?,
        None => Vec::new(),
    };
    let relay = block
        .get("relay")
        .map(|v| {
            let text = v.as_str().context("`[control] relay` is a URL string")?;
            let url = Url::parse(text).with_context(|| format!("`[control] relay` `{text}` is a URL"))?;
            if url.scheme() != "http" || url.host_str().is_none() || url.port().is_none() {
                bail!("`[control] relay` `{text}` is an http URL naming a host and a port");
            }
            Ok(url)
        })
        .transpose()?;
    if !workers.is_empty() && relay.is_none() {
        bail!("`[control] workers` reach `serve` only through the awakeable route: set `[control] relay` to the URL it binds");
    }
    Ok((workers, relay))
}

/// The declaration's `[control] relay`: the one route a worker calls back to
/// (`surface.dispatch.submit-signed`).
pub(crate) fn declared_relay(text: &str) -> Result<Option<Url>> {
    let value: toml::Value = if text.is_empty() { toml::Value::Table(Default::default()) } else { toml::from_str(text)? };
    match value.get("control") {
        Some(block) => Ok(worker_config(block.as_table().context("`[control]` is a table")?)?.1),
        None => Ok(None),
    }
}

fn control_config(text: &str, project: &Project) -> Result<ControlConfig> {
    let value: toml::Value = if text.is_empty() { toml::Value::Table(Default::default()) } else { toml::from_str(text)? };
    let block = value.get("control").cloned().unwrap_or(toml::Value::Table(Default::default()));
    let block = block.as_table().context("`[control]` is a table")?;
    let source = match (block.get("url"), block.get("snapshot_dir")) {
        (Some(_), Some(_)) => bail!("`[control]` names one source: `url` or `snapshot_dir`"),
        (Some(url), None) => Source::Url(ControlUrl::open(url.as_str().context("`[control] url` is a URL string")?)?),
        (None, Some(v)) => Source::Dir(SnapshotDir::open(&project.dir.join(v.as_str().context("`[control] snapshot_dir` is a path string")?))),
        (None, None) => Source::Dir(SnapshotDir::open(&project.dir.join(".contextful/control").join(&project.name))),
    };
    let pool = match block.get("pool") {
        Some(v) => usize::try_from(v.as_integer().context("`[control] pool` is an integer")?)
            .ok()
            .filter(|p| *p >= 1)
            .context("`[control] pool` is at least 1")?,
        None => DEFAULT_POOL,
    };
    let poll = block.get("poll").map(|v| v.as_str().context("`[control] poll` is a schedule string")).transpose()?;
    let (workers, relay) = worker_config(block)?;
    let trigger = block.get("trigger").map(|v| v.as_str().context("`[control] trigger` is a string")).transpose()?;
    Ok(ControlConfig { source, pool, poll: poll_schedule(poll)?, trigger: Trigger::parse(trigger)?, workers, relay })
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
    Ok(ManifestFile { path: snapshot_file(version), text: control.source.read(version)? })
}

/// The applied version and the specifications it holds, by id; none before the first apply.
fn applied(snaps: &Source) -> Result<(Option<u64>, BTreeMap<String, PipelineSpec>)> {
    let Some(version) = snaps.current()? else { return Ok((None, BTreeMap::new())) };
    let text = snaps.read(version)?;
    let specs = collect(&[ManifestFile { path: snapshot_file(version), text }])
        .map_err(|e| SurfaceError::ControlSnapshotUnreadable(format!("{}: {e}", snapshot_file(version))))?;
    Ok((Some(version), specs.into_iter().map(|d| (d.spec.id.clone(), d.spec)).collect()))
}

/// The applied snapshot and recorded run outcomes the store publishes for Admin.
pub(crate) fn published(project: &Project, declaration: &Path) -> Result<Value> {
    let text = std::fs::read_to_string(declaration)?;
    let control = control_config(&text, project)?;
    let (version, pipelines) = applied(&control.source)?;
    let truncated = pipelines.len() > 1000;
    let pipelines: Vec<PipelineSpec> = pipelines.into_values().take(1000).collect();
    let selected: BTreeSet<&str> = pipelines.iter().map(|spec| spec.id.as_str()).collect();
    let store = contextful_context::Store::open(&project.dir, &project.name)?;
    let states = contextful_sync::run_state::run_states(&store)?;
    let mut runs: BTreeMap<String, contextful_sync::RunMark> = BTreeMap::new();
    for state in states.into_values() {
        for (id, mark) in state.runs {
            if selected.contains(id.as_str()) && runs.get(&id).is_none_or(|earlier| mark.started_at > earlier.started_at) {
                runs.insert(id, mark);
            }
        }
    }
    let wired = wire_at(project, &None)?;
    for row in wired.engine.catalog.runs(None)? {
        if selected.contains(row.pipeline_id.as_str()) && runs.get(&row.pipeline_id).is_none_or(|earlier| row.started_at > earlier.started_at) {
            runs.insert(row.pipeline_id.clone(), contextful_sync::RunMark {
                run_id: row.run_id,
                table: row.table,
                status: row.status,
                started_at: row.started_at,
                ended_at: row.ended_at,
            });
        }
    }
    let pipelines: Vec<Value> = pipelines.into_iter().map(|spec| json!({
        "id": spec.id,
        "schedule": spec.schedule,
        "tables": spec.tables.iter().map(|table| table.name()).collect::<Vec<_>>(),
        "source": spec.source.name,
        "after": spec.after,
        "steps": spec.transforms.iter().map(|step| match step {
            TransformOp::Select { .. } => "select",
            TransformOp::Rename { .. } => "rename",
            TransformOp::Cast { .. } => "cast",
            TransformOp::Filter { .. } => "filter",
            TransformOp::Extract { .. } => "extract",
        }).collect::<Vec<_>>(),
    })).collect();
    Ok(json!({ "applied": version, "pipelines": pipelines, "runs": runs, "truncated": truncated, "declined": 0 }))
}

/// The snapshot document: every specification as a `[[pipeline]]` block, sorted by id.
fn render(specs: &BTreeMap<String, PipelineSpec>) -> Result<String> {
    #[derive(Serialize)]
    struct Doc<'a> {
        pipeline: Vec<&'a PipelineSpec>,
    }
    let body = toml::to_string(&Doc { pipeline: specs.values().collect() })
        .map_err(|e| SurfaceError::ApplyValidationRefused(format!("a specification holds a value a snapshot cannot carry: {e}")))?;
    // The document carries references alone (`surface.edit.secret-in-document`,
    // `surface.edit.connector-upload`).
    let value: toml::Value = toml::from_str(&body).map_err(|e| SurfaceError::ApplyValidationRefused(format!("the rendered snapshot does not read back: {e}")))?;
    check_document(&value)?;
    Ok(format!("# An applied snapshot, claimed by `contextful pipeline apply`; immutable once claimed.\n\n{body}"))
}

fn validated_draft(document: &str, declaration: &Path, tasks: &Tasks) -> Result<String> {
    let specs = collect(&[ManifestFile { path: "manifest@draft.toml".into(), text: document.to_owned() }])
        .map_err(|e| SurfaceError::ApplyValidationRefused(e.to_string()))?;
    let specs: BTreeMap<String, PipelineSpec> = specs.into_iter().map(|entry| (entry.spec.id.clone(), entry.spec)).collect();
    let rendered = render(&specs)?;
    for spec in specs.values() {
        if let Some(schedule) = spec.schedule.as_deref() {
            Schedule::parse(schedule).map_err(|e| SurfaceError::ApplyValidationRefused(format!("pipeline `{}`: {e}", spec.id)))?;
        }
        check(spec, declaration, tasks).map_err(|e| SurfaceError::ApplyValidationRefused(format!("pipeline `{}`: {e:#}", spec.id)))?;
    }
    Ok(rendered)
}

/// Store one validated, version-bound control draft without changing the applied pointer.
pub(crate) fn edit(project: &ProjectArgs, declaration: Option<PathBuf>, expected: u64, document: &str, operator: &str, tasks: &Tasks) -> Result<Value> {
    let (located, _, control) = located(project, declaration)?;
    let snapshots = owner(&control)?;
    snapshots.initialized()?;
    let text = validated_draft(document, &located.declaration, tasks)?;
    let draft = Draft::new(expected, text, operator.to_owned())?;
    snapshots.save_draft(&draft)?;
    Ok(json!({ "expected": expected, "nonce": draft.nonce }))
}

/// Claim a verified control request's nonce in the selected store owner's snapshot state.
pub(crate) fn claim_operator_nonce(project: &ProjectArgs, declaration: Option<PathBuf>, nonce: &str, signed_at: i64, now: i64) -> Result<bool> {
    let (_, _, control) = located(project, declaration)?;
    owner(&control)?.claim_attestation_nonce(nonce, signed_at, now).map_err(Into::into)
}

/// Revalidate the saved draft and claim it only at the version the editor read.
pub(crate) fn apply_draft(project: &ProjectArgs, declaration: Option<PathBuf>, expected: u64, nonce: &str, operator: &str, tasks: &Tasks) -> Result<()> {
    let (initial, _, control) = located(project, declaration.clone())?;
    let snapshots = owner(&control)?;
    snapshots.initialized()?;
    let draft = snapshots.read_draft()?;
    if draft.expected != expected || draft.nonce != nonce || draft.operator != operator {
        return Err(SurfaceError::ManifestVersionConflict(format!("the draft read v{} and apply named v{expected}", draft.expected)).into());
    }
    if validated_draft(&draft.document, &initial.declaration, tasks)? != draft.document {
        return Err(SurfaceError::ApplyValidationRefused("the saved draft is not the validated snapshot".into()).into());
    }
    let (fresh, _, control) = located(project, declaration)?;
    let owner = owner(&control)?;
    if owner.root() != snapshots.root() || fresh.declaration != initial.declaration {
        return Err(SurfaceError::ConfigOwnerUnconfigured("the store's control owner changed during validation".into()).into());
    }
    owner.claim_draft(&draft)?;
    Ok(())
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
    let (version, applied) = applied(&control.source)?;
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

/// The snapshot directory an edit or apply claims in. A `[control] url` names a remote owner
/// this process holds no storage or credential for, so it raises `ConfigOwnerUnconfigured`
/// (`surface.apply.owner-unconfigured`); a directory on a filesystem without linearizable
/// conditional writes raises `ConditionalWriteUnsupported` (`surface.apply.weak-conditional-backend`).
fn owner(control: &ControlConfig) -> Result<&SnapshotDir> {
    match &control.source {
        Source::Dir(d) => {
            d.admit()?;
            Ok(d)
        }
        Source::Url(u) => Err(SurfaceError::ConfigOwnerUnconfigured(format!(
            "`[control] url` names `{}` as the configuration owner, and this process holds no storage or credential for it; no local writer substitutes for it",
            u.base
        ))
        .into()),
    }
}

/// `pipeline import`: the guarded import, claiming v1 from every declared pipeline while the
/// directory holds no version (`surface.apply.uninitialized-store`).
pub(crate) fn import(project: &ProjectArgs, declaration: Option<PathBuf>, tasks: &Tasks) -> Result<()> {
    let (l, _, control) = located(project, declaration)?;
    let snapshots = owner(&control)?;
    let declared = declared(&l.declaration)?;
    let text = render(&declared)?;
    for spec in declared.values() {
        check(spec, &l.declaration, tasks).map_err(|e| SurfaceError::ApplyValidationRefused(format!("pipeline `{}`: {e:#}", spec.id)))?;
    }
    let v = snapshots.import(&text)?;
    for id in declared.keys() {
        println!("+ {id}");
    }
    println!("imported v{v}");
    Ok(())
}

/// `pipeline apply [id]`: validate what converges, then claim the next version. Losing the
/// pointer's compare-and-swap reloads the winner and reapplies onto it
/// (`surface.apply.version-race`).
pub(crate) fn apply(project: &ProjectArgs, declaration: Option<PathBuf>, id: Option<&str>, tasks: &Tasks) -> Result<()> {
    let (l, _, control) = located(project, declaration)?;
    let snapshots = owner(&control)?;
    snapshots.initialized()?;
    let source = &control.source;
    let declared = declared(&l.declaration)?;
    if let Some(id) = id {
        if !declared.contains_key(id) && !applied(source)?.1.contains_key(id) {
            bail!("no pipeline `{id}` is declared or applied");
        }
    }
    for _ in 0..CLAIM_ATTEMPTS {
        let (version, base) = applied(source)?;
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
        let text = render(&target)?;
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
        match snapshots.claim(version, &text) {
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

/// Every live child a serve process dispatched, each the leader of its own process group
/// (`surface.dispatch.children-reaped`). A child stays here, unreaped, until its exit status
/// is read under the lock, so a group signal never reaches a recycled pid.
#[derive(Default)]
struct Children {
    live: Mutex<BTreeMap<u32, Child>>,
    closed: AtomicBool,
}

impl Children {
    /// Start `cmd` as a tracked child in its own process group; refused once [`Children::end_all`] has run.
    fn spawn(&self, cmd: &mut Command) -> std::io::Result<(u32, Option<ChildStdout>, Option<ChildStderr>)> {
        #[cfg(unix)]
        std::os::unix::process::CommandExt::process_group(cmd, 0);
        let mut live = self.live.lock().unwrap_or_else(|e| e.into_inner());
        if self.closed.load(Ordering::SeqCst) {
            return Err(std::io::Error::other("serve is stopping and starts no child"));
        }
        let mut child = cmd.spawn()?;
        let (pid, out, err) = (child.id(), child.stdout.take(), child.stderr.take());
        live.insert(pid, child);
        Ok((pid, out, err))
    }

    /// Block until child `pid` exits, polling under the lock, then forget it.
    fn wait(&self, pid: u32) -> std::io::Result<ExitStatus> {
        loop {
            {
                let mut live = self.live.lock().unwrap_or_else(|e| e.into_inner());
                let Some(child) = live.get_mut(&pid) else { return Err(std::io::Error::other("the child is untracked")) };
                match child.try_wait() {
                    Ok(Some(status)) => {
                        live.remove(&pid);
                        return Ok(status);
                    }
                    Ok(None) => {}
                    Err(e) => {
                        live.remove(&pid);
                        return Err(e);
                    }
                }
            }
            std::thread::sleep(CHILD_POLL);
        }
    }

    /// Signal each child still running; answers how many were.
    fn signal(&self, kill: bool) -> usize {
        let mut live = self.live.lock().unwrap_or_else(|e| e.into_inner());
        let mut running = 0;
        for (pid, child) in live.iter_mut() {
            if !matches!(child.try_wait(), Ok(None)) {
                continue;
            }
            running += 1;
            #[cfg(unix)]
            let sent = contextful_engine::stop::signal_group(*pid, kill);
            #[cfg(not(unix))]
            let sent = if kill { child.kill() } else { Ok(()) };
            if let Err(e) = sent {
                eprintln!("signalling child {pid}: {e}");
            }
            if kill {
                let _ = child.wait();
            }
        }
        running
    }

    /// Refuse further children, send `SIGTERM` to every live child's group, wait up to
    /// `grace`, then `SIGKILL` the remainder and wait each out.
    fn end_all(&self, grace: Duration) {
        self.closed.store(true, Ordering::SeqCst);
        if self.signal(false) == 0 {
            return;
        }
        let deadline = std::time::Instant::now() + grace;
        while std::time::Instant::now() < deadline {
            std::thread::sleep(CHILD_POLL);
            if self.all_ended() {
                return;
            }
        }
        let killed = self.signal(true);
        if killed > 0 {
            eprintln!("killed {killed} child run(s) still alive {}s after SIGTERM", grace.as_secs());
        }
    }

    /// Whether no tracked child is still running.
    fn all_ended(&self) -> bool {
        let mut live = self.live.lock().unwrap_or_else(|e| e.into_inner());
        live.values_mut().all(|c| !matches!(c.try_wait(), Ok(None)))
    }
}

/// Ends every live child when serve returns or unwinds (`surface.dispatch.children-reaped`).
struct Reaper(Arc<Children>);

impl Drop for Reaper {
    fn drop(&mut self) {
        self.0.end_all(Duration::from_secs(CHILD_GRACE_SECS));
    }
}

/// A unit dispatched as a child `pipeline run --applied <N>` of this binary.
struct ChildDispatch {
    exe: PathBuf,
    project: String,
    declaration: Option<PathBuf>,
    now: Option<String>,
    children: Arc<Children>,
}

/// Read `pipe` to its end on a thread of its own.
fn drain_pipe(pipe: Option<impl Read + Send + 'static>) -> std::thread::JoinHandle<String> {
    std::thread::spawn(move || {
        let mut text = Vec::new();
        if let Some(mut p) = pipe {
            let _ = p.read_to_end(&mut text);
        }
        String::from_utf8_lossy(&text).into_owned()
    })
}

impl Dispatch for ChildDispatch {
    fn fire(&self, id: &str, steps: &[String], version: u64) -> Result<String, String> {
        let mut lines = Vec::new();
        for step in std::iter::once(id).chain(steps.iter().map(String::as_str)) {
            lines.push(self.step(step, version)?);
        }
        Ok(lines.join("; "))
    }
}

impl ChildDispatch {
    /// Run one landing step as a child `pipeline run --applied <N>`.
    fn step(&self, id: &str, version: u64) -> Result<String, String> {
        let mut cmd = Command::new(&self.exe);
        cmd.args(["pipeline", "run", id, "--applied", &version.to_string(), "--project", &self.project]);
        if let Some(d) = &self.declaration {
            cmd.arg("--declaration").arg(d);
        }
        if let Some(now) = &self.now {
            cmd.args(["--now", now]);
        }
        cmd.stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped());
        let (pid, out, err) = self.children.spawn(&mut cmd).map_err(|e| format!("{id}: starting `{}`: {e}", self.exe.display()))?;
        let (out, err) = (drain_pipe(out), drain_pipe(err));
        let status = self.children.wait(pid).map_err(|e| format!("{id}: waiting on child {pid}: {e}"))?;
        let (stdout, stderr) = (out.join().unwrap_or_default(), err.join().unwrap_or_default());
        for line in stdout.lines().chain(stderr.lines()) {
            eprintln!("[{id}] {line}");
        }
        let last = |t: &str| t.lines().last().unwrap_or_default().to_string();
        if status.success() {
            Ok(last(&stdout))
        } else if stderr.trim().is_empty() {
            Err(format!("child {pid} ended: {status}"))
        } else {
            Err(last(&stderr))
        }
    }
}

/// Bind the relay's awakeable route and dispatch onto `workers` through it
/// (`surface.dispatch.heartbeat-beat`). Heartbeats and callbacks are timed on the system
/// clock whatever `--now` reads.
fn worker_dispatch(relay: &Url, workers: &[String]) -> Result<Arc<dyn Dispatch>> {
    let key = crate::worker::worker_key()?;
    let addr = format!("{}:{}", relay.host_str().unwrap_or_default(), relay.port().unwrap_or_default());
    let listener = std::net::TcpListener::bind(&addr).with_context(|| format!("binding the relay at `{addr}`"))?;
    eprintln!("relay on http://{}/awake/:token for {} worker(s)", listener.local_addr()?, workers.len());
    let r = Arc::new(Relay::new(key.clone(), workers.to_vec(), Arc::new(crate::clock::SystemClock)));
    crate::worker::serve_relay(r.clone(), listener);
    Ok(Arc::new(WorkerDispatch {
        relay: r,
        client: Arc::new(crate::worker::HttpWorkers { transport: system(), key: key.clone() }),
        callback_base: relay.as_str().trim_end_matches('/').to_string(),
        check: Duration::from_secs(1),
    }))
}

/// Report each due step a beat refused (`surface.dispatch.not-a-head`).
fn report_refused(beat: &Beat) {
    for (id, refusal) in &beat.refused {
        eprintln!("fire {id}: refused · {refusal}");
    }
}

/// An applied pipeline the scheduler holds no entry for, and why (`surface.arm.unarmed-named`).
#[derive(Serialize)]
struct Unarmed {
    id: String,
    reason: String,
}

/// Arm `scheduler` from the applied snapshot: every entry declaring a readable schedule,
/// after taking the run starts the store's pulled run states record
/// (`surface.arm.pulled-history`). Each pipeline left out is named on stderr with its reason
/// and returned, once per applied version; a version already armed returns none. `None`
/// when no version is applied.
fn arm(scheduler: &mut Scheduler, snaps: &Source, project: &Project) -> Result<Option<Vec<Unarmed>>> {
    let store = contextful_context::Store::open(&project.dir, &project.name)?;
    scheduler.observe_runs(contextful_sync::run_state::newest_starts(&store)?);
    let (version, specs) = applied(snaps)?;
    let Some(version) = version else { return Ok(None) };
    if version == scheduler.version() {
        return Ok(Some(Vec::new()));
    }
    let runs = dependent_runs(specs.values()).map_err(|e| SurfaceError::ControlSnapshotUnreadable(format!("{}: {e}", snapshot_file(version))))?;
    let mut entries = Vec::new();
    let mut unarmed = Vec::new();
    for spec in specs.values() {
        let reason = match spec.schedule.as_deref().map(Schedule::parse) {
            Some(Ok(schedule)) => {
                entries.push(Entry { id: spec.id.clone(), schedule });
                continue;
            }
            Some(Err(e)) => e.to_string(),
            None => match runs.head_of.get(&spec.id) {
                Some(head) => format!("it lands as a step of the dependent run headed by `{head}`"),
                None => format!("it declares no `schedule`; `contextful pipeline run {}` fires it", spec.id),
            },
        };
        eprintln!("pipeline `{}` stays unarmed: {reason}", spec.id);
        unarmed.push(Unarmed { id: spec.id.clone(), reason });
    }
    scheduler.arm_runs(version, entries, runs.steps)?;
    eprintln!("armed v{version}: {} scheduled pipeline(s), {} unarmed", scheduler.armed().len(), unarmed.len());
    Ok(Some(unarmed))
}

/// `pipeline serve [--cycle]`.
pub(crate) fn serve(project: &ProjectArgs, declaration: Option<PathBuf>, cycle: bool, http: Option<&str>) -> Result<()> {
    let explicit = declaration.clone();
    let (l, text, control) = located(project, declaration)?;
    if !cycle {
        control.trigger.require_face(http.is_some())?;
        if http.is_some() && control.trigger == Trigger::InProcess {
            bail!("`--http` serves the external trigger's `POST /wake`; set `[control] trigger = \"external\"`");
        }
    }
    // Every configured resource resolves inside the residency allow-set before anything
    // serves (`surface.reside.region-mismatch`).
    crate::reside::enforce(&l, &text)?;
    let w = wire_at(&l.project, &project.now)?;
    let children = Arc::new(Children::default());
    let reaper = Reaper(children.clone());
    let dispatch: Arc<dyn Dispatch> = match &control.relay {
        Some(relay) if !control.workers.is_empty() => worker_dispatch(relay, &control.workers)?,
        _ => Arc::new(ChildDispatch {
            exe: std::env::current_exe()?,
            project: l.project.name.clone(),
            declaration: explicit,
            now: project.now.clone(),
            children: children.clone(),
        }),
    };
    let holder = format!("{}:{}", boot_id(), std::process::id());
    let mut scheduler = Scheduler::new(w.engine.catalog.clone(), dispatch, &l.project.name, &holder, control.pool);
    contextful_engine::stop::install();
    if cycle {
        // A stop signal ends the cycle's children, so its drain returns.
        let watched = children.clone();
        std::thread::spawn(move || {
            while !contextful_engine::stop::requested() {
                std::thread::sleep(CHILD_POLL);
            }
            watched.end_all(Duration::from_secs(CHILD_GRACE_SECS));
        });
        let answered = serve_cycle(&mut scheduler, &control, &l.project);
        drop(reaper);
        answered?;
        if contextful_engine::stop::requested() {
            bail!("a stop signal ended the cycle's dispatched units");
        }
        return Ok(());
    }
    if let Some(addr) = http {
        return serve_wakes(&mut scheduler, &control, addr, &l.project, reaper);
    }
    eprintln!("serving `{}` from {} as `{holder}`", l.project.name, control.source.describe());
    let tick = Duration::from_millis(TICK_INTERVAL_MS);
    let mut next_poll = w.clock.now();
    let mut held_by: Option<String> = None;
    while !contextful_engine::stop::requested() {
        // The lease comes before arming: a process finding it held arms nothing
        // (`surface.dispatch.lease-gated`).
        match scheduler.hold() {
            Ok(LeaseState::HeldBy(h)) => {
                if held_by.as_ref() != Some(&h) {
                    eprintln!("the cadence lease of `{}` is held by `{h}`; nothing arms or dispatches", l.project.name);
                    held_by = Some(h);
                }
            }
            Ok(LeaseState::Held) => {
                if held_by.take().is_some() {
                    eprintln!("took the cadence lease of `{}`", l.project.name);
                }
                let now = w.clock.now();
                if now >= next_poll {
                    // A failed poll leaves the armed set running (`surface.reconcile.fail-static`).
                    if let Err(e) = arm(&mut scheduler, &control.source, &l.project) {
                        eprintln!("{e:#}; the armed set stays in place");
                    }
                    next_poll = control.poll.next_after(now);
                }
                match scheduler.beat() {
                    Ok(beat) => {
                        report_refused(&beat);
                        for id in &beat.started {
                            eprintln!("fire {id}: started");
                        }
                    }
                    Err(e) => eprintln!("beat: {e}"),
                }
            }
            Err(e) => eprintln!("cadence lease: {e}"),
        }
        report(scheduler.ended());
        std::thread::sleep(tick);
    }
    eprintln!("stopping: ending dispatched children, then releasing the cadence lease");
    drop(reaper);
    report(scheduler.drain());
    scheduler.release()?;
    eprintln!("stopped");
    Ok(())
}

fn report(ended: Vec<Fired>) {
    for f in ended {
        match f.result {
            Ok(line) => eprintln!("fire {}: done · {line}", f.id),
            Err(line) => eprintln!("fire {}: failed · {line}", f.id),
        }
    }
}

/// The answer of a cycle finding the cadence lease held: nothing armed, the holder named.
fn held_answer(holder: &str) -> Result<()> {
    println!("{}", serde_json::to_string_pretty(&json!({ "fired": [], "failed": [], "pending": [], "held_by": holder }))?);
    Ok(())
}

/// One-shot evaluation (`surface.fire.cycle`). The lease comes first: finding it held,
/// the cycle arms nothing and answers the holder (`surface.dispatch.lease-gated`).
fn serve_cycle(scheduler: &mut Scheduler, control: &ControlConfig, project: &Project) -> Result<()> {
    if let LeaseState::HeldBy(holder) = scheduler.hold()? {
        return held_answer(&holder);
    }
    let armed = match arm(scheduler, &control.source, project) {
        Ok(Some(unarmed)) => Ok(unarmed),
        Ok(None) => Err(SurfaceError::CycleControlSourceUnresolved(format!(
            "{} holds no applied version; run `contextful pipeline apply` first",
            control.source.describe()
        ))
        .into()),
        Err(e) => Err(e),
    };
    let unarmed = match armed {
        Ok(unarmed) => unarmed,
        Err(e) => {
            scheduler.release()?;
            return Err(e);
        }
    };
    let beat = scheduler.beat()?;
    if let LeaseState::HeldBy(holder) = &beat.lease {
        return held_answer(holder);
    }
    report_refused(&beat);
    let ended = scheduler.drain();
    report(ended.clone());
    let mut fired: Vec<&str> = ended.iter().filter(|f| f.result.is_ok()).map(|f| f.id.as_str()).collect();
    let mut failed: Vec<&str> = ended.iter().filter(|f| f.result.is_err()).map(|f| f.id.as_str()).chain(beat.refused.iter().map(|(id, _)| id.as_str())).collect();
    fired.sort();
    failed.sort();
    let mut pending: Vec<&str> = beat.pending.iter().chain(&beat.held).map(String::as_str).collect();
    pending.sort();
    let next_due = scheduler.next_due()?.map(|t| t.to_rfc3339());
    scheduler.release()?;
    let answer = json!({
        "fired": fired,
        "failed": failed,
        "pending": pending,
        "armed": scheduler.armed().len(),
        "unarmed": unarmed,
        "next_due": next_due,
    });
    println!("{}", serde_json::to_string_pretty(&answer)?);
    // The answer prints before the exit status, so a caller learns what fired either way
    // (`surface.fire.cycle-exit`).
    if !failed.is_empty() {
        let ids: Vec<String> = failed.iter().map(|id| format!("`{id}`")).collect();
        bail!("{} dispatched unit(s) failed: {}", failed.len(), ids.join(", "));
    }
    Ok(())
}

/// `pipeline serve --http` under the external trigger: no tick runs, and each `POST /wake`
/// evaluates the armed set once and answers (`surface.arm.wake-answer`).
fn serve_wakes(scheduler: &mut Scheduler, control: &ControlConfig, addr: &str, project: &Project, reaper: Reaper) -> Result<()> {
    let listener = std::net::TcpListener::bind(addr)?;
    listener.set_nonblocking(true)?;
    eprintln!("wake on http://{}/wake", listener.local_addr()?);
    let watched = reaper.0.clone();
    std::thread::spawn(move || {
        while !contextful_engine::stop::requested() {
            std::thread::sleep(CHILD_POLL);
        }
        watched.end_all(Duration::from_secs(CHILD_GRACE_SECS));
    });
    while !contextful_engine::stop::requested() {
        match listener.accept() {
            Ok((stream, _)) => {
                if let Err(e) = answer_wake(scheduler, control, project, stream) {
                    eprintln!("wake: {e:#}");
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => std::thread::sleep(Duration::from_millis(50)),
            Err(e) => eprintln!("wake: accept: {e}"),
        }
    }
    eprintln!("stopping: ending dispatched children, then releasing the cadence lease");
    drop(reaper);
    report(scheduler.drain());
    scheduler.release()?;
    eprintln!("stopped");
    Ok(())
}

/// Largest request head a wake reads.
const WAKE_HEAD_MAX: usize = 16 * 1024;

/// Read one request's method and path from `stream`.
fn request_line(stream: &mut std::net::TcpStream) -> Result<(String, String)> {
    use std::io::Read;
    stream.set_nonblocking(false)?;
    stream.set_read_timeout(Some(Duration::from_secs(5)))?;
    let mut head = Vec::new();
    let mut buf = [0u8; 1024];
    while !head.windows(4).any(|w| w == b"\r\n\r\n") {
        let n = stream.read(&mut buf)?;
        if n == 0 {
            break;
        }
        head.extend_from_slice(&buf[..n]);
        if head.len() > WAKE_HEAD_MAX {
            bail!("the request head passes {WAKE_HEAD_MAX} bytes");
        }
    }
    let text = String::from_utf8_lossy(&head);
    let mut parts = text.lines().next().unwrap_or_default().split_whitespace();
    Ok((parts.next().unwrap_or_default().to_string(), parts.next().unwrap_or_default().to_string()))
}

fn respond(stream: &mut std::net::TcpStream, status: u16, body: &serde_json::Value) -> Result<()> {
    use std::io::Write;
    let reason = match status {
        200 => "OK",
        404 => "Not Found",
        405 => "Method Not Allowed",
        409 => "Conflict",
        422 => "Unprocessable Content",
        503 => "Service Unavailable",
        _ => "Internal Server Error",
    };
    let body = serde_json::to_vec(body)?;
    write!(stream, "HTTP/1.1 {status} {reason}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", body.len())?;
    stream.write_all(&body)?;
    Ok(())
}

fn answer_wake(scheduler: &mut Scheduler, control: &ControlConfig, project: &Project, mut stream: std::net::TcpStream) -> Result<()> {
    let (method, path) = request_line(&mut stream)?;
    if path != "/wake" {
        return respond(&mut stream, 404, &json!({ "error": format!("no route `{path}`; the wake is `POST /wake`") }));
    }
    if method != "POST" {
        return respond(&mut stream, 405, &json!({ "error": "the wake is `POST /wake`" }));
    }
    match wake(scheduler, control, project, std::time::Instant::now() + Duration::from_secs(WAKE_ANSWER_SECS)) {
        Ok(answer) => respond(&mut stream, 200, &answer),
        Err(e) => {
            let status = e.downcast_ref::<SurfaceError>().map_or(500, SurfaceError::status);
            respond(&mut stream, status, &json!({ "error": format!("{e:#}") }))
        }
    }
}

/// One wake: take the cadence lease, reconcile the applied snapshot, evaluate due-ness once,
/// and wait for the units it dispatched until `deadline`. A unit still running at the deadline
/// reports pending; a failed reconcile leaves the armed set running and reports its
/// diagnostic (`surface.reconcile.fail-static`).
fn wake(scheduler: &mut Scheduler, control: &ControlConfig, project: &Project, deadline: std::time::Instant) -> Result<serde_json::Value> {
    if let LeaseState::HeldBy(holder) = scheduler.hold()? {
        return Ok(json!({ "fired": [], "failed": [], "pending": [], "held_by": holder }));
    }
    let mut diagnostics = Vec::new();
    if let Err(e) = arm(scheduler, &control.source, project) {
        eprintln!("{}; the armed set stays in place", one_line(&e));
        diagnostics.push(one_line(&e));
    }
    let beat = scheduler.beat()?;
    if let LeaseState::HeldBy(holder) = &beat.lease {
        return Ok(json!({ "fired": [], "failed": [], "pending": [], "held_by": holder }));
    }
    report_refused(&beat);
    for id in &beat.started {
        eprintln!("fire {id}: started");
    }
    let ended = scheduler.settle(deadline);
    report(ended.clone());
    let mut fired: Vec<&str> = ended.iter().filter(|f| f.result.is_ok()).map(|f| f.id.as_str()).collect();
    let mut failed: Vec<&str> = ended.iter().filter(|f| f.result.is_err()).map(|f| f.id.as_str()).chain(beat.refused.iter().map(|(id, _)| id.as_str())).collect();
    fired.sort();
    failed.sort();
    let in_flight = scheduler.in_flight();
    let mut pending: Vec<&str> = beat.pending.iter().chain(&beat.held).map(String::as_str).chain(in_flight.iter().map(String::as_str)).collect();
    pending.sort();
    pending.dedup();
    let next_due = scheduler.next_due()?.map(|t| t.to_rfc3339());
    if in_flight.is_empty() {
        scheduler.release()?;
    }
    Ok(json!({
        "fired": fired,
        "failed": failed,
        "pending": pending,
        "armed": scheduler.armed().len(),
        "next_due": next_due,
        "diagnostics": diagnostics,
    }))
}

/// An error as one log line: a parser's multi-line report joins with `; `, so the
/// diagnostic and its disposition read together.
fn one_line(e: &anyhow::Error) -> String {
    format!("{e:#}").lines().map(str::trim).filter(|l| !l.is_empty()).collect::<Vec<_>>().join("; ")
}
