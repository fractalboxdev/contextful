//! The queued answer of a served delivery and its consumer: the route admits, queues and
//! answers `202` (`run.land.receive-queued`); `contextful queue drain` admits each queued
//! batch's credential again, lands the batches per credential through the receive group,
//! pushes, and records each outcome before its message is acknowledged
//! (`run.land.receive-queued-consume`); `GET /receive/:pipeline/:key` reads that outcome
//! (`run.land.receive-queued-outcome`).

use super::{class, credential_tag, decode, json, spool_id, Admitted, Receiver, Refusal, Served, Spooled, Value, GROUP_BYTES};
use crate::admit::{AdmitArgs, Author};
use crate::run::{wire_at, ProjectArgs, Wired};
use anyhow::{Context, Result};
use contextful_core::run::derive::task::Tasks;
use contextful_core::run::{Failure, FailureTag, RunError};
use contextful_engine::queue::{enqueue, Batch};
use contextful_policy::verify::AdmittedAuthority;
use std::collections::BTreeMap;
use std::fs::File;
use std::path::{Path, PathBuf};

/// A recorded outcome's `outcome` field.
const COMMITTED: &str = "committed";
const REFUSED: &str = "refused";

/// The identifier and message of a typed refusal's text, `<Identifier>: <message>`.
fn identified(text: &str) -> Value {
    let identifier = text.split(':').next().unwrap_or_default().trim();
    json!({ "identifier": identifier, "message": text })
}

impl Receiver {
    /// Queue an admitted delivery's `body` under `key` and answer the receipt
    /// (`run.land.receive-queued`, `run.land.receive-queued-spill`). The key window opens
    /// now (`run.land.receive-queued-window`); the queue slot frees once the message is sent.
    /// The message carries the route span the delivery arrived under
    /// (`disclosure.record.queued-parent`).
    pub(super) fn enqueue(&self, admitted: Admitted, key: &str, credential: Option<&str>, body: &[u8]) -> Result<Value> {
        let queued_at = admitted.wired.clock.now().unix_secs();
        let port = crate::queue::Port::open(&self.l)?;
        let trace = self.trace.as_ref().map(contextful_core::trace::Carrier::of);
        port.with(|queue, bodies| enqueue(queue, bodies, &self.spec.id, key, credential, queued_at, trace.as_ref(), body))?;
        Ok(json!({
            "pipeline": self.spec.id,
            "key": key,
            "run_id": self.base(key),
            "queued": true,
            "outcome": format!("{}{}/{key}", contextful_agent::http::RECEIVE_PREFIX, self.spec.id),
        }))
    }

    /// The first table the pipeline declares: the receive scope a queued outcome records on.
    fn outcome_table(&self) -> String {
        self.destinations().into_iter().next().unwrap_or_default()
    }

    fn recorded(&self, w: &Wired, key: &str) -> Result<Option<Value>> {
        Ok(w.engine.receive_outcome(&self.spec.id, &self.outcome_table(), key)?)
    }

    fn record(&self, w: &Wired, key: &str, outcome: Value) -> Result<()> {
        Ok(w.engine.receive_record_outcome(&self.spec.id, &self.outcome_table(), key, outcome)?)
    }

    fn refuse(&self, w: &Wired, key: &str, error: &str) -> Result<()> {
        super::note(&format!("{}: queued batch `{key}` refused: {error}", self.spec.id));
        self.record(w, key, json!({ "outcome": REFUSED, "error": identified(error) }))
    }

    /// Whether `key`'s window has closed: past `key_retention_secs` from when it queued, or
    /// from its first landing attempt (`run.land.receive-queued-window`).
    fn window_closed(&self, w: &Wired, key: &str, queued_at: i64) -> Result<Option<String>> {
        let retention = self.config.key_retention_secs as i64;
        let now = w.clock.now().unix_secs();
        let mut opened = queued_at;
        for table in self.destinations() {
            if let Some(row) = w.engine.receive_first(&self.spec.id, &table, key)? {
                opened = opened.min(row.started_at.unix_secs());
            }
        }
        let age = now - opened;
        Ok((age > retention).then(|| {
            RunError::ReceiveBatchKeyExpired(format!(
                "batch key `{key}` of pipeline `{}` queued or first landed {age} s ago, past `key_retention_secs = {retention}`; send the rows under a new key",
                self.spec.id
            ))
            .to_string()
        }))
    }

