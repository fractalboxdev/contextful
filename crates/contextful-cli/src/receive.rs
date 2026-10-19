//! `contextful pipeline receive` and the served `POST /receive/:pipeline` route: one
//! delivered batch of a `receive` pipeline lands through the pull land path, one run per
//! touched table in declaration order, and the answer follows the last commit
//! (`run.land.receive-entry`, `run.land.receive-ack`, `run.land.receive-endpoint`).

use crate::admit::{AdmitArgs, Author};
use crate::pipeline::{built_in, check, manifests, plan, Checked};
use crate::project::Located;
use crate::run::{boot_id, wire_at, ProjectArgs, StoreDestination, Wired};
use anyhow::{bail, Context, Result};
use contextful_connectors::receive::{decode, Delivered, ReceiveConfig, RETRY_AFTER_MS};
use contextful_context::{node, Store};
use contextful_core::pipeline::declare::collect;
use contextful_core::pipeline::declare::PipelineSpec;
use contextful_core::pipeline::transform::Chain;
use contextful_core::run::derive::task::Tasks;
use contextful_core::run::journal::sha256_hex;
use contextful_core::run::own::receive_digest;
use contextful_core::run::record::{RunRow, RunStatus};
use contextful_core::run::{Failure, FailureTag, RunError};
use contextful_core::store::StoreError;
use contextful_engine::{EngineError, RunSpec};
use contextful_policy::verify::AdmittedAuthority;
use serde_json::{json, Value};
use std::fs::File;
use std::io::Read;
use std::path::{Path, PathBuf};

/// The longest a delivered batch's pull or push waits for the push guard, in seconds
/// (`run.land.receive-push-wait`).
pub(crate) const PUSH_WAIT_SECS: u64 = 30;

/// The marker a batch whose push met a held push guard leaves in its pipeline's queue
/// directory; the next batch pushes while it stands (`run.land.receive-push-wait`).
const PUSH_PENDING: &str = "push.pending";

/// The arguments of one receive on the command line.
pub(crate) struct Receive<'a> {
    pub id: String,
    pub key: String,
    pub project: ProjectArgs,
    pub declaration: Option<PathBuf>,
    pub site_id: Option<String>,
    pub site_id_env: Option<String>,
    pub admit: &'a AdmitArgs,
}

/// A refusal answered without landing a table.
pub(crate) struct Refusal {
    pub error: RunError,
    pub retryable: bool,
}

impl Refusal {
    fn final_(error: RunError) -> Refusal {
        Refusal { error, retryable: false }
    }

    /// The retryable refusal of a pull or push whose push guard stayed held past
    /// [`PUSH_WAIT_SECS`] (`run.land.receive-push-wait`); `None` for any other failure.
    fn guard_held(e: &anyhow::Error) -> Option<Refusal> {
        let held = e.chain().find_map(|c| match c.downcast_ref::<contextful_sync::SyncError>() {
            Some(contextful_sync::SyncError::Store(StoreError::SyncPushInFlight(m))) => Some(m.clone()),
            _ => None,
        })?;
        Some(Refusal { error: RunError::Store(StoreError::SyncPushInFlight(format!("{held}; resend the batch under its key"))), retryable: true })
    }

    /// The served status of the refusal (`run.land.receive-endpoint-status`).
    fn status(&self) -> u16 {
        match self.error {
            RunError::ReceiveWriteNotGranted(_) => 403,
            RunError::ReceiveBatchTooLarge(_) => 413,
            RunError::ReceiveBatchKeyExpired(_) => 410,
            RunError::ReceiveBackpressure(_) => 503,
            _ if self.retryable => 503,
            _ => 422,
        }
    }
}

/// A batch whose checks before the body passed: its held queue slot and the wired engine.
pub(crate) struct Admitted {
    slot: u32,
    _held: File,
    wired: Wired,
}

