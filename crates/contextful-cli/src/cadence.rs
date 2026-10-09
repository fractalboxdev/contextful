//! `contextful pipeline plan`, `apply` and `serve`: the local control plane.
//!
//! `apply` validates the pipelines and registered jobs it converges and claims the next `manifest@v<N>.toml`
//! in the project's snapshot directory (`surface.apply.local-claim`); it dispatches nothing
//! (`run.declare.apply-fires-nothing`). `serve` arms the applied version's schedules and
//! dispatches each due pipeline or job through this executable with `--applied <N>`.
//! The engine's scheduler holds the deployment's cadence lease and shared dispatch pool.
//!
//! The control source is the snapshot directory, or a loopback `[control] url` serving
//! `manifest@current` and each `manifest@v<N>.toml` beneath it (`surface.reconcile.loopback-only`).

use crate::admit::{revocation_state, AdmitArgs, LedgerFile};
use crate::clock::SystemClock;
use contextful_core::job::{parse_jobs, JobKind};
use contextful_core::run::drive::Bodies;
use crate::pipeline::{check, manifests};
use crate::project::Located;
use crate::run::{boot_id, wire_at, ProjectArgs};
use anyhow::{bail, Context, Result};
use contextful_context::project::Project;
use contextful_core::pipeline::declare::{collect, dependent_runs, ManifestFile, PipelineSpec};
use contextful_core::pipeline::transform::TransformOp;
use contextful_core::grant::Action;
use contextful_core::ports::Clock;
use contextful_core::run::derive::task::Tasks;
use contextful_core::surface::arm::{Schedule, Trigger, TICK_INTERVAL_MS, WAKE_ANSWER_SECS};
use contextful_core::surface::control::{admit_loopback, control_url, parse_pointer, poll_schedule, receipt_file, receipt_version, snapshot_file, source_file, POINTER_FILE};
use contextful_core::store::sync::ControlHead;
use contextful_core::surface::edit::check_document;
use contextful_core::surface::dispatch::{CHILD_GRACE_SECS, DEFAULT_POOL};
use contextful_core::surface::SurfaceError;
use contextful_engine::control::{ControlError, Draft, SnapshotDir};
use contextful_engine::scheduler::{Beat, Dispatch, Entry, Fired, LeaseState, Scheduler};
use contextful_engine::worker::{Relay, WorkerDispatch};
use contextful_policy::control_receipt::ControlReceipt;
use contextful_policy::issue::{SeedSigner, SignerKey, DEFAULT_SEED_PATH};
use contextful_policy::keyset::KeySource;
use contextful_policy::verify::{effect_boundary, Admission, AdmittedAuthority};
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
    bodies: BTreeSet<String>,
    source: Source,
    pool: usize,
    poll: Schedule,
    trigger: Trigger,
    /// Worker base URLs; empty, each unit runs as a child process.
    workers: Vec<String>,
    /// The URL whose `/awake/:token` route `serve` binds for its workers.
    relay: Option<Url>,
    issuer_pin: Option<String>,
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
    Ok(ControlConfig { bodies: BTreeSet::new(), source, pool, poll: poll_schedule(poll)?, trigger: Trigger::parse(trigger)?, workers, relay, issuer_pin: None })
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

/// A locally applied descendant keeps precedence over an older bucket head after every
/// receipt on the local path verifies under the same pinned issuer keys.
fn local_descends(snaps: &SnapshotDir, current: u64, bucket: &ControlHead, project: &str, trusted: &[SignerKey]) -> Result<bool, SurfaceError> {
    let untrusted = |why: String| SurfaceError::ControlSnapshotUntrusted(why);
    let mut versions = Vec::new();
    for entry in std::fs::read_dir(snaps.root()).map_err(|e| untrusted(format!("{}: {e}", snaps.root().display())))? {
        let entry = entry.map_err(|e| untrusted(format!("{}: {e}", snaps.root().display())))?;
        if let Some(version) = entry.file_name().to_str().and_then(receipt_version).filter(|n| *n <= current) {
            versions.push(version);
        }
    }
    versions.sort_unstable_by(|a, b| b.cmp(a));
    if versions.first() != Some(&current) {
        return Err(untrusted(format!("local applied v{current} has no receipt")));
    }
    let mut parent_needed: Option<String> = None;
    for version in versions {
        if version != current && parent_needed.is_none() { break; }
        let receipt_path = snaps.root().join(receipt_file(version));
        let bytes = std::fs::read(&receipt_path).map_err(|e| untrusted(format!("{}: {e}", receipt_path.display())))?;
        let receipt: ControlReceipt = match serde_json::from_slice(&bytes) {
            Ok(receipt) => receipt,
            Err(_) if version != current => continue,
            Err(e) => return Err(untrusted(format!("{}: {e}", receipt_path.display()))),
        };
        if receipt.version != version { continue; }
        let digest = receipt.digest();
        if version != current && parent_needed.as_deref() != Some(digest.as_str()) { continue; }
        let snapshot_path = snaps.root().join(snapshot_file(version));
        let snapshot = std::fs::read(&snapshot_path).map_err(|e| untrusted(format!("{}: {e}", snapshot_path.display())))?;
        receipt.verify(project, &snapshot, trusted).map_err(|e| untrusted(format!("{}: {e}", receipt_path.display())))?;
        if version == bucket.version && digest == bucket.receipt_sha256 {
            return Ok(true);
        }
        parent_needed = receipt.parent;
    }
    Ok(false)
}

