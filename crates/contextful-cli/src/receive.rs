//! `contextful pipeline receive`: one delivered batch of a `receive` pipeline lands through
//! the pull land path, one run per touched table in declaration order, and the answer
//! follows the last commit (`run.land.receive-entry`, `run.land.receive-ack`).

use crate::admit::AdmitArgs;
use crate::pipeline::{built_in, check, manifests, plan, Checked};
use crate::run::{boot_id, wire_at, ProjectArgs, StoreDestination};
use anyhow::{bail, Context, Result};
use contextful_connectors::receive::{decode, Delivered, ReceiveConfig, RETRY_AFTER_MS};
use contextful_context::{node, Store};
use contextful_core::pipeline::declare::collect;
use contextful_core::pipeline::transform::Chain;
use contextful_core::run::derive::task::Tasks;
use contextful_core::run::journal::sha256_hex;
use contextful_core::run::own::receive_digest;
use contextful_core::run::record::{RunRow, RunStatus};
use contextful_core::run::{Failure, FailureTag, RunError};
use contextful_engine::{EngineError, RunSpec};
use serde_json::{json, Value};
use std::fs::File;
use std::io::Read;
use std::path::{Path, PathBuf};

/// The arguments of one receive.
pub(crate) struct Receive<'a> {
    pub id: String,
    pub key: String,
    pub project: ProjectArgs,
    pub declaration: Option<PathBuf>,
    pub site_id: Option<String>,
    pub site_id_env: Option<String>,
    pub admit: &'a AdmitArgs,
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