/// A batch that reached the land path: the answer and the table that failed, if one did,
/// with whether its class retries.
pub(crate) struct Landed {
    pub answer: Value,
    pub failed: Option<(String, bool)>,
}

/// One `receive` pipeline as declared, resolved for its batches.
pub(crate) struct Receiver {
    l: Located,
    text: String,
    site_id: String,
    spec: PipelineSpec,
    config: ReceiveConfig,
    now: Option<String>,
}

/// Write `line` to standard error; a served process whose standard error has closed keeps
/// answering.
fn note(line: &dyn std::fmt::Display) {
    use std::io::Write;
    let _ = writeln!(std::io::stderr(), "{line}");
}

/// Print `answer` as the one stdout line a sender reads.
fn say(answer: &Value) {
    println!("{answer}");
}

/// Take one of `depth` queue slots under `dir`, or `None` while every slot is held
/// (`run.land.receive-queue`). The slot is released when the returned file closes.
fn take_slot(dir: &Path, depth: u32) -> Result<Option<(u32, File)>> {
    std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    for n in 0..depth {
        let path = dir.join(format!("slot-{n}.lock"));
        let file = std::fs::OpenOptions::new().read(true).write(true).create(true).truncate(false).open(&path).with_context(|| format!("opening {}", path.display()))?;
        match file.try_lock() {
            Ok(()) => return Ok(Some((n, file))),
            Err(std::fs::TryLockError::WouldBlock) => continue,
            Err(std::fs::TryLockError::Error(e)) => return Err(e).with_context(|| format!("locking {}", path.display())),
        }
    }
    Ok(None)
}

/// Whether a failed table's class retries (`run.retry.retryable-classes`): `Transient` and
/// `RateLimited` do; a refusal before the run, a schema mismatch or any other class does not.
fn retries(tag: Option<FailureTag>) -> bool {
    matches!(tag, Some(FailureTag::Transient | FailureTag::RateLimited))
}

/// The class an error raised around a table's run carries, when it carries one.
fn class(e: &anyhow::Error) -> Option<FailureTag> {
    match e.downcast_ref::<EngineError>() {
        Some(EngineError::Failure(f)) if !f.deterministic => Some(f.tag),
        Some(_) => None,
        None => e.downcast_ref::<Failure>().filter(|f| !f.deterministic).map(|f| f.tag),
    }
}

/// The run of `stem` that committed, or the id the next attempt takes: attempts are
/// `stem`, `stem.2`, `stem.3`, … since a run id names one attempt.
fn attempt(catalog: &dyn contextful_core::coordinate::Catalog, stem: &str) -> Result<std::result::Result<RunRow, String>> {
    for n in 1u32.. {
        let id = if n == 1 { stem.to_string() } else { format!("{stem}.{n}") };
        match catalog.run(&id)? {
            Some(row) if row.status == RunStatus::Success => return Ok(Ok(row)),
            Some(_) => continue,
            None => return Ok(Err(id)),
        }
    }
    unreachable!("the attempt numbers are unbounded")
}

impl Receiver {
    /// Resolve the `receive` pipeline `id` of the project's declaration.
    pub(crate) fn open(project: &ProjectArgs, declaration: Option<PathBuf>, id: &str, site_id: Option<String>, site_id_env: Option<String>, tasks: &Tasks) -> Result<Receiver> {
        let l = project.locate(declaration)?;
        let declaration = l.declaration.clone();
        let text = if declaration.exists() { std::fs::read_to_string(&declaration)? } else { String::new() };
        let site_id = crate::run::site_id_for(&text, &declaration, site_id, site_id_env)?;
        let files = manifests(&declaration)?;
        crate::secrets::check_records(&declaration, &files)?;
        let Some(spec) = collect(&files)?.into_iter().find(|d| d.spec.id == id).map(|d| d.spec) else {
            return Err(RunError::ReceivePipelineUnknown(format!("no pipeline `{id}` is declared")).into());
        };
        if spec.source.name != contextful_connectors::receive::NAME {
            return Err(RunError::ReceivePipelineUnknown(format!(
                "pipeline `{}` reads source `{}`; `pipeline receive` lands the delivered batches of a `receive` source",
                spec.id, spec.source.name
            ))
            .into());
        }
        let Checked::Receive(config) = check(&spec, &declaration, tasks)? else {
            bail!("pipeline `{}` reads source `{}`; `pipeline receive` lands the delivered batches of a `receive` source", spec.id, spec.source.name);
        };
        Ok(Receiver { l, text, site_id, spec, config, now: project.now.clone() })
    }