/// Adopt only a complete staged chain signed by a locally pinned issuer and matching
/// this node's declarations. The snapshot directory publishes its pointer last.
fn adopt_pulled(snaps: &SnapshotDir, project: &Project, declaration: &Path, tasks: &Tasks, issuer_pin: Option<&str>) -> Result<()> {
    let staged = project.store_root().join("control");
    let head_path = staged.join("head.json");
    let head_bytes = match std::fs::read(&head_path) {
        Ok(bytes) => bytes,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(SurfaceError::ControlSnapshotUntrusted(format!("{}: {e}", head_path.display())).into()),
    };
    let untrusted = |why: String| SurfaceError::ControlSnapshotUntrusted(why);
    let head: ControlHead = serde_json::from_slice(&head_bytes).map_err(|e| untrusted(format!("{}: {e}", head_path.display())))?;
    if head.version == 0 {
        return Err(untrusted("the pulled control head names version 0".into()).into());
    }
    let current = snaps.current().map_err(|e| untrusted(format!("local applied pointer: {e}")))?;
    let pin_text = issuer_pin.filter(|s| !s.trim().is_empty())
        .ok_or_else(|| untrusted(format!("no locally pinned issuer key; set {}", crate::admit::PUBKEY_VAR)))?;
    let pins = crate::admit::static_pins(Some(pin_text)).map_err(|e| untrusted(format!("issuer pins: {e}")))?;
    let keys = pins.keys().map_err(|e| untrusted(format!("issuer pins: {e}")))?;
    let trusted: Vec<SignerKey> = keys.keys().map(|key| SignerKey { algorithm: key.algorithm(), public_key: key.public_key.to_bytes() }).collect();
    let mut versions = Vec::new();
    for entry in std::fs::read_dir(&staged).map_err(|e| untrusted(format!("{}: {e}", staged.display())))? {
        let entry = entry.map_err(|e| untrusted(format!("{}: {e}", staged.display())))?;
        if let Some(version) = entry.file_name().to_str().and_then(receipt_version).filter(|n| *n <= head.version) {
            versions.push(version);
        }
    }
    versions.sort_unstable_by(|a, b| b.cmp(a));
    if versions.first() != Some(&head.version) {
        return Err(untrusted(format!("pulled head v{} has no receipt", head.version)).into());
    }
    let mut wanted = Some(head.receipt_sha256.clone());
    let mut chain = Vec::new();
    for version in versions {
        let Some(digest) = wanted.clone() else { break };
        let receipt_path = staged.join(receipt_file(version));
        let receipt_bytes = match std::fs::read(&receipt_path) {
            Ok(bytes) => bytes,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound && version != head.version => continue,
            Err(e) => return Err(untrusted(format!("{}: {e}", receipt_path.display())).into()),
        };
        let receipt: ControlReceipt = match serde_json::from_slice(&receipt_bytes) {
            Ok(receipt) => receipt,
            Err(_) if version != head.version => continue,
            Err(e) => return Err(untrusted(format!("{}: {e}", receipt_path.display())).into()),
        };
        if receipt.version != version || receipt.digest() != digest {
            if version != head.version { continue; }
            return Err(untrusted(format!("{} differs from the pulled head version or digest", receipt_path.display())).into());
        }
        let snapshot_path = staged.join(snapshot_file(version));
        let snapshot = std::fs::read(&snapshot_path).map_err(|e| untrusted(format!("{}: {e}", snapshot_path.display())))?;
        receipt.verify(&project.name, &snapshot, &trusted).map_err(|e| untrusted(format!("{}: {e}", receipt_path.display())))?;
        wanted = receipt.parent;
        chain.push((version, snapshot, receipt_bytes, digest));
    }
    if let Some(missing) = wanted {
        return Err(untrusted(format!("pulled control chain lacks predecessor {missing}")).into());
    }
    if let Some(version) = current {
        if version > head.version {
            if local_descends(snaps, version, &head, &project.name, &trusted)? {
                return Ok(());
            }
            return Err(untrusted(format!("local applied v{version} does not descend from bucket head {}", head.receipt_sha256)).into());
        }
        let path = snaps.root().join(receipt_file(version));
        let bytes = std::fs::read(&path).map_err(|e| untrusted(format!("{}: {e}", path.display())))?;
        let receipt: ControlReceipt = serde_json::from_slice(&bytes).map_err(|e| untrusted(format!("{}: {e}", path.display())))?;
        if !chain.iter().any(|(n, _, _, digest)| *n == version && *digest == receipt.digest()) {
            return Err(untrusted(format!("pulled head {} does not descend from local head {}", head.receipt_sha256, receipt.digest())).into());
        }
    }
    let latest = &chain[0].1;
    let text = std::str::from_utf8(latest).map_err(|e| untrusted(format!("pulled snapshot is not UTF-8: {e}")))?;
    let document: toml::Value = toml::from_str(text).map_err(|e| untrusted(format!("pulled snapshot: {e}")))?;
    check_document(&document).map_err(|e| untrusted(format!("pulled snapshot: {e}")))?;
    let pulled = collect(&[ManifestFile { path: snapshot_file(head.version), text: text.to_string() }])
        .map_err(|e| untrusted(format!("pulled snapshot: {e}")))?;
    let local = declared(declaration).map_err(|e| untrusted(format!("local declarations: {e:#}")))?;
    let pulled_jobs = job_blocks(text, None, &local).map_err(|e| untrusted(format!("pulled jobs: {e:#}")))?;
    let local_jobs = job_blocks(&std::fs::read_to_string(declaration)?, None, &local)?;
    if pulled_jobs.iter().any(|job| !local_jobs.contains(job)) {
        return Err(untrusted("pulled jobs differ from this node's declarations".into()).into());
    }
    if pulled.iter().any(|p| local.get(&p.spec.id).is_none_or(|d| d.content_hash() != p.spec.content_hash())) {
        return Err(untrusted("pulled snapshot differs from this node's pipeline declarations".into()).into());
    }
    for pipeline in &pulled {
        check(&pipeline.spec, declaration, tasks).map_err(|e| untrusted(format!("pipeline `{}`: {e:#}", pipeline.spec.id)))?;
    }
    chain.reverse();
    let files: Vec<(u64, Vec<u8>, Vec<u8>)> = chain.into_iter().map(|(n, snapshot, receipt, _)| (n, snapshot, receipt)).collect();
    snaps.adopt(current, &files, head.version).map_err(|e| untrusted(format!("local control adoption: {e}")))?;
    eprintln!("adopted pulled control v{}", head.version);
    Ok(())
}