    /// Check one queued batch as the route and the land path would, and decode it: the
    /// spooled member and its author, or the refusal its outcome records.
    fn admit_queued(&self, w: &Wired, admit: &AdmitArgs, project: Option<&str>, batch: &Batch, body: &[u8]) -> Result<std::result::Result<(Spooled, Option<Author>), String>> {
        let key = &batch.batch_key;
        if let Some(expired) = self.window_closed(w, key, batch.queued_at)? {
            return Ok(Err(expired));
        }
        let author = match &batch.credential {
            Some(credential) => match admit.carried(project, credential) {
                Ok(author) => Some(author),
                Err(e) => return Ok(Err(format!("{e:#}"))),
            },
            None => None,
        };
        if let Some(Refusal { error, .. }) = author.as_ref().and_then(|a| self.ungranted(a.authority())) {
            return Ok(Err(error.to_string()));
        }
        if body.len() as u64 > self.config.max_batch_bytes {
            return Ok(Err(self.too_large(key).error.to_string()));
        }
        let names: Vec<&str> = self.spec.tables.iter().map(|t| t.name()).collect();
        let tables = match decode(body, &names) {
            Ok(split) => split,
            Err(e) => return Ok(Err(e.to_string())),
        };
        let credential = author.as_ref().map(|a| a.authority().credential_id().to_string());
        let trace = batch.trace.as_ref().and_then(contextful_core::trace::Carrier::context);
        Ok(Ok((Spooled { id: spool_id(credential.as_deref()), key: key.clone(), credential, bytes: body.len() as u64, tables, trace }, author)))
    }

    /// Land the admitted `members` of one credential as groups of at most `max_queue_depth`
    /// members and [`GROUP_BYTES`], recording each member's outcome; the answer of each
    /// member, in order.
    fn land_queued(&self, w: &Wired, members: Vec<Spooled>, author: Option<Author>) -> Vec<std::result::Result<(), Failure>> {
        let mut answers = Vec::with_capacity(members.len());
        let mut chunk: Vec<Spooled> = Vec::new();
        let mut bytes = 0u64;
        let mut chunks = Vec::new();
        for m in members {
            if !chunk.is_empty() && (chunk.len() as u64 >= u64::from(self.config.max_queue_depth) || bytes.saturating_add(m.bytes) > GROUP_BYTES) {
                chunks.push(std::mem::take(&mut chunk));
                bytes = 0;
            }
            bytes += m.bytes;
            chunk.push(m);
        }
        if !chunk.is_empty() {
            chunks.push(chunk);
        }
        for group in chunks {
            match self.land_group(w, &group, author.clone()) {
                Ok(Ok(each)) => {
                    for (m, landed) in group.iter().zip(each) {
                        answers.push(self.settle(w, &m.key, &landed.answer, landed.failed));
                    }
                }
                Ok(Err(refusal)) => answers.extend(group.iter().map(|_| Err(Failure::new(FailureTag::Transient, refusal.error.to_string())))),
                Err(e) => {
                    let tag = class(&e).filter(|t| super::retries(Some(*t))).unwrap_or(FailureTag::Transient);
                    answers.extend(group.iter().map(|_| Err(Failure::new(tag, format!("{e:#}")))));
                }
            }
        }
        answers
    }

    /// Record a landed member's outcome: committed, or the refusal a table's non-retryable
    /// failure raised; a retryable failure records nothing and leaves the message queued.
    fn settle(&self, w: &Wired, key: &str, answer: &Value, failed: Option<(String, bool)>) -> std::result::Result<(), Failure> {
        let storage = |e: anyhow::Error| Failure::new(FailureTag::Transient, format!("{e:#}"));
        match failed {
            None => self.record(w, key, json!({ "outcome": COMMITTED })).map_err(storage),
            Some((table, true)) => Err(Failure::new(FailureTag::Transient, format!("batch `{key}` committed no row of `{table}`; it redelivers"))),
            Some((table, false)) => {
                let run = answer["tables"].as_array().and_then(|t| t.iter().find(|t| t["table"] == table.as_str())).and_then(|t| t["run_id"].as_str()).unwrap_or_default().to_string();
                let why = w.engine.catalog.run(&run).ok().flatten().map(|r| format!("{}: {}", r.error_kind.map(|k| k.name()).unwrap_or("unknown"), r.error_message.unwrap_or_default())).unwrap_or_default();
                let error = RunError::PipelineTableFailed(format!("table `{table}` failed in run `{run}`: {why}")).to_string();
                self.refuse(w, key, &error).map_err(storage)
            }
        }
    }
}

/// Land the queued `batches` (`run.land.receive-queued-consume`), each pipeline's under its
/// land lock: the answer of each batch, in order. `Ok` once its outcome records, committed
/// or refused; a batch whose outcome already recorded settles without landing again
/// (`run.land.receive-queued-redelivery`).
pub(crate) fn consume(project: &ProjectArgs, declaration: Option<PathBuf>, tasks: &Tasks, admit: &AdmitArgs, batches: &[Batch]) -> Vec<std::result::Result<(), Failure>> {
    let mut answers: Vec<Option<std::result::Result<(), Failure>>> = vec![None; batches.len()];
    let mut pipelines: BTreeMap<&str, Vec<usize>> = BTreeMap::new();
    for (i, b) in batches.iter().enumerate() {
        pipelines.entry(b.pipeline.as_str()).or_default().push(i);
    }
    for (pipeline, at) in pipelines {
        if let Err(e) = consume_pipeline(project, declaration.clone(), tasks, admit, pipeline, &at, batches, &mut answers) {
            super::note(&format!("{pipeline}: {e:#}"));
            for i in at {
                answers[i].get_or_insert_with(|| Err(Failure::new(FailureTag::Transient, format!("{e:#}"))));
            }
        }
    }
    answers.into_iter().map(|a| a.unwrap_or_else(|| Err(Failure::new(FailureTag::Transient, "the batch landed in no group")))).collect()
}