pub(crate) fn receive(args: Receive<'_>, tasks: &Tasks) -> Result<()> {
    let l = args.project.locate(args.declaration)?;
    crate::sync::pull_before_run(&l)?;
    let declaration = l.declaration.clone();
    let text = if declaration.exists() { std::fs::read_to_string(&declaration)? } else { String::new() };
    let site_id = crate::run::site_id_for(&text, &declaration, args.site_id, args.site_id_env)?;
    let files = manifests(&declaration)?;
    crate::secrets::check_records(&declaration, &files)?;
    let spec = collect(&files)?.into_iter().find(|d| d.spec.id == args.id).with_context(|| format!("no pipeline `{}` is declared", args.id))?.spec;
    let Checked::Receive(config) = check(&spec, &declaration, tasks)? else {
        bail!("pipeline `{}` reads source `{}`; `pipeline receive` lands the delivered batches of a `receive` source", spec.id, spec.source.name);
    };
    let ReceiveConfig { max_batch_bytes, max_queue_depth } = config;
    let key = args.key;
    if key.trim().is_empty() {
        bail!("`--key` names the batch; a resend under it replays what committed");
    }
    let base = format!("receive-{}", receive_digest(&spec.id, &key));
    let refused = |retryable: bool, e: &RunError| json!({ "pipeline": spec.id, "key": key, "run_id": base, "committed": false, "retryable": retryable, "error": e.to_string() });
    // The credential is checked before the body is read: it holds `write` over every table.
    let destinations: Vec<String> = spec.tables.iter().map(|t| spec.table_name(t.name())).collect();
    let author = args.admit.author(args.project.project.as_deref(), &text, &destinations.iter().map(String::as_str).collect::<Vec<_>>())?;

    let queue = l.project.run_dir().join("receive").join(&spec.id);
    let Some((slot, _held)) = take_slot(&queue, max_queue_depth)? else {
        let e = RunError::ReceiveBackpressure(format!(
            "pipeline `{}` holds all {max_queue_depth} queue slots; resend batch `{key}` after {RETRY_AFTER_MS} ms",
            spec.id
        ));
        let mut answer = refused(true, &e);
        answer["retry_after_ms"] = json!(RETRY_AFTER_MS);
        say(&answer);
        return Err(e.into());
    };
    eprintln!("{}: batch `{key}` holds queue slot {} of {max_queue_depth}", spec.id, slot + 1);

    // The reader stops one byte past the bound (`run.land.receive-batch-too-large`).
    let mut body = Vec::new();
    std::io::stdin().lock().take(max_batch_bytes + 1).read_to_end(&mut body).context("reading the batch body on stdin")?;
    if body.len() as u64 > max_batch_bytes {
        let e = RunError::ReceiveBatchTooLarge(format!("batch `{key}` of pipeline `{}` passes `max_batch_bytes = {max_batch_bytes}`", spec.id));
        say(&refused(false, &e));
        return Err(e.into());
    }
    let names: Vec<&str> = spec.tables.iter().map(|t| t.name()).collect();
    let mut split = match decode(&body, &names) {
        Ok(split) => split,
        Err(e) => {
            say(&refused(false, &e));
            return Err(e.into());
        }
    };

    // Batches of one pipeline land one at a time; the waiting ones hold their slots.
    let land = File::options().read(true).write(true).create(true).truncate(false).open(queue.join("land.lock"))?;
    land.lock()?;

    let w = wire_at(&l.project, &args.project.now)?;
    let store = Store::open_declared(&l.project.dir, &l.project.name, &declaration)?;
    let (node, _) = node::resolve(&store, |k| std::env::var(k).ok())?;
    let decls = spec.tables.iter().map(|t| spec.destination_decl(t)).collect();
    let normalize = Some(contextful_core::pipeline::normalize::Normalize::parse(spec.normalize.as_ref())?);
    let mut dest = StoreDestination { store, decls, node, author, normalize, relational_parts: Default::default(), schema_diffs: Vec::new() };
    let artifact = sha256_hex(serde_json::to_string(&spec.source).unwrap_or_default().as_bytes());
    let mut tables = Vec::new();
    let mut failed: Option<String> = None;
    let mut retryable = false;
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
        let outcome = (|| -> Result<RunRow> {
            let plan = plan(&spec, t.name(), built_in(&spec))?;
            dest.store.validate_source_plan(&plan, dest.normalize)?;
            let connector = plan.connector_pin(&artifact);
            let run = RunSpec { plan, connector, run_id: next.clone(), site_id: site_id.clone(), pid: std::process::id(), boot_id: boot_id(), trace_id: None };
            let shape = Chain { ops: spec.transforms.clone(), table: table.clone() };
            Ok(w.engine.run_receive(&run, &key, &mut Delivered(rows), &shape, &mut dest)?)
        })();
        opened |= w.engine.catalog.run(&next).ok().flatten().is_some();
        match outcome {
            Ok(row) if row.status == RunStatus::Success => {
                tables.push(json!({ "table": table, "run_id": row.run_id, "outcome": "committed", "rows": row.rows }));
            }
            Ok(row) => {
                let why = format!("{}: {}", row.error_kind.map(|k| k.name()).unwrap_or("unknown"), row.error_message.unwrap_or_default());
                eprintln!("{}", RunError::PipelineTableFailed(format!("table `{table}` failed in run `{}`: {why}", row.run_id)));
                retryable = retries(row.error_kind);
                tables.push(json!({ "table": table, "run_id": row.run_id, "outcome": "failed" }));
                failed = Some(table);
                break;
            }
            Err(e) => {
                eprintln!("{}", RunError::PipelineTableFailed(format!("table `{table}` failed as refused in run `{next}`: {e:#}")));
                retryable = retries(class(&e));
                tables.push(json!({ "table": table, "run_id": next, "outcome": "failed" }));
                failed = Some(table);
                break;
            }
        }
    }
    let committed = failed.is_none();
    say(&json!({ "pipeline": spec.id, "key": key, "run_id": base, "committed": committed, "retryable": retryable, "tables": tables }));
    let pushed = if opened { crate::sync::push_after_run(&l, w.clock.now()) } else { Ok(()) };
    if let Some(table) = failed {
        if let Err(e) = pushed {
            eprintln!("{e:#}");
        }
        if retryable {
            bail!("pipeline `{}`: batch `{key}` committed no row of `{table}` or any table after it; resend it under the same key", spec.id);
        }
        bail!("pipeline `{}`: batch `{key}` committed no row of `{table}` or any table after it, and its failure class does not retry; mend the batch before resending it under the same key", spec.id);
    }
    pushed
}
