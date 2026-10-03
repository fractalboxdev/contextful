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
use contextful_core::pipeline::declare::{collect, ManifestFile, PipelineSpec};
use contextful_core::run::derive::task::Tasks;
use contextful_core::surface::arm::{Schedule, Trigger, TICK_INTERVAL_MS, WAKE_ANSWER_SECS};
use contextful_core::surface::control::{admit_loopback, control_url, parse_pointer, poll_schedule, snapshot_file, source_file, POINTER_FILE};
use contextful_core::surface::edit::check_document;
use contextful_core::surface::dispatch::DEFAULT_POOL;
use contextful_core::surface::SurfaceError;
use contextful_engine::control::{ControlError, SnapshotDir};
use contextful_engine::scheduler::{Dispatch, Entry, Fired, LeaseState, Scheduler};
use contextful_outbound::egress::{system, Outbound, Transport};
use serde::Serialize;
use serde_json::json;
use std::collections::BTreeMap;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use std::time::Duration;
use url::Url;

/// Claims an apply retries after losing the pointer's compare-and-swap before it gives up.
const CLAIM_ATTEMPTS: usize = 8;

/// Wall clock one control-URL read may take.
const CONTROL_READ_TIMEOUT: Duration = Duration::from_secs(10);

/// Largest pointer or snapshot body a control-URL read takes.
const CONTROL_READ_MAX_BYTES: u64 = 16 * 1024 * 1024;

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
    let trigger = block.get("trigger").map(|v| v.as_str().context("`[control] trigger` is a string")).transpose()?;
    Ok(ControlConfig { source, pool, poll: poll_schedule(poll)?, trigger: Trigger::parse(trigger)? })
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
    let mut entries = Vec::new();
    let mut unarmed = Vec::new();
    for spec in specs.values() {
        let reason = match spec.schedule.as_deref().map(Schedule::parse) {
            Some(Ok(schedule)) => {
                entries.push(Entry { id: spec.id.clone(), schedule });
                continue;
            }
            Some(Err(e)) => e.to_string(),
            None => format!("it declares no `schedule`; `contextful pipeline run {}` fires it", spec.id),
        };
        eprintln!("pipeline `{}` stays unarmed: {reason}", spec.id);
        unarmed.push(Unarmed { id: spec.id.clone(), reason });
    }
    scheduler.arm(version, entries)?;
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
    let dispatch = Arc::new(ChildDispatch {
        exe: std::env::current_exe()?,
        project: l.project.name.clone(),
        declaration: explicit,
        now: project.now.clone(),
    });
    let holder = format!("{}:{}", boot_id(), std::process::id());
    let mut scheduler = Scheduler::new(w.engine.catalog.clone(), dispatch, &l.project.name, &holder, control.pool);
    if cycle {
        return serve_cycle(&mut scheduler, &control, &l.project);
    }
    contextful_engine::stop::install();
    if let Some(addr) = http {
        return serve_wakes(&mut scheduler, &control, addr, &l.project);
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
    eprintln!("stopping: waiting for dispatched units, then releasing the cadence lease");
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
    let ended = scheduler.drain();
    let mut fired: Vec<&str> = ended.iter().filter(|f| f.result.is_ok()).map(|f| f.id.as_str()).collect();
    let mut failed: Vec<&str> = ended.iter().filter(|f| f.result.is_err()).map(|f| f.id.as_str()).collect();
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
fn serve_wakes(scheduler: &mut Scheduler, control: &ControlConfig, addr: &str, project: &Project) -> Result<()> {
    let listener = std::net::TcpListener::bind(addr)?;
    listener.set_nonblocking(true)?;
    eprintln!("wake on http://{}/wake", listener.local_addr()?);
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
    eprintln!("stopping: waiting for dispatched units, then releasing the cadence lease");
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
    for id in &beat.started {
        eprintln!("fire {id}: started");
    }
    let ended = scheduler.settle(deadline);
    report(ended.clone());
    let mut fired: Vec<&str> = ended.iter().filter(|f| f.result.is_ok()).map(|f| f.id.as_str()).collect();
    let mut failed: Vec<&str> = ended.iter().filter(|f| f.result.is_err()).map(|f| f.id.as_str()).collect();
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