/// One credential's admitted batches: its author and each batch's index and member.
type Credentialed = (Option<Author>, Vec<(usize, Spooled)>);

#[allow(clippy::too_many_arguments)]
fn consume_pipeline(
    project: &ProjectArgs,
    declaration: Option<PathBuf>,
    tasks: &Tasks,
    admit: &AdmitArgs,
    pipeline: &str,
    at: &[usize],
    batches: &[Batch],
    answers: &mut [Option<std::result::Result<(), Failure>>],
) -> Result<()> {
    let receiver = match Receiver::open(project, declaration, pipeline, None, None, tasks, true) {
        Ok(r) => r,
        Err(e) if matches!(e.downcast_ref::<RunError>(), Some(RunError::ReceivePipelineUnknown(_))) => {
            // A pipeline undeclared since its batches queued has no table to land in.
            super::note(&format!("{e:#}; its queued batches leave the queue"));
            for i in at {
                answers[*i] = Some(Ok(()));
            }
            return Ok(());
        }
        Err(e) => return Err(e),
    };
    let lock = receiver.queue().join("land.lock");
    std::fs::create_dir_all(receiver.queue()).with_context(|| format!("creating {}", receiver.queue().display()))?;
    let land = File::options().read(true).write(true).create(true).truncate(false).open(&lock).with_context(|| format!("opening {}", lock.display()))?;
    land.lock().with_context(|| format!("locking {}", lock.display()))?;
    let w = wire_at(&receiver.l.project, &receiver.now)?;
    let mut groups: BTreeMap<String, Credentialed> = BTreeMap::new();
    for &i in at {
        let b = &batches[i];
        // A spilled body is gone only once an earlier delivery's outcome recorded.
        let Some(body) = &b.body else {
            answers[i] = Some(Ok(()));
            continue;
        };
        if receiver.recorded(&w, &b.batch_key)?.is_some() {
            answers[i] = Some(Ok(()));
            continue;
        }
        match receiver.admit_queued(&w, admit, project.project.as_deref(), b, body)? {
            Err(refusal) => {
                receiver.refuse(&w, &b.batch_key, &refusal)?;
                answers[i] = Some(Ok(()));
            }
            Ok((member, author)) => {
                let tag = credential_tag(member.credential.as_deref());
                groups.entry(tag).or_insert_with(|| (author, Vec::new())).1.push((i, member));
            }
        }
    }
    for (_, (author, members)) in groups {
        let (at, members): (Vec<usize>, Vec<Spooled>) = members.into_iter().unzip();
        for (i, answer) in at.into_iter().zip(receiver.land_queued(&w, members, author)) {
            answers[i] = Some(answer);
        }
    }
    let _ = land.unlock();
    Ok(())
}

/// The served answer of `GET /receive/:pipeline/:key` under `authority`
/// (`run.land.receive-queued-outcome`, `run.land.receive-queued-outcome-grant`).
pub(crate) fn outcome(project: &ProjectArgs, declaration: &Path, tasks: &Tasks, authority: &AdmittedAuthority, pipeline: &str, key: &str) -> Served {
    let receiver = match Receiver::open(project, Some(declaration.to_path_buf()), pipeline, None, None, tasks, true) {
        Ok(r) => r,
        Err(e) => {
            return match e.downcast::<RunError>() {
                Ok(unknown @ RunError::ReceivePipelineUnknown(_)) => super::served_error(404, unknown),
                Ok(other) => super::served_failure(&other.into()),
                Err(e) => super::served_failure(&e),
            }
        }
    };
    if let Some(refusal) = receiver.ungranted(authority) {
        return super::served_refusal(Some(&receiver), key, &refusal);
    }
    let read = || -> Result<Value> {
        let w = wire_at(&receiver.l.project, &receiver.now)?;
        let mut answer = json!({ "pipeline": receiver.spec.id, "key": key, "run_id": receiver.base(key) });
        match receiver.recorded(&w, key)? {
            Some(recorded) if recorded["outcome"] == COMMITTED => {
                let mut tables = Vec::new();
                for table in receiver.destinations() {
                    if let Ok(row) = w.engine.receive_landed(&receiver.spec.id, &table, key)? {
                        tables.push(json!({ "table": table, "run_id": row.run_id, "outcome": COMMITTED, "rows": row.rows }));
                    }
                }
                answer["outcome"] = json!(COMMITTED);
                answer["tables"] = json!(tables);
            }
            Some(recorded) => {
                answer["outcome"] = json!(REFUSED);
                answer["error"] = recorded["error"].clone();
            }
            None => answer["outcome"] = json!("pending"),
        }
        Ok(answer)
    };
    match read() {
        Ok(body) => Served { status: 200, body, retry_after: false },
        Err(e) => super::served_failure(&e),
    }
}