/// Validate jobs through their owning schema and retain their declaration verbatim.
fn job_blocks(text: &str, bodies: Option<&Bodies>, specs: &BTreeMap<String, PipelineSpec>) -> Result<Vec<toml::Value>> {
    parse_job_blocks(text, bodies, specs).map_err(|error| SurfaceError::ApplyValidationRefused(format!("job declarations: {error:#}")).into())
}

fn parse_job_blocks(text: &str, bodies: Option<&Bodies>, specs: &BTreeMap<String, PipelineSpec>) -> Result<Vec<toml::Value>> {
    let jobs = parse_jobs(text, &|name| bodies.is_none_or(|b| b.get(name).is_some()))?;
    for job in jobs {
        let id = format!("job:{}", job.name);
        if specs.contains_key(&id) {
            bail!("job `{}` and pipeline `{id}` share a dispatch identity", job.name);
        }
        if let Some(schedule) = job.schedule.as_deref() {
            Schedule::parse(schedule).with_context(|| format!("job `{}` schedule", job.name))?;
        }
    }
    let document: toml::Value = toml::from_str(text)?;
    Ok(document.get("job").and_then(toml::Value::as_array).cloned().unwrap_or_default())
}

fn applied_jobs(source: &Source, version: Option<u64>, specs: &BTreeMap<String, PipelineSpec>) -> Result<Vec<toml::Value>> {
    version.map(|v| job_blocks(&source.read(v)?, None, specs)).transpose().map(Option::unwrap_or_default)
}