    /// Every store table the pipeline lands in, in declaration order.
    fn destinations(&self) -> Vec<String> {
        self.spec.tables.iter().map(|t| self.spec.table_name(t.name())).collect()
    }

    /// The batch run id: the stem of every touched table's run id (`run.land.receive-batch-key`).
    fn base(&self, key: &str) -> String {
        format!("receive-{}", receive_digest(&self.spec.id, key))
    }

    /// The answer of a refused batch.
    pub(crate) fn refused(&self, key: &str, refusal: &Refusal) -> Value {
        let mut answer = json!({ "pipeline": self.spec.id, "key": key, "run_id": self.base(key), "committed": false, "retryable": refusal.retryable, "error": refusal.error.to_string() });
        if refusal.retryable {
            answer["retry_after_ms"] = json!(RETRY_AFTER_MS);
        }
        answer
    }

    /// The credential holds `write` over every table of the pipeline
    /// (`run.land.receive-write-not-granted`).
    pub(crate) fn ungranted(&self, authority: &AdmittedAuthority) -> Option<Refusal> {
        let table = self.destinations().into_iter().find(|t| authority.require_write(t).is_err())?;
        Some(Refusal::final_(RunError::ReceiveWriteNotGranted(format!(
            "the credential holds no `write` grant over `{table}`, a table of pipeline `{}`; every table of a `receive` pipeline needs one",
            self.spec.id
        ))))
    }

    /// The checks before the body is read: a queue slot (`run.land.receive-backpressure`)
    /// and the key's retention window (`run.land.receive-key-expired`).
    pub(crate) fn admit(&self, key: &str) -> Result<std::result::Result<Admitted, Refusal>> {
        let ReceiveConfig { max_queue_depth, key_retention_secs, .. } = self.config;
        let Some((slot, held)) = take_slot(&self.queue(), max_queue_depth)? else {
            return Ok(Err(Refusal {
                error: RunError::ReceiveBackpressure(format!(
                    "pipeline `{}` holds all {max_queue_depth} queue slots; resend batch `{key}` after {RETRY_AFTER_MS} ms",
                    self.spec.id
                )),
                retryable: true,
            }));
        };
        let wired = wire_at(&self.l.project, &self.now)?;
        let base = self.base(key);
        let mut first: Option<i64> = None;
        for table in self.destinations() {
            if let Some(row) = wired.engine.catalog.run(&format!("{base}.{table}"))? {
                let started = row.started_at.unix_secs();
                first = Some(first.map_or(started, |f| f.min(started)));
            }
        }
        if let Some(first) = first {
            let age = wired.clock.now().unix_secs() - first;
            if age > key_retention_secs as i64 {
                return Ok(Err(Refusal::final_(RunError::ReceiveBatchKeyExpired(format!(
                    "batch key `{key}` of pipeline `{}` first landed {age} s ago, past `key_retention_secs = {key_retention_secs}`; send the rows under a new key",
                    self.spec.id
                )))));
            }
        }
        Ok(Ok(Admitted { slot, _held: held, wired }))
    }

    /// The directory holding the pipeline's queue slots and land lock.
    fn queue(&self) -> PathBuf {
        self.l.project.run_dir().join("receive").join(&self.spec.id)
    }

