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
use contextful_core::job::{parse_jobs, Job, JobKind, Reach};
use contextful_core::run::drive::Bodies;
use crate::pipeline::{check, manifests};
use crate::project::Located;
use crate::run::{boot_id, wire_at, ProjectArgs};
use anyhow::{bail, Context, Result};
use contextful_context::project::Project;
use contextful_core::pipeline::declare::{collect, ManifestFile, PipelineSpec};
use contextful_core::pipeline::transform::TransformOp;
use contextful_core::pipeline::model::ModelSpec;
use contextful_core::store::declare::TableDecl;
use contextful_core::grant::{Action, Grant, TablePattern};
use contextful_core::ports::Clock;
use contextful_core::run::derive::task::Tasks;
use contextful_core::surface::arm::{Schedule, Trigger, TICK_INTERVAL_MS, WAKE_ANSWER_SECS};
use contextful_core::surface::control::{admit_loopback, control_url, parse_pointer, poll_schedule, receipt_file, receipt_version, snapshot_file, source_file, POINTER_FILE};
use contextful_core::store::sync::ControlHead;
use contextful_core::surface::edit::check_document;
use contextful_core::surface::external::{bind_externals, parse_externals, External, Known, Ref};
use contextful_core::surface::dispatch::{CHILD_GRACE_SECS, DEFAULT_POOL};
use contextful_core::surface::SurfaceError;
use contextful_snapshot::{ControlError, Draft, SnapshotDir};
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
    /// The delegates the daemon's own `[control.delegates]` binds (`surface.delegate.binding`).
    delegates: BTreeMap<String, contextful_core::surface::delegate::Binding>,
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
    // A binding outside its shape refuses at startup and arms nothing (`surface.delegate.host`).
    let delegates = contextful_core::surface::delegate::bindings(block)?;
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
    Ok(ControlConfig { bodies: BTreeSet::new(), delegates, source, pool, poll: poll_schedule(poll)?, trigger: Trigger::parse(trigger)?, workers, relay, issuer_pin: None })
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

/// Capture the current pointer once and read that version's immutable document.
pub(crate) fn current_manifest(project: &Project, text: &str) -> Result<Option<(u64, ManifestFile)>> {
    let control = control_config(text, project)?;
    control.source.current()?.map(|version| Ok((version, ManifestFile { path: snapshot_file(version), text: control.source.read(version)? }))).transpose()
}

/// The applied version and the specifications it holds, by id; none before the first apply.
fn applied(snaps: &Source) -> Result<(Option<u64>, BTreeMap<String, PipelineSpec>)> {
    let Some(version) = snaps.current()? else { return Ok((None, BTreeMap::new())) };
    let text = snaps.read(version)?;
    let specs = crate::effective::public_manifest(ManifestFile { path: snapshot_file(version), text })
        .and_then(|file| collect(&[file]).map_err(Into::into))
        .map_err(|e| SurfaceError::ControlSnapshotUnreadable(format!("{}: {e}", snapshot_file(version))))?;
    Ok((Some(version), specs.into_iter().map(|d| (d.spec.id.clone(), d.spec)).collect()))
}

/// The applied snapshot and recorded run outcomes the store publishes for Admin: pipelines,
/// jobs, models, publications and external steps, joined by an explicit edge list
/// (`surface.visualize.workflow-canvas`, `surface.visualize.dataflow-edges`).
pub(crate) fn published(project: &Project, declaration: &Path) -> Result<Value> {
    let text = std::fs::read_to_string(declaration)?;
    let control = control_config(&text, project)?;
    let (version, pipelines) = applied(&control.source)?;
    let snapshot = version.map(|v| control.source.read(v)).transpose()?.unwrap_or_default();
    let truncated = pipelines.len() > 1000;
    let pipelines: Vec<PipelineSpec> = pipelines.into_values().take(1000).collect();
    let jobs = parse_jobs(&snapshot, &|_| true)?;
    let mut selected: BTreeSet<String> = pipelines.iter().map(|spec| spec.id.clone()).collect();
    selected.extend(jobs.iter().map(|job| format!("job:{}", job.name)));
    let store = contextful_context::Store::open(&project.dir, &project.name)?;
    let states = contextful_sync::run_state::run_states(&store)?;
    let mut runs: BTreeMap<String, contextful_sync::RunMark> = BTreeMap::new();
    let mut evidence = crate::source_runs::Summaries::new();
    for state in states.into_values() {
        crate::source_runs::merge(&mut evidence, state.sources);
        for (id, mark) in state.runs {
            if selected.contains(&id) && runs.get(&id).is_none_or(|earlier| mark.started_at > earlier.started_at) {
                runs.insert(id, mark);
            }
        }
    }
    let wired = wire_at(project, &None)?;
    let rows = wired.engine.catalog.runs(None)?;
    for row in &rows {
        if selected.contains(&row.pipeline_id) && runs.get(&row.pipeline_id).is_none_or(|earlier| row.started_at > earlier.started_at) {
            runs.insert(row.pipeline_id.clone(), contextful_sync::RunMark {
                run_id: row.run_id.clone(),
                table: row.table.clone(),
                status: row.status,
                started_at: row.started_at,
                ended_at: row.ended_at,
            });
        }
    }
    let models = applied_models(&control.source, version)?;
    let externals = parse_externals(&snapshot)?;
    let publications = local_publications(&text)?;
    let graph = Graph::of(&pipelines, &jobs, &models, &publications, &externals)?;
    crate::source_runs::merge(&mut evidence, crate::source_runs::local(&project.audit_dir(), &rows)?);
    let sources: Vec<Value> = pipelines.iter().map(|spec| {
        let outcomes = version.and_then(|version| evidence.get(&(spec.id.clone(), version, spec.content_hash())));
        let outcome = |fire: &contextful_sync::SourceFire| json!({
            "run_id": fire.run_id, "status": fire.status, "rows": fire.rows, "started_at": fire.started_at, "ended_at": fire.ended_at,
            "error_kind": fire.error_kind, "message": fire.error_kind.map(source_failure_message).or((fire.status == contextful_core::run::record::RunStatus::Failed).then_some("A destination refused this source run. Check the output table configuration and retry.")),
        });
        json!({ "id": spec.id, "connector": spec.source.name,
            "destinations": spec.tables.iter().map(|table| spec.table_name(table.name())).collect::<Vec<_>>(),
            "schedule": spec.schedule, "trigger": if spec.source.name == "receive" { "receive" } else if spec.after.is_some() { "after" } else if spec.schedule.is_some() { "schedule" } else { "manual" },
            "after": spec.after, "version": version, "first_run": outcomes.map(|summary| outcome(&summary.first)), "latest_run": outcomes.map(|summary| outcome(&summary.latest)),
            "state": outcomes.map_or("pending", |summary| summary.latest.status.name()),
        })
    }).collect();
    let document = crate::effective::public_manifest(ManifestFile { path: "applied document".into(), text: snapshot.clone() })?.text;
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
    let jobs: Vec<Value> = jobs.iter().map(|job| json!({
        "name": job.name,
        "kind": job.kind_name(),
        "schedule": job.schedule,
        "after": job.after,
        "target": job.target,
    })).collect();
    let models: Vec<Value> = models.iter().map(|model| json!({ "id": model.id, "publish": model.publishes() })).collect();
    let publications: Vec<Value> = publications.iter().map(|(name, job, members)| json!({ "name": name, "job": job, "members": members })).collect();
    Ok(json!({
        "applied": version,
        "document": document,
        "sources": sources,
        "pipelines": pipelines,
        "jobs": jobs,
        "models": models,
        "publications": publications,
        "externals": externals,
        "tables": graph.tables,
        "edges": graph.edges.iter().map(|(from, to, via)| json!({ "from": from, "to": to, "via": via })).collect::<Vec<_>>(),
        "runs": runs,
        "truncated": truncated,
        "declined": 0,
    }))
}