/// A pipeline-only editor refuses a store whose applied snapshot carries jobs.
fn refuse_job_edit(control: &ControlConfig) -> Result<()> {
    let (version, specs) = applied(&control.source)?;
    if !applied_jobs(&control.source, version, &specs)?.is_empty() {
        return Err(SurfaceError::ApplyValidationRefused("the applied snapshot contains jobs; use the registered host's pipeline apply to edit them".into()).into());
    }
    Ok(())
}

/// The snapshot document: sorted pipeline specifications and validated job blocks.
fn render(specs: &BTreeMap<String, PipelineSpec>, jobs: &[toml::Value]) -> Result<String> {
    #[derive(Serialize)]
    struct Doc<'a> {
        pipeline: Vec<&'a PipelineSpec>,
        #[serde(skip_serializing_if = "<[toml::Value]>::is_empty")]
        job: &'a [toml::Value],
    }
    let body = toml::to_string(&Doc { pipeline: specs.values().collect(), job: jobs })
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
    if !job_blocks(document, None, &specs)?.is_empty() {
        return Err(SurfaceError::ApplyValidationRefused("the pipeline editor does not edit jobs; use the registered host's pipeline apply".into()).into());
    }
    let rendered = render(&specs, &[])?;
    for spec in specs.values() {
        if let Some(schedule) = spec.schedule.as_deref() {
            Schedule::parse(schedule).map_err(|e| SurfaceError::ApplyValidationRefused(format!("pipeline `{}`: {e}", spec.id)))?;
        }
        check(spec, declaration, tasks).map_err(|e| SurfaceError::ApplyValidationRefused(format!("pipeline `{}`: {e:#}", spec.id)))?;
    }
    Ok(rendered)
}

/// Store one validated, version-bound control draft without changing the applied pointer.
pub(crate) fn edit(project: &ProjectArgs, declaration: Option<PathBuf>, expected: u64, document: &str, operator: &str, tasks: &Tasks, boundary: &dyn Fn() -> Result<(), ControlError>) -> Result<Value> {
    let (located, _, control) = located(project, declaration)?;
    refuse_job_edit(&control)?;
    let snapshots = owner(&control)?;
    snapshots.initialized()?;
    let text = validated_draft(document, &located.declaration, tasks)?;
    let draft = Draft::new(expected, text, operator.to_owned())?;
    snapshots.save_draft_guarded(&draft, boundary)?;
    Ok(json!({ "expected": expected, "nonce": draft.nonce }))
}

/// Claim a verified control request's nonce in the selected store owner's snapshot state.
pub(crate) fn claim_operator_nonce(project: &ProjectArgs, declaration: Option<PathBuf>, nonce: &str, signed_at: i64, now: i64, boundary: &dyn Fn() -> Result<(), ControlError>) -> Result<bool> {
    let (_, _, control) = located(project, declaration)?;
    owner(&control)?.claim_attestation_nonce_guarded(nonce, signed_at, now, boundary).map_err(Into::into)
}