    /// The refusal of a body past `max_batch_bytes` (`run.land.receive-batch-too-large`).
    fn too_large(&self, key: &str) -> Refusal {
        Refusal::final_(RunError::ReceiveBatchTooLarge(format!("batch `{key}` of pipeline `{}` passes `max_batch_bytes = {}`", self.spec.id, self.config.max_batch_bytes)))
    }

    /// Land one delivered `body` under `key`, one run per touched table in declaration order,
    /// waiting for the pipeline's land lock (`run.land.receive-queue`).
    pub(crate) fn land(&self, admitted: Admitted, key: &str, body: &[u8], author: Option<Author>) -> Result<std::result::Result<Landed, Refusal>> {
        let spec = &self.spec;
        let names: Vec<&str> = spec.tables.iter().map(|t| t.name()).collect();
        let mut split = match decode(body, &names) {
            Ok(split) => split,
            Err(e) => return Ok(Err(Refusal::final_(e))),
        };
        let w = admitted.wired;
        let land = File::options().read(true).write(true).create(true).truncate(false).open(self.queue().join("land.lock"))?;
        land.lock()?;

        let declaration = &self.l.declaration;
        let store = Store::open_declared(&self.l.project.dir, &self.l.project.name, declaration)?;
        let (node, _) = node::resolve(&store, |k| std::env::var(k).ok())?;
        let decls = spec.tables.iter().map(|t| spec.destination_decl(t)).collect();
        let normalize = Some(contextful_core::pipeline::normalize::Normalize::parse(spec.normalize.as_ref())?);
        let mut dest = StoreDestination { store, decls, node, author, normalize, relational_parts: Default::default(), schema_diffs: Vec::new() };
        let artifact = sha256_hex(serde_json::to_string(&spec.source).unwrap_or_default().as_bytes());
        let base = self.base(key);
        let mut tables = Vec::new();
        let mut failed: Option<(String, bool)> = None;
        let mut opened = false;
        for t in &spec.tables {
            let Some(rows) = split.remove(t.name()) else { continue };
            let table = spec.table_name(t.name());
            let next = match attempt(w.engine.catalog.as_ref(), &format!("{base}.{table}"))? {
                Ok(row) => {
                    tables.push(json!({ "table": table, "run_id": row.run_id, "outcome": "replayed", "rows": row.rows }));
                    continue;
                }
                Err(next) => next,
            };
            let trace = crate::telemetry::RunTrace::open();
            let outcome = (|| -> Result<RunRow> {
                let plan = plan(spec, t.name(), built_in(spec))?;
                dest.store.validate_source_plan(&plan, dest.normalize)?;
                let connector = plan.connector_pin(&artifact);
                let run = RunSpec { plan, connector, run_id: next.clone(), site_id: self.site_id.clone(), pid: std::process::id(), boot_id: boot_id(), trace_id: Some(trace.trace_id()) };
                let shape = Chain { ops: spec.transforms.clone(), table: table.clone() };
                Ok(w.engine.run_receive(&run, key, &mut Delivered(rows), &shape, &mut dest)?)
            })();
            opened |= w.engine.catalog.run(&next).ok().flatten().is_some();
            if let Ok(row) = &outcome {
                trace.close(row);
            }
            match outcome {
                Ok(row) if row.status == RunStatus::Success => {
                    tables.push(json!({ "table": table, "run_id": row.run_id, "outcome": "committed", "rows": row.rows }));
                }
                Ok(row) => {
                    let why = format!("{}: {}", row.error_kind.map(|k| k.name()).unwrap_or("unknown"), row.error_message.unwrap_or_default());
                    note(&RunError::PipelineTableFailed(format!("table `{table}` failed in run `{}`: {why}", row.run_id)));
                    tables.push(json!({ "table": table, "run_id": row.run_id, "outcome": "failed" }));
                    failed = Some((table, retries(row.error_kind)));
                    break;
                }
                Err(e) => {
                    note(&RunError::PipelineTableFailed(format!("table `{table}` failed as refused in run `{next}`: {e:#}")));
                    tables.push(json!({ "table": table, "run_id": next, "outcome": "failed" }));
                    failed = Some((table, retries(class(&e))));
                    break;
                }
            }
        }
        let retryable = failed.as_ref().is_some_and(|(_, r)| *r);
        let answer = json!({ "pipeline": spec.id, "key": key, "run_id": base, "committed": failed.is_none(), "retryable": retryable, "tables": tables });
        // A batch whose push met a held guard left the marker; the next batch pushes what it committed.
        let pending = self.queue().join(PUSH_PENDING);
        if opened || pending.exists() {
            match crate::sync::push_after_run_within(&self.l, w.clock.now(), Some(PUSH_WAIT_SECS)) {
                Ok(()) => match std::fs::remove_file(&pending) {
                    Err(e) if e.kind() != std::io::ErrorKind::NotFound => return Err(e).with_context(|| format!("removing {}", pending.display())),
                    _ => {}
                },
                Err(e) => match Refusal::guard_held(&e) {
                    Some(refusal) => {
                        std::fs::write(&pending, b"").with_context(|| format!("writing {}", pending.display()))?;
                        return Ok(Err(refusal));
                    }
                    None if failed.is_none() => return Err(e),
                    None => note(&format!("{e:#}")),
                },
            }
        }
        Ok(Ok(Landed { answer, failed }))
    }
}