fn source_failure_message(tag: contextful_core::run::FailureTag) -> &'static str {
    use contextful_core::run::FailureTag;
    match tag {
        FailureTag::Transient => "The source could not complete its request. Check endpoint availability and retry.",
        FailureTag::Permanent => "The source rejected the request. Check its endpoint and connector options before retrying.",
        FailureTag::AuthExpired => "The source credential expired. Renew its secret reference and retry.",
        FailureTag::RateLimited => "The source rate limit stopped this run. Wait for the limit to reset and retry.",
        FailureTag::SchemaIncompatible => "The source rows do not match the destination schema. Check table columns and transforms.",
        FailureTag::SecretNotFound => "A source secret reference is unavailable. Configure its credential provider and retry.",
        FailureTag::Storage => "The destination could not persist the run. Check store access and available storage.",
        FailureTag::UnknownConnector => "The source connector is unavailable. Check its name and the server's connector support.",
        FailureTag::Config => "The connector configuration refused the run. Check its required fields and table mappings.",
        FailureTag::Canceled => "The source run was canceled. Start another run when the source is ready.",
    }
}

/// The local `[[publication]]` blocks as `(name, producer job, members)`; delivery
/// validation belongs to `export group`, so the canvas reads the shape alone.
fn local_publications(text: &str) -> Result<Vec<(String, String, Vec<String>)>> {
    let document: toml::Value = if text.is_empty() { toml::Value::Table(Default::default()) } else { toml::from_str(text)? };
    Ok(document.get("publication").and_then(toml::Value::as_array).into_iter().flatten().filter_map(|block| {
        let name = block.get("name")?.as_str()?.to_string();
        let job = block.get("job")?.as_str()?.to_string();
        let members = block.get("members").and_then(toml::Value::as_array).into_iter().flatten().filter_map(|m| m.as_str().map(str::to_string)).collect();
        Some((name, job, members))
    }).collect())
}

/// The workflow graph's table nodes and its `(from, to, via)` edges between typed node ids
/// (`surface.visualize.dataflow-edges`).
struct Graph {
    tables: BTreeSet<String>,
    edges: BTreeSet<(String, String, &'static str)>,
}

impl Graph {
    fn of(pipelines: &[PipelineSpec], jobs: &[contextful_core::job::Job], models: &[ModelSpec], publications: &[(String, String, Vec<String>)], externals: &[External]) -> Result<Graph> {
        let model_ids: BTreeSet<&str> = models.iter().map(|m| m.id.as_str()).collect();
        let pipeline_ids: BTreeSet<&str> = pipelines.iter().map(|p| p.id.as_str()).collect();
        let job_names: BTreeSet<&str> = jobs.iter().map(|j| j.name.as_str()).collect();
        let mut graph = Graph { tables: BTreeSet::new(), edges: BTreeSet::new() };
        for spec in pipelines {
            if let Some(after) = spec.after.as_deref().filter(|a| pipeline_ids.contains(a)) {
                graph.edges.insert((format!("pipeline:{after}"), format!("pipeline:{}", spec.id), "after"));
            }
            for entry in &spec.tables {
                let table = spec.table_name(&entry.decl().name);
                graph.edges.insert((format!("pipeline:{}", spec.id), format!("table:{table}"), "lands"));
                graph.tables.insert(table);
            }
        }
        for job in jobs {
            let id = format!("job:{}", job.name);
            if let Some(after) = job.after.as_deref().filter(|a| pipeline_ids.contains(a)) {
                graph.edges.insert((format!("pipeline:{after}"), id.clone(), "after"));
            }
            match &job.kind {
                JobKind::StoreDriven(driven) => {
                    for table in &driven.tables {
                        graph.edges.insert((id.clone(), format!("table:{table}"), "writes"));
                        graph.tables.insert(table.clone());
                    }
                }
                JobKind::Synthesize(synthesis) => {
                    if let Some(target) = &job.target {
                        graph.edges.insert((id.clone(), format!("table:{target}"), "writes"));
                        graph.tables.insert(target.clone());
                    }
                    graph.reading(&model_ids, &synthesis.source, &id);
                }
                _ if job.kind_name() == "build" => {
                    if let Some(target) = job.target.as_deref().filter(|t| model_ids.contains(t)) {
                        graph.edges.insert((id.clone(), format!("model:{target}"), "builds"));
                    }
                }
                _ => {}
            }
        }
        // A model's and a store-driven job's statements name the relations they read.
        let mut statements: Vec<(String, ModelSpec)> = models.iter().map(|m| (format!("model:{}", m.id), m.clone())).collect();
        for job in jobs {
            if let JobKind::StoreDriven(driven) = &job.kind {
                let spec: ModelSpec = serde_json::from_value(json!({ "id": format!("job:{}", job.name), "sql": driven.input.statement }))?;
                statements.push((format!("job:{}", job.name), spec));
            }
        }
        for (reader, spec) in statements {
            let reads = contextful_context::build::admit_statements(&spec, |_| None).map_err(|(what, e)| anyhow::anyhow!("{what}: {e}"))?;
            let known: Vec<&String> = reads.iter().filter(|r| model_ids.contains(r.as_str()) || graph.tables.contains(r.as_str())).collect();
            for relation in known {
                graph.reading(&model_ids, relation, &reader);
            }
        }
        for (name, job, _) in publications {
            if job_names.contains(job.as_str()) {
                graph.edges.insert((format!("job:{job}"), format!("publication:{name}"), "publishes"));
            }
        }
        for external in externals {
            let id = format!("external:{}", external.name);
            for input in external.input_refs()? {
                let from = graph.node(&model_ids, &input);
                graph.edges.insert((from, id.clone(), "input"));
            }
            for output in external.output_refs()? {
                let to = graph.node(&model_ids, &output);
                graph.edges.insert((id.clone(), to, "output"));
            }
        }
        Ok(graph)
    }