/// Revalidate the saved draft and claim it only at the version the editor read.
pub(crate) fn apply_draft(project: &ProjectArgs, declaration: Option<PathBuf>, expected: u64, nonce: &str, operator: &str, tasks: &Tasks, authority: &AdmittedAuthority, admit: &AdmitArgs, boundary: &dyn Fn() -> Result<(), ControlError>) -> Result<()> {
    let (initial, _, control) = located(project, declaration.clone())?;
    refuse_job_edit(&control)?;
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
    match SyncAttestation::for_authority(&fresh, authority.clone(), admit, None)? {
        Some(attestation) => {
            owner.claim_draft_attested_guarded(&draft, |next, previous| {
                let prior_snapshot = owner.read(expected)?;
                let parent = previous.map(|receipt| (receipt, expected, prior_snapshot.as_str()));
                attestation.receipt(&fresh.project.name, next, parent, &draft.document)
            }, boundary)?;
        }
        None => { owner.claim_draft_guarded(&draft, boundary)?; }
    }
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

fn change(id: &str, declared: Option<(String, Option<String>)>, applied: Option<(String, Option<String>)>) -> Change {
    let action = match (&declared, &applied) {
        (Some(_), None) => "add",
        (None, _) => "remove",
        (Some((x, _)), Some((y, _))) if x == y => "unchanged",
        _ => "change",
    };
    let schedule = declared.as_ref().or(applied.as_ref()).and_then(|(_, schedule)| schedule.clone());
    Change { id: id.into(), action, content_hash: declared.map(|(hash, _)| hash), applied_hash: applied.map(|(hash, _)| hash), schedule }
}

fn diff(declared: &BTreeMap<String, PipelineSpec>, applied: &BTreeMap<String, PipelineSpec>) -> Vec<Change> {
    let ids: BTreeSet<&String> = declared.keys().chain(applied.keys()).collect();
    let identity = |spec: &PipelineSpec| (spec.content_hash(), spec.schedule.clone());
    ids.into_iter().map(|id| change(id, declared.get(id).map(identity), applied.get(id).map(identity))).collect()
}

fn job_diff(declared: &[toml::Value], applied: &[toml::Value]) -> Vec<Change> {
    fn identities(jobs: &[toml::Value]) -> BTreeMap<String, (String, Option<String>)> {
        jobs.iter().filter_map(|job| {
            let name = job.get("name")?.as_str()?;
            let hash = contextful_core::run::journal::sha256_hex(job.to_string().as_bytes());
            let schedule = job.get("schedule").and_then(toml::Value::as_str).map(str::to_owned);
            Some((format!("job:{name}"), (hash, schedule)))
        }).collect()
    }
    let (declared, applied) = (identities(declared), identities(applied));
    let ids: BTreeSet<&String> = declared.keys().chain(applied.keys()).collect();
    ids.into_iter().map(|id| change(id, declared.get(id).cloned(), applied.get(id).cloned())).collect()
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
    let (l, declaration_text, control) = located(project, declaration)?;
    let (version, applied) = applied(&control.source)?;
    let declared = declared(&l.declaration)?;
    let changes = diff(&declared, &applied);
    let jobs = job_diff(&job_blocks(&declaration_text, None, &declared)?, &applied_jobs(&control.source, version, &applied)?);
    if as_json {
        println!("{}", serde_json::to_string_pretty(&json!({ "applied": version, "pipelines": changes, "jobs": jobs }))?);
        return Ok(());
    }
    println!("applied: {}", version.map(|v| format!("v{v}")).unwrap_or_else(|| "none".into()));
    for c in changes.iter().chain(&jobs) {
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

/// The admitted admin capability and issuer port a synced control claim needs.
struct SyncAttestation {
    authority: AdmittedAuthority,
    signer: SeedSigner,
    ledger: LedgerFile,
    denylist: Option<PathBuf>,
}

impl SyncAttestation {
    fn for_project(l: &Located, project: &ProjectArgs, admit: &AdmitArgs, issuer_key: Option<&Path>) -> Result<Option<Self>> {
        if crate::sync::sync_config(l)?.0.is_none() {
            return Ok(None);
        }
        let unavailable = |why: String| SurfaceError::ControlAttestationUnavailable(why);
        let (authority, _) = admit.admit(project.project.as_deref(), "a synced control claim")
            .map_err(|e| unavailable(format!("admin capability: {e:#}")))?;
        Self::for_authority(l, authority, admit, issuer_key)
    }

    fn for_authority(l: &Located, authority: AdmittedAuthority, admit: &AdmitArgs, issuer_key: Option<&Path>) -> Result<Option<Self>> {
        if crate::sync::sync_config(l)?.0.is_none() {
            return Ok(None);
        }
        let unavailable = |why: String| SurfaceError::ControlAttestationUnavailable(why);
        if !authority.permits(Action::Admin, &[]) {
            return Err(unavailable("the credential carries no admin grant".into()).into());
        }
        let seed = issuer_key.map(Path::to_path_buf).unwrap_or_else(|| l.project.dir.join(DEFAULT_SEED_PATH));
        let signer = SeedSigner::resolve(Some(&seed)).map_err(|e| unavailable(format!("issuer signing port: {e}")))?;
        Ok(Some(SyncAttestation {
            authority,
            signer,
            ledger: LedgerFile::at(&l.project.dir, admit.keyset.as_deref()),
            denylist: admit.denylist.clone(),
        }))
    }

    fn receipt(
        &self,
        project: &str,
        version: u64,
        previous: Option<(&str, u64, &str)>,
        snapshot: &str,
    ) -> Result<String, ControlError> {
        let unavailable = |why: String| ControlError::Surface(SurfaceError::ControlAttestationUnavailable(why));
        let ledger = self.ledger.read().map_err(|e| unavailable(format!("key-set ledger: {e}")))?;
        let revocation = revocation_state(self.denylist.as_deref(), &ledger)
            .map_err(|e| unavailable(format!("revocation state: {e:#}")))?;
        effect_boundary(&self.authority, &Admission::new(SystemClock.now(), &revocation))
            .map_err(|e| unavailable(format!("admin capability: {e}")))?;
        let parent = previous.map(|(text, expected, prior_snapshot)| {
            let receipt: ControlReceipt = serde_json::from_str(text)
                .map_err(|e| unavailable(format!("previous receipt: {e}")))?;
            if receipt.version != expected {
                return Err(unavailable(format!("previous receipt names v{} instead of v{expected}", receipt.version)));
            }
            let signer: SignerKey = receipt.signer.parse()
                .map_err(|e| unavailable(format!("previous receipt signer: {e}")))?;
            receipt.verify(project, prior_snapshot.as_bytes(), &[signer])
                .map_err(|e| unavailable(format!("previous receipt: {e}")))?;
            Ok(receipt.digest())
        }).transpose()?;
        let receipt = ControlReceipt::sign(project, version, parent.as_deref(), snapshot.as_bytes(), &self.signer)
            .map_err(|e| unavailable(format!("issuer signing port: {e}")))?;
        serde_json::to_string(&receipt).map_err(|e| unavailable(format!("receipt encoding: {e}")))
    }
}

/// `pipeline import`: the guarded import, claiming v1 from every declared pipeline while the
/// directory holds no version (`surface.apply.uninitialized-store`).
pub(crate) fn import(project: &ProjectArgs, declaration: Option<PathBuf>, tasks: &Tasks, bodies: &Bodies, admit: &AdmitArgs, issuer_key: Option<&Path>) -> Result<()> {
    let (l, declaration_text, control) = located(project, declaration)?;
    let snapshots = owner(&control)?;
    let attestation = SyncAttestation::for_project(&l, project, admit, issuer_key)?;
    let declared = declared(&l.declaration)?;
    let jobs = job_blocks(&declaration_text, Some(bodies), &declared)?;
    let text = render(&declared, &jobs)?;
    for spec in declared.values() {
        check(spec, &l.declaration, tasks).map_err(|e| SurfaceError::ApplyValidationRefused(format!("pipeline `{}`: {e:#}", spec.id)))?;
    }
    let v = match &attestation {
        Some(attestation) => snapshots.import_attested(&text,
            |version, _| attestation.receipt(&l.project.name, version, None, &text),
            |existing| {
                let unavailable = |why: String| ControlError::Surface(SurfaceError::ControlAttestationUnavailable(why));
                let receipt: ControlReceipt = serde_json::from_str(existing)
                    .map_err(|error| unavailable(format!("unpublished receipt: {error}")))?;
                if receipt.version != 1 || receipt.parent.is_some() {
                    return Err(unavailable("unpublished receipt is not the first version".into()));
                }
                receipt.verify(&l.project.name, text.as_bytes(), &[SignerKey::of(&attestation.signer)])
                    .map_err(|error| unavailable(format!("unpublished receipt: {error}")))
            },
        )?,
        None => snapshots.import(&text)?,
    };
    for id in declared.keys() {
        println!("+ {id}");
    }
    println!("imported v{v}");
    Ok(())
}

/// `pipeline apply [id]`: validate what converges, then claim the next version. Losing the
/// pointer's compare-and-swap reloads the winner and reapplies onto it
/// (`surface.apply.version-race`).
pub(crate) fn apply(project: &ProjectArgs, declaration: Option<PathBuf>, id: Option<&str>, tasks: &Tasks, bodies: &Bodies, admit: &AdmitArgs, issuer_key: Option<&Path>) -> Result<()> {
    let (l, declaration_text, control) = located(project, declaration)?;
    let snapshots = owner(&control)?;
    let attestation = SyncAttestation::for_project(&l, project, admit, issuer_key)?;
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
        let prior_jobs = applied_jobs(source, version, &base)?;
        let jobs = if id.is_some() { prior_jobs.clone() } else {
            job_blocks(&declaration_text, Some(bodies), &target)?
        };
        let text = render(&target, &jobs)?;
        job_blocks(&text, Some(bodies), &target)?;
        collect(&[ManifestFile { path: "proposed applied snapshot".into(), text: text.clone() }])
            .map_err(|e| SurfaceError::ApplyValidationRefused(format!("combined snapshot: {e}")))?;
        for c in changes.iter().filter(|c| c.action != "remove") {
            let spec = &target[&c.id];
            check(spec, &l.declaration, tasks)
                .map_err(|e| SurfaceError::ApplyValidationRefused(format!("pipeline `{}`: {e:#}", spec.id)))?;
        }
        if changes.is_empty() && jobs == prior_jobs {
            match version {
                Some(v) => println!("unchanged at v{v}"),
                None => println!("nothing declared to apply"),
            }
            return Ok(());
        }
        let claim = match &attestation {
            Some(attestation) => snapshots.claim_attested(version, &text, |next, previous| {
                let prior_snapshot = version.map(|old| snapshots.read(old)).transpose()?;
                let parent = previous.zip(version).zip(prior_snapshot.as_deref())
                    .map(|((receipt, old), snapshot)| (receipt, old, snapshot));
                attestation.receipt(&l.project.name, next, parent, &text)
            }),
            None => snapshots.claim(version, &text),
        };
        match claim {
            Ok(v) => {
                for c in changes.iter().chain(job_diff(&jobs, &prior_jobs).iter().filter(|c| c.action != "unchanged")) {
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

enum ChildStepError {
    Run(String),
    Infrastructure(String),
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
    fn fire(&self, id: &str, steps: &[String], derived_parents: &BTreeMap<String, String>, version: u64) -> Result<String, String> {
        contextful_engine::scheduler::fire_ordered(id, steps, derived_parents, |step| match self.step(step, version) {
            Ok(line) => Ok(contextful_engine::scheduler::CompletedStep::Succeeded(line)),
            Err(ChildStepError::Run(error)) => Ok(contextful_engine::scheduler::CompletedStep::Failed(error)),
            Err(ChildStepError::Infrastructure(error)) => Err(error),
        })
    }
}

impl ChildDispatch {
    /// Run one applied pipeline or registered job through this embedding executable.
    fn step(&self, id: &str, version: u64) -> Result<String, ChildStepError> {
        let mut cmd = Command::new(&self.exe);
        let selected = (|| -> Result<Option<String>> {
            let located = crate::project::locate(Some(&self.project), self.declaration.clone())?;
            let manifest = std::fs::read_to_string(&located.declaration)?;
            let snapshot = snapshot_manifest(&located.project, &manifest, version)?;
            let jobs = parse_jobs(&snapshot.text, &|_| true)?;
            Ok(jobs.into_iter().find(|job| id == format!("job:{}", job.name)).map(|job| job.name))
        })().map_err(|e| ChildStepError::Infrastructure(format!("{id}: reading applied dispatch: {e:#}")))?;
        match selected {
            Some(name) => { cmd.args(["job", "fire", &name]); }
            None => { cmd.args(["pipeline", "run", id]); }
        }
        cmd.args(["--applied", &version.to_string(), "--project", &self.project]);
        if let Some(d) = &self.declaration {
            cmd.arg("--declaration").arg(d);
        }
        if let Some(now) = &self.now {
            cmd.args(["--now", now]);
        }
        cmd.stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped());
        let (pid, out, err) = self.children.spawn(&mut cmd).map_err(|e| ChildStepError::Infrastructure(format!("{id}: starting `{}`: {e}", self.exe.display())))?;
        let (out, err) = (drain_pipe(out), drain_pipe(err));
        let status = self.children.wait(pid).map_err(|e| ChildStepError::Infrastructure(format!("{id}: waiting on child {pid}: {e}")))?;
        let (stdout, stderr) = (out.join().unwrap_or_default(), err.join().unwrap_or_default());
        for line in stdout.lines().chain(stderr.lines()) {
            eprintln!("[{id}] {line}");
        }
        let last = |t: &str| t.lines().last().unwrap_or_default().to_string();
        if status.success() {
            Ok(last(&stdout))
        } else if status.code().is_none() {
            Err(ChildStepError::Infrastructure(format!("child {pid} ended: {status}")))
        } else if stderr.trim().is_empty() {
            Err(ChildStepError::Run(format!("child {pid} ended: {status}")))
        } else {
            Err(ChildStepError::Run(last(&stderr)))
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
fn arm(scheduler: &mut Scheduler, control: &ControlConfig, project: &Project, declaration: &Path, tasks: &Tasks) -> Result<Option<Vec<Unarmed>>> {
    let store = contextful_context::Store::open(&project.dir, &project.name)?;
    scheduler.observe_runs(contextful_sync::run_state::newest_starts(&store)?);
    if let Source::Dir(directory) = &control.source {
        adopt_pulled(directory, project, declaration, tasks, control.issuer_pin.as_deref())?;
    }
    let (version, specs) = applied(&control.source)?;
    let Some(version) = version else { return Ok(None) };
    if version == scheduler.version() {
        return Ok(Some(Vec::new()));
    }
    let runs = dependent_runs(specs.values()).map_err(|e| SurfaceError::ControlSnapshotUnreadable(format!("{}: {e}", snapshot_file(version))))?;
    let mut entries = Vec::new();
    let mut unarmed = Vec::new();
    for spec in specs.values() {
        if runs.derived_parents.contains_key(&spec.id) {
            let head = runs.head_of.get(&spec.id).expect("a derived child has a head");
            let reason = format!("it lands after its derive parent in the run headed by `{head}`");
            eprintln!("pipeline `{}` stays unarmed: {reason}", spec.id);
            unarmed.push(Unarmed { id: spec.id.clone(), reason });
            continue;
        }
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
    let job_text = control.source.read(version)?;
    job_blocks(&job_text, None, &specs)?;
    let jobs = parse_jobs(&job_text, &|name| control.bodies.contains(name))?;
    let mut scheduled_jobs = 0;
    for job in jobs {
        let id = format!("job:{}", job.name);
        let reason = match (&job.kind, job.schedule.as_deref()) {
            (JobKind::StoreDriven(_), Some(text)) => {
                if control.relay.is_some() && !control.workers.is_empty() {
                    bail!("scheduled job `{}` requires the local registered host; worker dispatch handles pipelines", job.name);
                }
                entries.push(Entry { id, schedule: Schedule::parse(text)? });
                scheduled_jobs += 1;
                continue;
            }
            (_, None) => "it declares no schedule".to_string(),
            _ => "its maintenance kind has no native dispatch adapter".to_string(),
        };
        eprintln!("job `{}` stays unarmed: {reason}", job.name);
        unarmed.push(Unarmed { id, reason });
    }
    scheduler.arm_runs_with_derived(version, entries, runs.steps, runs.derived_parents)?;
    eprintln!("armed v{version}: {} scheduled pipeline(s), {scheduled_jobs} scheduled job(s), {} unarmed", scheduler.armed().len() - scheduled_jobs, unarmed.len());
    Ok(Some(unarmed))
}

/// `pipeline serve [--cycle]`.
pub(crate) fn serve(project: &ProjectArgs, declaration: Option<PathBuf>, cycle: bool, http: Option<&str>, public_key: Option<&str>, tasks: &Tasks, bodies: &Bodies) -> Result<()> {
    let explicit = declaration.clone();
    let (l, text, mut control) = located(project, declaration)?;
    control.bodies = bodies.names().into_iter().collect();
    control.issuer_pin = public_key.map(str::to_string).or_else(|| std::env::var(crate::admit::PUBKEY_VAR).ok());
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
        let answered = serve_cycle(&mut scheduler, &control, &l.project, &l.declaration, tasks);
        drop(reaper);
        answered?;
        if contextful_engine::stop::requested() {
            bail!("a stop signal ended the cycle's dispatched units");
        }
        return Ok(());
    }
    if let Some(addr) = http {
        return serve_wakes(&mut scheduler, &control, addr, &l.project, &l.declaration, tasks, reaper);
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
                    if let Err(e) = arm(&mut scheduler, &control, &l.project, &l.declaration, tasks) {
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
fn serve_cycle(scheduler: &mut Scheduler, control: &ControlConfig, project: &Project, declaration: &Path, tasks: &Tasks) -> Result<()> {
    if let LeaseState::HeldBy(holder) = scheduler.hold()? {
        return held_answer(&holder);
    }
    let armed = match arm(scheduler, control, project, declaration, tasks) {
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
fn serve_wakes(scheduler: &mut Scheduler, control: &ControlConfig, addr: &str, project: &Project, declaration: &Path, tasks: &Tasks, reaper: Reaper) -> Result<()> {
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
                if let Err(e) = answer_wake(scheduler, control, project, declaration, tasks, stream) {
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

fn answer_wake(scheduler: &mut Scheduler, control: &ControlConfig, project: &Project, declaration: &Path, tasks: &Tasks, mut stream: std::net::TcpStream) -> Result<()> {
    let (method, path) = request_line(&mut stream)?;
    if path != "/wake" {
        return respond(&mut stream, 404, &json!({ "error": format!("no route `{path}`; the wake is `POST /wake`") }));
    }
    if method != "POST" {
        return respond(&mut stream, 405, &json!({ "error": "the wake is `POST /wake`" }));
    }
    match wake(scheduler, control, project, declaration, tasks, std::time::Instant::now() + Duration::from_secs(WAKE_ANSWER_SECS)) {
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
fn wake(scheduler: &mut Scheduler, control: &ControlConfig, project: &Project, declaration: &Path, tasks: &Tasks, deadline: std::time::Instant) -> Result<serde_json::Value> {
    if let LeaseState::HeldBy(holder) = scheduler.hold()? {
        return Ok(json!({ "fired": [], "failed": [], "pending": [], "held_by": holder }));
    }
    let mut diagnostics = Vec::new();
    if let Err(e) = arm(scheduler, control, project, declaration, tasks) {
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