pub(crate) fn receive(args: Receive<'_>, tasks: &Tasks) -> Result<()> {
    let l = args.project.locate(args.declaration.clone())?;
    let held = match crate::sync::pull_before_run_within(&l, Some(PUSH_WAIT_SECS)) {
        Ok(()) => None,
        Err(e) => Some(Refusal::guard_held(&e).ok_or(e)?),
    };
    let receiver = Receiver::open(&args.project, args.declaration, &args.id, args.site_id, args.site_id_env, tasks)?;
    let key = args.key;
    if key.trim().is_empty() {
        return Err(RunError::ReceiveBatchKeyMissing("`--key` names the batch; a resend under it replays what committed".into()).into());
    }
    let refuse = |refusal: Refusal| -> Result<()> {
        say(&receiver.refused(&key, &refusal));
        Err(refusal.error.into())
    };
    if let Some(refusal) = held {
        return refuse(refusal);
    }
    // The credential is checked before the body is read: it holds `write` over every table.
    let author = args.admit.author(args.project.project.as_deref(), &receiver.text, &[])?;
    if let Some(author) = &author {
        if let Some(refusal) = receiver.ungranted(author.authority()) {
            return refuse(refusal);
        }
    }
    let admitted = match receiver.admit(&key)? {
        Ok(admitted) => admitted,
        Err(refusal) => return refuse(refusal),
    };
    eprintln!("{}: batch `{key}` holds queue slot {} of {}", receiver.spec.id, admitted.slot + 1, receiver.config.max_queue_depth);

    // The reader stops one byte past the bound (`run.land.receive-batch-too-large`).
    let max_batch_bytes = receiver.config.max_batch_bytes;
    let mut body = Vec::new();
    std::io::stdin().lock().take(max_batch_bytes + 1).read_to_end(&mut body).context("reading the batch body on stdin")?;
    if body.len() as u64 > max_batch_bytes {
        return refuse(receiver.too_large(&key));
    }
    let landed = match receiver.land(admitted, &key, &body, author)? {
        Ok(landed) => landed,
        Err(refusal) => return refuse(refusal),
    };
    say(&landed.answer);
    match landed.failed {
        None => Ok(()),
        Some((table, true)) => bail!("pipeline `{}`: batch `{key}` committed no row of `{table}` or any table after it; resend it under the same key", receiver.spec.id),
        Some((table, false)) => bail!(
            "pipeline `{}`: batch `{key}` committed no row of `{table}` or any table after it, and its failure class does not retry; mend the batch before resending it under the same key",
            receiver.spec.id
        ),
    }
}