    /// A `reads` edge from the node holding `relation`: a model by its id, otherwise a table.
    fn reading(&mut self, models: &BTreeSet<&str>, relation: &str, reader: &str) {
        let from = if models.contains(relation) { format!("model:{relation}") } else {
            self.tables.insert(relation.to_string());
            format!("table:{relation}")
        };
        self.edges.insert((from, reader.to_string(), "reads"));
    }

    /// The node a typed reference names; a table a model builds is that model's node.
    fn node(&mut self, models: &BTreeSet<&str>, reference: &Ref) -> String {
        match reference {
            Ref::Table(name) if models.contains(name.as_str()) => format!("model:{name}"),
            Ref::Table(name) => {
                self.tables.insert(name.clone());
                format!("table:{name}")
            }
            Ref::Pipeline(id) => format!("pipeline:{id}"),
            Ref::Job(name) => format!("job:{name}"),
        }
    }
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

/// What a pulled head must fit beyond its signature and its own validity.
#[derive(Clone, Copy, PartialEq)]
enum Fit {
    /// The node arms the head, so every pulled pipeline and job is declared here unchanged
    /// (`surface.reconcile.pulled-control`).
    Declarations,
    /// Import or apply claims onto the head next, so the declarations may drop what it holds
    /// (`surface.apply.adopt-before-claim`).
    Successor,
}

/// A snapshot directory holding no version adopts the store's pulled control chain before
/// import or apply claims one, so a host replacement converges onto the signed head instead
/// of claiming a v1 that diverges from it. The pins are the verb's `--public-key`. True when
/// a head was adopted.
fn adopt_before_claim(snaps: &SnapshotDir, project: &Project, declaration: &Path, tasks: &Tasks, admit: &AdmitArgs) -> Result<bool> {
    if snaps.current()?.is_some() || !project.store_root().join("control").join("head.json").exists() {
        return Ok(false);
    }
    adopt_pulled(snaps, project, declaration, tasks, admit.public_key.as_deref(), Fit::Successor)?;
    Ok(snaps.current()?.is_some())
}

/// The pipelines of a snapshot that validates on its own: its document, build plans, and
/// every job's `after` against the snapshot's pipelines. Apply runs it on what it signs and
/// adoption on what it pulls (`surface.apply.self-valid`).
fn self_valid(text: &str) -> Result<BTreeMap<String, PipelineSpec>> {
    let document: toml::Value = toml::from_str(text)?;
    check_document(&document)?;
    validate_build_jobs(text)?;
    let specs: BTreeMap<String, PipelineSpec> = collect(&[crate::effective::public_manifest(ManifestFile { path: "applied snapshot".into(), text: text.to_string() })?])?
        .into_iter().map(|d| (d.spec.id.clone(), d.spec)).collect();
    job_blocks(text, None, &specs)?;
    snapshot_externals(text, &specs)?;
    Ok(specs)
}

/// The `[[external]]` blocks of every declaration file, in shape
/// (`surface.visualize.external-shape`); a malformed block fails apply validation.
fn declared_externals(declaration: &Path) -> Result<Vec<External>> {
    let mut externals = Vec::new();
    for file in manifests(declaration)?.iter().filter(|f| !f.path.ends_with(".json")) {
        externals.extend(parse_externals(&file.text).map_err(|e| match e {
            SurfaceError::Invalid(why) => SurfaceError::ApplyValidationRefused(format!("{}: {why}", file.path)),
            other => other,
        })?);
    }
    Ok(externals)
}

/// What a snapshot declares that an external step may name: its pipelines, the tables they
/// and its store-driven jobs produce, its standalone tables, its models and its jobs.
fn snapshot_known(text: &str, specs: &BTreeMap<String, PipelineSpec>) -> Result<Known> {
    let jobs = parse_jobs(text, &|_| true)?;
    let document: toml::Value = toml::from_str(text)?;
    let models: Vec<String> = document.get("model").and_then(toml::Value::as_array).into_iter().flatten()
        .filter_map(|m| m.get("id").and_then(toml::Value::as_str).map(str::to_string)).collect();
    let pipelines: Vec<PipelineSpec> = specs.values().cloned().collect();
    let mut tables = contextful_core::job::Targets::new(&pipelines, &jobs, &[]).produced;
    tables.extend(models);
    tables.extend(crate::effective::snapshot_tables(text)?.unwrap_or_default().into_iter().map(|t| t.name));
    Ok(Known { tables, pipelines: specs.keys().cloned().collect(), jobs: jobs.into_iter().map(|j| j.name).collect() })
}

/// Bind a snapshot's external steps against that snapshot's own declarations
/// (`surface.visualize.external-unbound`, `surface.visualize.external-collision`).
fn snapshot_externals(text: &str, specs: &BTreeMap<String, PipelineSpec>) -> Result<Vec<External>> {
    let externals = parse_externals(text)?;
    if !externals.is_empty() {
        bind_externals(&externals, &snapshot_known(text, specs)?)?;
    }
    Ok(externals)
}

/// `job validate`: bind a declaration's external steps against its declared pipelines,
/// produced and standalone tables, models and jobs.
pub(crate) fn validate_externals(text: &str, declaration: &Path, jobs: &[contextful_core::job::Job]) -> Result<()> {
    let externals = parse_externals(text)?;
    if externals.is_empty() {
        return Ok(());
    }
    let files = manifests(declaration)?;
    let specs: Vec<PipelineSpec> = collect(&files)?.into_iter().map(|d| d.spec).collect();
    let models = crate::model_source::collect(&files)?;
    let mut tables = contextful_core::job::Targets::new(&specs, jobs, &models).produced;
    tables.extend(models.into_iter().map(|m| m.id));
    tables.extend(crate::effective::standalone_tables(&files)?.into_iter().map(|t| t.name));
    let known = Known { tables, pipelines: specs.into_iter().map(|s| s.id).collect(), jobs: jobs.iter().map(|j| j.name.clone()).collect() };
    bind_externals(&externals, &known)?;
    Ok(())
}

fn applied_externals(source: &Source, version: Option<u64>) -> Result<Vec<External>> {
    version.map(|v| parse_externals(&source.read(v)?).map_err(Into::into)).transpose().map(Option::unwrap_or_default)
}

/// Adopt only a complete staged chain signed by a locally pinned issuer whose head validates
/// on its own and fits `fit`. The snapshot directory publishes its pointer last.
fn adopt_pulled(snaps: &SnapshotDir, project: &Project, declaration: &Path, tasks: &Tasks, issuer_pin: Option<&str>, fit: Fit) -> Result<()> {
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
    let pulled = self_valid(text).map_err(|e| untrusted(format!("pulled snapshot: {e:#}")))?;
    let local = declared(declaration).map_err(|e| untrusted(format!("local declarations: {e:#}")))?;
    if fit == Fit::Declarations {
        let pulled_jobs = job_blocks(text, None, &pulled).map_err(|e| untrusted(format!("pulled jobs: {e:#}")))?;
        let local_jobs = job_blocks(&std::fs::read_to_string(declaration)?, None, &local)?;
        if pulled_jobs.iter().any(|job| !local_jobs.contains(job)) {
            return Err(untrusted("pulled jobs differ from this node's declarations".into()).into());
        }
        if pulled.values().any(|p| local.get(&p.id).is_none_or(|d| d.content_hash() != p.content_hash())) {
            return Err(untrusted("pulled snapshot differs from this node's pipeline declarations".into()).into());
        }
    }
    // A pipeline the declarations keep unchanged runs here; the next claim checks the rest.
    for spec in pulled.values().filter(|p| local.get(&p.id).is_some_and(|d| d.content_hash() == p.content_hash())) {
        check(spec, declaration, tasks).map_err(|e| untrusted(format!("pipeline `{}`: {e:#}", spec.id)))?;
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
    // A delegate binds in the daemon's own configuration, so apply and import admit any
    // delegate name; arm and fire refuse an unbound one (`surface.delegate.binding`).
    let jobs = parse_jobs(text, &|name| bodies.is_none_or(|b| b.get(name).is_some() || name.starts_with(contextful_core::surface::delegate::BODY_PREFIX)))?;
    contextful_core::job::dependent_jobs(&specs.values().cloned().collect::<Vec<_>>(), &jobs)?;
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
    if !applied_jobs(&control.source, version, &specs)?.is_empty() || !applied_models(&control.source, version)?.is_empty() || !applied_externals(&control.source, version)?.is_empty() {
        return Err(SurfaceError::ApplyValidationRefused("the applied snapshot contains jobs, models or external steps; use the registered host's pipeline apply to edit them".into()).into());
    }
    Ok(())
}

/// The snapshot document: sorted pipeline specifications and validated job blocks.
fn render(specs: &BTreeMap<String, PipelineSpec>, jobs: &[toml::Value], models: &[ModelSpec], tables: Option<&[TableDecl]>, externals: &[External]) -> Result<String> {
    #[derive(Serialize)]
    struct Doc<'a> {
        pipeline: Vec<&'a PipelineSpec>,
        #[serde(skip_serializing_if = "<[toml::Value]>::is_empty")]
        job: &'a [toml::Value],
        #[serde(skip_serializing_if = "<[External]>::is_empty")]
        external: &'a [External],
        #[serde(skip_serializing_if = "<[ModelSpec]>::is_empty")]
        model: &'a [ModelSpec],
        #[serde(skip_serializing_if = "Option::is_none")]
        standalone_tables: Option<&'a [TableDecl]>,
    }
    let body = toml::to_string(&Doc { pipeline: specs.values().collect(), job: jobs, external: externals, model: models, standalone_tables: tables })
        .map_err(|e| SurfaceError::ApplyValidationRefused(format!("a specification holds a value a snapshot cannot carry: {e}")))?;
    // The document carries references alone (`surface.edit.secret-in-document`,
    // `surface.edit.connector-upload`).
    let value: toml::Value = toml::from_str(&body).map_err(|e| SurfaceError::ApplyValidationRefused(format!("the rendered snapshot does not read back: {e}")))?;
    check_document(&value)?;
    Ok(format!("# An applied snapshot, claimed by `contextful pipeline apply`; immutable once claimed.\n\n{body}"))
}

fn validated_draft(document: &str, project: &Project, declaration: &Path, tasks: &Tasks, tables: Option<&[TableDecl]>, preserving: Option<&str>) -> Result<String> {
    let specs = collect(&[ManifestFile { path: "manifest@draft.toml".into(), text: document.to_owned() }])
        .map_err(|error| match error {
            contextful_core::run::RunError::PipelineDuplicateId(_) => SourceRefusal::new("id", "Choose distinct source identities."),
            contextful_core::run::RunError::PipelineTableNameCollision(_) => SourceRefusal::new("tables", "Choose source identities and output tables that expand to distinct destination names."),
            contextful_core::run::RunError::PipelineUnknownDestination(_) => SourceRefusal::new("destination", "Choose the local store destination."),
            _ => SourceRefusal::new("source", "Check source fields, destination declarations and predecessor references against the applied document."),
        })?;
    let specs: BTreeMap<String, PipelineSpec> = specs.into_iter().map(|entry| (entry.spec.id.clone(), entry.spec)).collect();
    let jobs = job_blocks(document, None, &specs)?;
    let models = crate::model_source::applied(&ManifestFile { path: "draft".into(), text: document.into() })?;
    let externals = parse_externals(document)?;
    let baseline = preserving.unwrap_or_default();
    if job_blocks(baseline, None, &specs)? != jobs ||
        crate::model_source::applied(&ManifestFile { path: "applied document".into(), text: baseline.into() })? != models ||
        parse_externals(baseline)? != externals {
        return Err(SurfaceError::ApplyValidationRefused("the source editor preserves jobs, models and external steps; use pipeline apply to change them".into()).into());
    }
    if !crate::effective::snapshot_tables(document)?.unwrap_or_default().is_empty() || !crate::effective::standalone_tables(&[ManifestFile { path: "draft.toml".into(), text: document.into() }])?.is_empty() {
        return Err(SurfaceError::ApplyValidationRefused("the pipeline editor does not edit standalone tables; use pipeline apply".into()).into());
    }
    let rendered = render(&specs, &jobs, &models, tables, &externals)?;
    self_valid(&rendered)?;
    let store = contextful_context::Store::open_declared(&project.dir, &project.name, declaration)?;
    for spec in specs.values() {
        if let Some(schedule) = spec.schedule.as_deref() {
            Schedule::parse(schedule).map_err(|_| SourceRefusal::new("schedule", "Enter a valid schedule, such as every 1h."))?;
        }
        if spec.tables.is_empty() {
            return Err(SourceRefusal::new("tables", "Choose at least one destination table.").into());
        }
        let checked = check(spec, declaration, tasks).map_err(|error| {
            if spec.source.name == "http" && spec.source.config.get("endpoint").and_then(Value::as_str).is_none_or(|endpoint| endpoint.trim().is_empty()) {
                return SourceRefusal::new("source.config.endpoint", "Enter the HTTP endpoint URL.");
            }
            if !contextful_connectors::BUILT_IN.contains(&spec.source.name.as_str()) &&
                contextful_core::connector::component::ComponentSource::parse(&spec.source.name, &spec.source.config).ok().flatten().is_none() {
                return SourceRefusal::new("source.name", "Choose a supported connector name, such as http or file, or a component artifact path or pinned URL.");
            }
            if contextful_connectors::compiled_in(&spec.source.name).is_err() {
                return SourceRefusal::new("source.name", "This server build does not include the connector. Enable its server feature or choose another connector.");
            }
            match error.downcast_ref::<contextful_core::run::RunError>() {
                Some(contextful_core::run::RunError::PipelineUnknownDestination(_)) => SourceRefusal::new("destination", "Choose the local store destination."),
                _ => SourceRefusal::new("source", "Choose an available connector and supply its required configuration fields."),
            }
        })?;
        let connector = if let crate::pipeline::Checked::Component(decl) = &checked {
            let vars = std::env::vars().collect();
            let resolver = contextful_outbound::assemble(&vars, std::sync::Arc::new(crate::clock::SystemClock))?;
            let loaded = crate::component::load(&spec.source.name, decl, &project.dir, &project.name, Some(&resolver), crate::component::ComponentTarget::default(), store.requires_connector_pin())
                .map_err(|_| SourceRefusal::new("source", "Provide an available component artifact with a valid digest and capability manifest."))?;
            let names = loaded.discover(decl).map_err(|_| SourceRefusal::new("source", "Provide a component that exports source discovery."))?;
            if spec.tables.iter().any(|table| !names.iter().any(|name| name == table.name())) {
                return Err(SourceRefusal::new("tables", "Choose destination tables exported by the component's discovery.").into());
            }
            loaded.connector_spec()
        } else { crate::pipeline::built_in(spec) };
        for table in &spec.tables {
            let destination = spec.destination_decl(table);
            if let Some(schema) = store.try_schema(&destination.name)? {
                let declared = contextful_core::store::reconcile::Schema {
                    columns: destination.column_types().into_iter().map(|(name, ty)| contextful_core::store::reconcile::Column::new(name, ty, true)).collect(),
                    ..Default::default()
                };
                schema.merge(&declared, destination.primary_key()).and_then(|merged| destination.validate(&merged))
                    .map_err(|_| SourceRefusal::new("tables", "Choose destination column types, keys and writer rules compatible with the existing table schema."))?;
            }
            let plan = crate::pipeline::plan(spec, table.name(), connector.clone())?;
            let normalize = contextful_core::pipeline::normalize::Normalize::parse(spec.normalize.as_ref())?;
            store.validate_source_plan(&plan, Some(normalize))
                .map_err(|_| SourceRefusal::new("tables", "Choose destination schemas and writer rules compatible with the existing store."))?;
        }
    }
    Ok(rendered)
}

#[derive(Debug, thiserror::Error)]
#[error("{message}")]
pub(crate) struct SourceRefusal {
    pub field: &'static str,
    pub message: &'static str,
}

impl SourceRefusal {
    fn new(field: &'static str, message: &'static str) -> Self { Self { field, message } }
}

/// Add one source to the applied document and save it through the versioned editor.
pub(crate) fn edit_source(project: &ProjectArgs, declaration: Option<PathBuf>, expected: u64, source: Value, operator: &str, tasks: &Tasks, boundary: &dyn Fn() -> Result<(), ControlError>) -> Result<Value> {
    let (located, _, control) = located(project, declaration.clone())?;
    let (version, specs) = applied(&control.source)?;
    if version != Some(expected) {
        return Err(SurfaceError::ManifestVersionConflict("Read the applied version again before saving a source.".into()).into());
    }
    let spec: PipelineSpec = serde_json::from_value(source)
        .map_err(|_| SourceRefusal::new("source", "Supply a source identity, connector configuration and destination tables."))?;
    if spec.id.trim().is_empty() || specs.contains_key(&spec.id) {
        return Err(SourceRefusal::new("id", "Choose a nonempty source identity that the applied document does not contain.").into());
    }
    if spec.tables.is_empty() {
        return Err(SourceRefusal::new("tables", "Choose at least one destination table.").into());
    }
    let snapshot = control.source.read(expected)?;
    let public = crate::effective::public_manifest(ManifestFile { path: "applied document".into(), text: snapshot.clone() })?;
    let mut document: toml::Value = toml::from_str(&public.text)?;
    let pipelines = document.as_table_mut().context("applied document is a table")?.entry("pipeline").or_insert_with(|| toml::Value::Array(Vec::new()));
    let source = toml::Value::try_from(spec).map_err(|_| SourceRefusal::new("source.config", "Use TOML-compatible connector options: strings, numbers, booleans, lists or objects; remove null values."))?;
    pipelines.as_array_mut().context("applied pipelines are an array")?.push(source);
    let tables = applied_tables(&control.source, Some(expected))?;
    let document = validated_draft(&toml::to_string(&document)?, &located.project, &located.declaration, tasks, tables.as_deref(), Some(&snapshot))?;
    let draft = Draft::new(expected, document, operator.to_owned())?;
    owner(&control)?.save_draft_guarded(&draft, boundary)?;
    let public = crate::effective::public_manifest(ManifestFile { path: "source draft".into(), text: draft.document })?;
    Ok(json!({ "expected": expected, "nonce": draft.nonce, "document": public.text }))
}

/// Store one validated, version-bound control draft without changing the applied pointer.
pub(crate) fn edit(project: &ProjectArgs, declaration: Option<PathBuf>, expected: u64, document: &str, operator: &str, tasks: &Tasks, boundary: &dyn Fn() -> Result<(), ControlError>) -> Result<Value> {
    let (located, _, control) = located(project, declaration)?;
    refuse_job_edit(&control)?;
    let snapshots = owner(&control)?;
    snapshots.initialized()?;
    if control.source.current()? != Some(expected) {
        return Err(SurfaceError::ManifestVersionConflict(format!("the editor read v{expected}, which is no longer current")).into());
    }
    let tables = applied_tables(&control.source, Some(expected))?;
    let text = validated_draft(document, &located.project, &located.declaration, tasks, tables.as_deref(), None)?;
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
    let snapshots = owner(&control)?;
    snapshots.initialized()?;
    let draft = snapshots.read_draft()?;
    if draft.expected != expected || draft.nonce != nonce || draft.operator != operator {
        return Err(SurfaceError::ManifestVersionConflict(format!("the draft read v{} and apply named v{expected}", draft.expected)).into());
    }
    let tables = applied_tables(&control.source, Some(expected))?;
    let public = crate::effective::public_manifest(ManifestFile { path: "saved draft".into(), text: draft.document.clone() })?;
    if validated_draft(&public.text, &initial.project, &initial.declaration, tasks, tables.as_deref(), Some(&control.source.read(expected)?))? != draft.document {
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
    let model_changes = model_diff(&crate::model_source::collect(&manifests(&l.declaration)?)?, &applied_models(&control.source, version)?);
    let jobs = job_diff(&job_blocks(&declaration_text, None, &declared)?, &applied_jobs(&control.source, version, &applied)?);
    let tables = table_diff(&crate::effective::standalone_tables(&manifests(&l.declaration)?)?, &applied_tables(&control.source, version)?.unwrap_or_default());
    let externals = external_diff(&declared_externals(&l.declaration)?, &applied_externals(&control.source, version)?);
    if as_json {
        println!("{}", serde_json::to_string_pretty(&json!({ "applied": version, "pipelines": changes, "jobs": jobs, "models": model_changes, "tables": tables, "externals": externals }))?);
        return Ok(());
    }
    println!("applied: {}", version.map(|v| format!("v{v}")).unwrap_or_else(|| "none".into()));
    for c in changes.iter().chain(&jobs).chain(&model_changes).chain(&tables).chain(&externals) {
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
    if adopt_before_claim(snapshots, &l.project, &l.declaration, tasks, admit)? {
        println!("adopted the pulled control head; import claims nothing");
        return Ok(());
    }
    // A restart on a warm disk imports again; the version standing here answers it.
    if let Some(v) = snapshots.current()? {
        println!("v{v} stands; import claims nothing, and `contextful pipeline apply` edits it");
        return Ok(());
    }
    let attestation = SyncAttestation::for_project(&l, project, admit, issuer_key)?;
    let declared = declared(&l.declaration)?;
    let jobs = job_blocks(&declaration_text, Some(bodies), &declared)?;
    let models = crate::model_source::collect(&manifests(&l.declaration)?)?;
    let tables = crate::effective::standalone_tables(&manifests(&l.declaration)?)?;
    let externals = declared_externals(&l.declaration)?;
    let text = render(&declared, &jobs, &models, Some(&tables), &externals)?;
    snapshot_externals(&text, &declared)?;
    self_valid(&text).map_err(|e| SurfaceError::ApplyValidationRefused(format!("combined snapshot: {e:#}")))?;
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
    adopt_before_claim(snapshots, &l.project, &l.declaration, tasks, admit)?;
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
        let prior_models = applied_models(source, version)?;
        let models = if id.is_some() { prior_models.clone() } else { crate::model_source::collect(&manifests(&l.declaration)?)? };
        let prior_tables = applied_tables(source, version)?;
        let tables = if id.is_some() { prior_tables.clone() } else { Some(crate::effective::standalone_tables(&manifests(&l.declaration)?)?) };
        let prior_externals = applied_externals(source, version)?;
        let externals = if id.is_some() { prior_externals.clone() } else { declared_externals(&l.declaration)? };
        let text = render(&target, &jobs, &models, tables.as_deref(), &externals)?;
        job_blocks(&text, Some(bodies), &target)?;
        snapshot_externals(&text, &target)?;
        self_valid(&text).map_err(|e| SurfaceError::ApplyValidationRefused(format!("combined snapshot: {e:#}")))?;
        for c in changes.iter().filter(|c| c.action != "remove") {
            let spec = &target[&c.id];
            check(spec, &l.declaration, tasks)
                .map_err(|e| SurfaceError::ApplyValidationRefused(format!("pipeline `{}`: {e:#}", spec.id)))?;
        }
        if changes.is_empty() && jobs == prior_jobs && models == prior_models && tables == prior_tables && externals == prior_externals {
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
                for c in table_diff(tables.as_deref().unwrap_or_default(), prior_tables.as_deref().unwrap_or_default()).iter().filter(|c| c.action != "unchanged") {
                    println!("{} {}", sigil(c.action), c.id);
                }
                println!("applied v{v}");
                // The claim stands whatever retirement meets; the fire's refusal still names the verb.
                if let Err(e) = crate::job::retire_superseded(&l, project, &declaration_text, v, bodies) {
                    eprintln!("warning: v{v} is applied, and retiring its superseded executions failed: {e:#}");
                }
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
    /// The seed each store-driven or synthesize fire's credential is minted under, and the root holding
    /// the issuance policy (`surface.dispatch.fire-credential`).
    issuer: Option<(PathBuf, PathBuf)>,
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
        let selected = (|| -> Result<Option<(Job, Located)>> {
            let located = crate::project::locate(Some(&self.project), self.declaration.clone())?;
            let manifest = std::fs::read_to_string(&located.declaration)?;
            let snapshot = snapshot_manifest(&located.project, &manifest, version)?;
            let jobs = parse_jobs(&snapshot.text, &|_| true)?;
            Ok(jobs.into_iter().find(|job| id == format!("job:{}", job.name)).map(|job| (job, located)))
        })().map_err(|e| ChildStepError::Infrastructure(format!("{id}: reading applied dispatch: {e:#}")))?;
        match selected {
            Some((job, located)) => {
                cmd.args(["job", "fire", &job.name]);
                if let Some((seed, root)) = self.issuer.as_ref().filter(|_| admits_credential(&job.kind)) {
                    let credential = fire_credential(&located, root, seed, &job).map_err(|e| ChildStepError::Infrastructure(format!("{id}: minting the fire credential: {e:#}")))?;
                    // The child admits under the pin and audience it was minted with (`surface.dispatch.fire-credential-pins`).
                    cmd.env(crate::admit::TOKEN_VAR, credential.token)
                        .env(crate::admit::PUBKEY_VAR, credential.public_key)
                        .env(crate::admit::AUDIENCE_VAR, credential.audience);
                }
            }
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

/// The lifetime of a fire credential, clamped to the issuance ceiling
/// (`surface.dispatch.fire-credential-lifetime`).
const FIRE_CREDENTIAL_SECS: u64 = 600;

/// Whether a fire of this kind admits a capability credential (`surface.dispatch.fire-credential`).
fn admits_credential(kind: &JobKind) -> bool {
    matches!(kind, JobKind::StoreDriven(_) | JobKind::Synthesize(_))
}

/// The tables one fire of `job` reads and writes (`surface.dispatch.fire-reach`): a
/// store-driven job's statement relations and `tables`, a synthesize job's declared reach.
fn fire_reach(located: &Located, job: &Job) -> Result<Reach> {
    let face = crate::admit::face(located)?;
    match &job.kind {
        JobKind::StoreDriven(driven) => {
            let tree = face.serialize(&driven.input.statement)?;
            let read = contextful_core::read::guard::admit(&tree, |_| true)?.relations.into_iter().collect();
            Ok(Reach { read, write: driven.tables.clone() })
        }
        _ => job.reach(face.memory()).with_context(|| format!("job `{}` is kind `{}`, which admits no fire credential", job.name, job.kind_name())),
    }
}

/// One credential for a fire of `job`, on behalf of `svc://job/<job>`: `read` and `write`
/// over its reach (`surface.dispatch.fire-credential`).
fn fire_credential(located: &Located, root: &Path, seed: &Path, job: &Job) -> Result<crate::token::ServiceCredential> {
    let reach = fire_reach(located, job)?;
    let grant = |action: Action, tables: &[String]| -> Result<Grant> {
        Ok(Grant {
            horizon: None,
            actions: vec![action],
            tables: tables.iter().map(|t| TablePattern::parse(t)).collect::<Result<_, _>>()?,
            tenant: None,
            aggregate: None,
            templates: None,
            max_rows: None,
            max_duration_ms: None,
            max_response_bytes: None,
        })
    };
    let mut grants = Vec::new();
    if !reach.read.is_empty() {
        grants.push(grant(Action::Read, &reach.read)?);
    }
    grants.push(grant(Action::Write, &reach.write)?);
    crate::token::mint_service(root, seed, &format!("svc://job/{}", job.name), grants, FIRE_CREDENTIAL_SECS)
}

/// Refuse a serve whose applied snapshot schedules a store-driven or synthesize job while it
/// holds neither an issuer seed nor an inherited credential
/// (`surface.dispatch.fire-credential-missing`), or holds a seed under a root declaring no
/// default zone (`surface.dispatch.fire-credential-zone`).
fn require_fire_credential(control: &ControlConfig, issuer: Option<&Path>) -> Result<()> {
    if issuer.is_none() && AdmitArgs::presented() {
        return Ok(());
    }
    let Some(version) = control.source.current()? else { return Ok(()) };
    let text = control.source.read(version)?;
    // A job the snapshot cannot register meets its own refusal when the snapshot arms.
    let Ok(jobs) = parse_jobs(&text, &|name| {
        control.bodies.contains(name) || name.strip_prefix(contextful_core::surface::delegate::BODY_PREFIX).is_some_and(|d| control.delegates.contains_key(d))
    }) else {
        return Ok(());
    };
    let scheduled: Vec<String> = jobs.iter().filter(|j| j.schedule.is_some() && admits_credential(&j.kind)).map(|j| format!("`{}`", j.name)).collect();
    if scheduled.is_empty() {
        return Ok(());
    }
    let Some(root) = issuer else {
        bail!(
            "FireCredentialMissing: applied v{version} schedules job(s) {}, and serve holds neither `--issuer-key` nor {}; each fire admits one credential",
            scheduled.join(", "),
            crate::admit::TOKEN_VAR
        )
    };
    if crate::token::default_zone(root)?.is_none() {
        bail!(
            "FireCredentialZoneUnset: applied v{version} schedules job(s) {} under `--issuer-key`, and `[project.zone]` declares no `default`; a fire credential signing no zone reads no row",
            scheduled.join(", ")
        )
    }
    Ok(())
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
        adopt_pulled(directory, project, declaration, tasks, control.issuer_pin.as_deref(), Fit::Declarations)?;
    }
    let (version, specs) = applied(&control.source)?;
    let Some(version) = version else { return Ok(None) };
    if version == scheduler.version() {
        return Ok(Some(Vec::new()));
    }
    let job_text = control.source.read(version)?;
    validate_build_jobs(&job_text).context("applied build plans are incomplete; reapply with a declared model and resolved SQL")?;
    let jobs = parse_jobs(&job_text, &|name| {
        control.bodies.contains(name) || name.strip_prefix(contextful_core::surface::delegate::BODY_PREFIX).is_some_and(|d| control.delegates.contains_key(d))
    })?;
    let runs = contextful_core::job::dependent_jobs(&specs.values().cloned().collect::<Vec<_>>(), &jobs)?;
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
                entries.push(Entry { id: spec.id.clone(), schedule, priority: spec.priority() });
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
    job_blocks(&job_text, None, &specs)?;
    let mut scheduled_jobs = 0;
    for job in jobs {
        let id = format!("job:{}", job.name);
        if job.after.is_some() {
            if control.relay.is_some() && !control.workers.is_empty() { bail!("dependent build jobs require local dispatch"); }
            continue;
        }
        let reason = match (&job.kind, job.schedule.as_deref()) {
            (JobKind::StoreDriven(_) | JobKind::Synthesize(_), Some(text)) | (JobKind::Maintenance(_), Some(text))
                if job.kind_name() == "build" || matches!(job.kind, JobKind::StoreDriven(_) | JobKind::Synthesize(_)) =>
            {
                if control.relay.is_some() && !control.workers.is_empty() {
                    bail!("scheduled job `{}` requires the local registered host; worker dispatch handles pipelines", job.name);
                }
                entries.push(Entry { id, schedule: Schedule::parse(text)?, priority: 0 });
                scheduled_jobs += 1;
                continue;
            }
            (_, None) => "it declares no schedule".to_string(),
            (JobKind::Plan(_), Some(_)) => format!("its plan kind fires through `contextful job fire {}`", job.name),
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
pub(crate) fn serve(project: &ProjectArgs, declaration: Option<PathBuf>, cycle: bool, http: Option<&str>, public_key: Option<&str>, issuer_key: Option<&Path>, tasks: &Tasks, bodies: &Bodies) -> Result<()> {
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
        _ => {
            let root = crate::root::root(project.project.as_deref())?;
            require_fire_credential(&control, issuer_key.map(|_| root.as_path()))?;
            Arc::new(ChildDispatch {
                exe: std::env::current_exe()?,
                project: l.project.name.clone(),
                declaration: explicit,
                now: project.now.clone(),
                children: children.clone(),
                issuer: issuer_key.map(|seed| (seed.to_path_buf(), root)),
            })
        }
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
        if contextful_engine::stop::requested() {
            crate::sync::drain(&l)?;
        }
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
    // The stop push carries what the ended children committed (`store.drain.deadline`).
    crate::sync::drain(&l)?;
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
    // The stop push carries what the ended children committed (`store.drain.deadline`).
    let located = Located { project: project.clone(), declaration: declaration.to_path_buf() };
    crate::sync::drain(&located)?;
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
/// and wait for the units it dispatched until `deadline`, each freed slot admitting a unit the
/// evaluation left pending. A unit still running or still pending at the deadline reports
/// pending; a failed reconcile leaves the armed set running and reports its
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
    let refill = scheduler.refill().clone();
    for id in &refill.started {
        eprintln!("fire {id}: started");
    }
    if let Some(reason) = refill.stopped {
        let line = format!("refill stopped: {reason}; the waiting units report pending");
        eprintln!("{line}");
        diagnostics.push(line);
    }
    report(ended.clone());
    let mut fired: Vec<&str> = ended.iter().filter(|f| f.result.is_ok()).map(|f| f.id.as_str()).collect();
    let mut failed: Vec<&str> = ended.iter().filter(|f| f.result.is_err()).map(|f| f.id.as_str()).chain(beat.refused.iter().map(|(id, _)| id.as_str())).collect();
    fired.sort();
    failed.sort();
    let in_flight = scheduler.in_flight();
    // A refilled slot started some of what the beat left pending (`surface.dispatch.refill`).
    let waiting = scheduler.waiting();
    let mut pending: Vec<&str> = waiting.iter().map(String::as_str).chain(in_flight.iter().map(String::as_str)).collect();
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


fn applied_models(source: &Source, version: Option<u64>) -> Result<Vec<ModelSpec>> {
    version.map(|v| crate::model_source::applied(&ManifestFile { path: snapshot_file(v), text: source.read(v)? })).transpose().map(Option::unwrap_or_default)
}

fn applied_tables(source: &Source, version: Option<u64>) -> Result<Option<Vec<TableDecl>>> {
    match version {
        Some(version) => crate::effective::snapshot_tables(&source.read(version)?),
        None => Ok(None),
    }
}

fn table_diff(declared: &[TableDecl], applied: &[TableDecl]) -> Vec<Change> {
    let identities = |tables: &[TableDecl]| -> BTreeMap<String, (String, Option<String>)> {
        tables.iter().map(|t| (format!("table:{}", t.name), (contextful_core::run::journal::sha256_hex(t.canonical().as_bytes()), None))).collect()
    };
    let (d, a) = (identities(declared), identities(applied));
    let ids: BTreeSet<_> = d.keys().chain(a.keys()).collect();
    ids.into_iter().map(|id| change(id, d.get(id).cloned(), a.get(id).cloned())).collect()
}

fn model_diff(declared: &[ModelSpec], applied: &[ModelSpec]) -> Vec<Change> {
    let identities = |models: &[ModelSpec]| -> BTreeMap<String, (String, Option<String>)> {
        models.iter().map(|m| (format!("model:{}", m.id), (contextful_core::run::journal::sha256_hex(&serde_json::to_vec(m).expect("model serializes")), None))).collect()
    };
    let (d, a) = (identities(declared), identities(applied));
    let ids: BTreeSet<_> = d.keys().chain(a.keys()).collect();
    ids.into_iter().map(|id| change(id, d.get(id).cloned(), a.get(id).cloned())).collect()
}

fn external_diff(declared: &[External], applied: &[External]) -> Vec<Change> {
    let identities = |externals: &[External]| -> BTreeMap<String, (String, Option<String>)> {
        externals.iter().map(|e| (format!("external:{}", e.name), (contextful_core::run::journal::sha256_hex(&serde_json::to_vec(e).expect("external serializes")), None))).collect()
    };
    let (d, a) = (identities(declared), identities(applied));
    let ids: BTreeSet<_> = d.keys().chain(a.keys()).collect();
    ids.into_iter().map(|id| change(id, d.get(id).cloned(), a.get(id).cloned())).collect()
}

fn validate_build_jobs(text: &str) -> Result<()> {
    crate::effective::validate_snapshot_tables(text)?;
    let file = ManifestFile { path: "applied snapshot".into(), text: text.into() };
    let models = crate::model_source::applied(&file)?;
    let specs: Vec<_> = collect(&[crate::effective::public_manifest(file)?])?.into_iter().map(|p| p.spec).collect();
    let jobs = parse_jobs(text, &|_| true)?;
    crate::job::bind_declared(&jobs, &specs, &models)?;
    Ok(())
}