/// The served answer of `POST /receive/:pipeline` (`run.land.receive-endpoint`): the status,
/// the JSON body and whether it carries the retry interval.
pub(crate) struct Served {
    pub status: u16,
    pub body: Value,
    pub retry_after: bool,
}

fn served_refusal(receiver: Option<&Receiver>, key: &str, refusal: &Refusal) -> Served {
    let status = refusal.status();
    let text = refusal.error.to_string();
    let identifier = text.split(':').next().unwrap_or_default().trim().to_string();
    let mut body = match receiver {
        Some(r) => r.refused(key, refusal),
        None => json!({ "key": key, "committed": false, "retryable": refusal.retryable }),
    };
    body["error"] = json!({ "http": status, "identifier": identifier, "message": text });
    Served { status, body, retry_after: status == 503 }
}

fn served_error(status: u16, error: RunError) -> Served {
    let message = error.to_string();
    let identifier = message.split(':').next().unwrap_or_default().trim().to_string();
    Served { status, body: json!({ "error": { "http": status, "identifier": identifier, "message": message } }), retry_after: false }
}

/// The face's generic `500` body: a failure no clause names, its identifier withheld.
fn served_failure(e: &anyhow::Error) -> Served {
    Served { status: 500, body: json!({ "error": { "http": 500, "message": format!("{e:#}") } }), retry_after: false }
}

/// Land one served delivery to `pipeline` under `authority`: the grant and the queue slot
/// are checked before `read` takes the body, which it reads to at most `max_batch_bytes`.
#[allow(clippy::too_many_arguments)]
pub(crate) fn served(
    project: &ProjectArgs,
    declaration: &Path,
    tasks: &Tasks,
    author: Author,
    key: Option<&str>,
    pipeline: &str,
    read: &mut dyn FnMut(u64) -> std::result::Result<Option<Vec<u8>>, contextful_agent::http::HttpResponse>,
) -> std::result::Result<Served, contextful_agent::http::HttpResponse> {
    let receiver = match Receiver::open(project, Some(declaration.to_path_buf()), pipeline, None, None, tasks) {
        Ok(r) => r,
        Err(e) => {
            return Ok(match e.downcast::<RunError>() {
                Ok(unknown @ RunError::ReceivePipelineUnknown(_)) => served_error(404, unknown),
                Ok(other) => served_failure(&other.into()),
                Err(e) => served_failure(&e),
            })
        }
    };
    let Some(key) = key.map(str::trim).filter(|k| !k.is_empty()) else {
        return Ok(served_error(400, RunError::ReceiveBatchKeyMissing(format!("a delivery names its batch in `{}`", contextful_agent::http::BATCH_KEY_HEADER))));
    };
    if let Some(refusal) = receiver.ungranted(author.authority()) {
        return Ok(served_refusal(Some(&receiver), key, &refusal));
    }
    let admitted = match receiver.admit(key) {
        Ok(Ok(admitted)) => admitted,
        Ok(Err(refusal)) => return Ok(served_refusal(Some(&receiver), key, &refusal)),
        Err(e) => return Ok(served_failure(&e)),
    };
    let Some(body) = read(receiver.config.max_batch_bytes)? else {
        return Ok(served_refusal(Some(&receiver), key, &receiver.too_large(key)));
    };
    match receiver.land(admitted, key, &body, Some(author)) {
        Ok(Ok(Landed { answer, failed: None })) => Ok(Served { status: 200, body: answer, retry_after: false }),
        Ok(Ok(Landed { answer, failed: Some((_, retryable)) })) => Ok(Served { status: if retryable { 503 } else { 422 }, body: answer, retry_after: retryable }),
        Ok(Err(refusal)) => Ok(served_refusal(Some(&receiver), key, &refusal)),
        Err(e) => Ok(served_failure(&e)),
    }
}
