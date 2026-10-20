//! `contextful pipeline receive` and the served `POST /receive/:pipeline` route: delivered
//! batches of a `receive` pipeline spool behind its land lock and land through the pull land
//! path as one group, one run per touched table in declaration order, and each answer
//! follows the group's commits (`run.land.receive-entry`, `run.land.receive-group`,
//! `run.land.receive-ack`, `run.land.receive-endpoint`).

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

    /// The push guard's message of a [`Refusal::guard_held`] refusal, which a led member
    /// rebuilds from its answer.
    fn held(&self) -> String {
        match &self.error {
            RunError::Store(StoreError::SyncPushInFlight(m)) => m.clone(),
            other => other.to_string(),
        }
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
    applied_version: Option<u64>,
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

impl Receiver {
    /// Resolve the `receive` pipeline `id` of the project's declaration.
    pub(crate) fn open(project: &ProjectArgs, declaration: Option<PathBuf>, id: &str, site_id: Option<String>, site_id_env: Option<String>, tasks: &Tasks, use_applied: bool) -> Result<Receiver> {
        let l = project.locate(declaration)?;
        let declaration = l.declaration.clone();
        let text = if declaration.exists() { std::fs::read_to_string(&declaration)? } else { String::new() };
        let site_id = crate::run::site_id_for(&text, &declaration, site_id, site_id_env)?;
        let applied = if use_applied { crate::cadence::current_manifest(&l.project, &text)? } else { None };
        let (applied_version, files) = match applied {
            Some((version, snapshot)) => (Some(version), vec![crate::effective::public_manifest(snapshot)?]),
            None => (None, manifests(&declaration)?),
        };
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
        Ok(Receiver { l, text, site_id, spec, config, now: project.now.clone(), applied_version })
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
        let mut first: Option<i64> = None;
        for table in self.destinations() {
            if let Some(row) = wired.engine.receive_first(&self.spec.id, &table, key)? {
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

    /// Land one delivered `body` under `key` (`run.land.receive-spool`, `run.land.receive-group`):
    /// the batch decodes alone, so a refused body fails no other batch, then spools to the
    /// pipeline's queue and waits. The holder of the land lock lands every batch spooled by
    /// then as one group, one run per touched table, and answers each member after the
    /// group's commits; a batch neither led nor claimed within the commit wait withdraws.
    pub(crate) fn land(&self, admitted: Admitted, key: &str, body: &[u8], author: Option<Author>) -> Result<std::result::Result<Landed, Refusal>> {
        let names: Vec<&str> = self.spec.tables.iter().map(|t| t.name()).collect();
        let tables = match decode(body, &names) {
            Ok(split) => split,
            Err(e) => return Ok(Err(Refusal::final_(e))),
        };
        let credential = author.as_ref().map(|a| a.authority().credential_id().to_string());
        let member = Spooled { id: spool_id(credential.as_deref()), key: key.to_string(), credential, bytes: body.len() as u64, tables };
        let spool = self.spool();
        member.write(&spool)?;
        note(&format!("{}: batch `{key}` spooled; waiting for the land lock", self.spec.id));
        let w = admitted.wired;
        // The wait of `store.reserve.commit-wait` bounds a batch no group takes (`run.land.receive-wait`).
        let wait = std::time::Duration::from_secs(contextful_core::store::reserve::COMMIT_LOCK_WAIT_SECS);
        let started = std::time::Instant::now();
        let land = File::options().read(true).write(true).create(true).truncate(false).open(self.queue().join("land.lock"))?;
        loop {
            if let Some(answered) = Answered::take(&spool, &member.id)? {
                return answered.landed();
            }
            match land.try_lock() {
                Ok(()) => {
                    // A leader may answer between the look above and the lock.
                    if let Some(answered) = Answered::take(&spool, &member.id)? {
                        return answered.landed();
                    }
                    let landed = self.lead(&w, &member, author);
                    let _ = land.unlock();
                    return landed;
                }
                Err(std::fs::TryLockError::WouldBlock) => {}
                Err(std::fs::TryLockError::Error(e)) => return Err(e).context("locking the land lock"),
            }
            if started.elapsed() >= wait && member.withdraw(&spool)? {
                return Ok(Err(Refusal {
                    error: RunError::ReceiveBackpressure(format!(
                        "pipeline `{}` landed no group holding batch `{key}` within {} s; resend it after {RETRY_AFTER_MS} ms",
                        self.spec.id,
                        wait.as_secs()
                    )),
                    retryable: true,
                }));
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
    }

    /// The directory holding the pipeline's spooled batches and their answers.
    fn spool(&self) -> PathBuf {
        let spool = self.queue().join("spool");
        match self.applied_version {
            Some(version) => spool.join(format!("applied-{version}-{}", self.spec.content_hash())),
            None => spool,
        }
    }

    /// Lead under the held land lock (`run.land.receive-adopt`): first land the batches a dead
    /// leader claimed, as its group, then claim `own` and every batch spooled beside it under
    /// the same credential, up to `max_queue_depth` members and [`GROUP_BYTES`]; land them;
    /// answer each other member; answer `own`.
    fn lead(&self, w: &Wired, own: &Spooled, author: Option<Author>) -> Result<std::result::Result<Landed, Refusal>> {
        let spool = self.spool();
        collect_stale(&spool);
        let (orphans, waiting): (Vec<_>, Vec<_>) = Spooled::waiting(&spool, &credential_tag(own.credential.as_deref()))?.into_iter().filter(|(s, _)| s.credential == own.credential).partition(|(_, claimed)| *claimed);
        let mut orphans: Vec<Spooled> = orphans.into_iter().map(|(s, _)| s).collect();
        if !orphans.is_empty() {
            note(&format!("{}: landing {} batches a dead leader claimed", self.spec.id, orphans.len()));
            // A resend of a claimed key takes the answer its claimed batch lands.
            if orphans.iter().any(|o| o.key == own.key && o.id != own.id) {
                own.claim(&spool)?;
                orphans.push(own.clone());
            }
            if let Some(landed) = self.answer(w, &orphans, Some(&own.id), author.clone())? {
                return Ok(landed);
            }
        }
        let mut members = vec![own.clone()];
        let mut bytes = own.bytes;
        for (other, _) in waiting {
            if members.len() as u64 >= u64::from(self.config.max_queue_depth) {
                break;
            }
            if other.id == own.id || bytes.saturating_add(other.bytes) > GROUP_BYTES {
                continue;
            }
            bytes += other.bytes;
            members.push(other);
        }
        members.retain(|m| m.id == own.id || m.claim(&spool).unwrap_or(false));
        own.claim(&spool)?;
        let landed = self.answer(w, &members, Some(&own.id), author)?;
        landed.context("the leading batch answers in its own group")
    }

    /// Land claimed `members` as one group, answer each member but `own` through the spool and
    /// release them all; the answer of `own`, when it is a member.
    fn answer(&self, w: &Wired, members: &[Spooled], own: Option<&str>, author: Option<Author>) -> Result<Option<std::result::Result<Landed, Refusal>>> {
        let spool = self.spool();
        let landed = self.land_group(w, members, author);
        let answers: Vec<Answered> = match &landed {
            Ok(Ok(each)) => each.iter().map(|l| Answered { answer: l.answer.clone(), failed: l.failed.clone(), error: None, held: None }).collect(),
            Ok(Err(refusal)) => members.iter().map(|_| Answered { answer: Value::Null, failed: None, error: None, held: Some(refusal.held()) }).collect(),
            Err(e) => members.iter().map(|_| Answered { answer: Value::Null, failed: None, error: Some(format!("{e:#}")), held: None }).collect(),
        };
        // A member whose answer cannot be written still releases; it lands again as a replay.
        for (m, answer) in members.iter().zip(&answers) {
            if Some(m.id.as_str()) == own {
                continue;
            }
            if let Err(e) = answer.write(&spool, &m.id) {
                note(&format!("{}: answering batch `{}`: {e:#}", self.spec.id, m.key));
            }
        }
        for m in members {
            m.release(&spool);
        }
        Ok(match landed? {
            Ok(mut each) => members.iter().position(|m| Some(m.id.as_str()) == own).map(|at| Ok(each.swap_remove(at))),
            Err(refusal) => members.iter().any(|m| Some(m.id.as_str()) == own).then_some(Err(refusal)),
        })
    }

    /// Land `members`, one run per touched table in declaration order. A member key whose
    /// table committed replays it; the rest of the table's members land as one run, under
    /// the member's own key when it lands alone and under the group's key otherwise
    /// (`run.land.receive-group`). A group run that fails with a class that does not retry
    /// lands each of its members alone, so one bad batch fails no other. A member stops at
    /// its first failed table.
    fn land_group(&self, w: &Wired, members: &[Spooled], author: Option<Author>) -> Result<std::result::Result<Vec<Landed>, Refusal>> {
        let spec = &self.spec;
        let declaration = &self.l.declaration;
        let store = Store::open_declared(&self.l.project.dir, &self.l.project.name, declaration)?;
        let (node, _) = node::resolve(&store, |k| std::env::var(k).ok())?;
        let decls = spec.tables.iter().map(|t| spec.destination_decl(t)).collect();
        let normalize = Some(contextful_core::pipeline::normalize::Normalize::parse(spec.normalize.as_ref())?);
        let mut dest = StoreDestination { store, decls, node, author, normalize, relational_parts: Default::default(), schema_diffs: Vec::new() };
        let artifact = sha256_hex(serde_json::to_string(&spec.source).unwrap_or_default().as_bytes());
        // A key spooled twice lands once; its later members take the first one's answer.
        let first: Vec<usize> = members.iter().map(|m| members.iter().position(|o| o.key == m.key).unwrap_or(0)).collect();
        let mut tables: Vec<Vec<Value>> = vec![Vec::new(); members.len()];
        let mut failed: Vec<Option<(String, bool)>> = vec![None; members.len()];
        let mut opened = false;
        let audit = self.applied_version.map(|_| crate::project::read_audit(&self.l.project, None, None, None)).transpose()?;
        let prior: std::collections::BTreeSet<_> = if self.applied_version.is_some() {
            w.engine.catalog.runs(Some(&spec.id))?.into_iter().map(|row| row.run_id).collect()
        } else { Default::default() };
        for t in &spec.tables {
            let table = spec.table_name(t.name());
            let mut pending: Vec<(usize, String)> = Vec::new();
            for (i, m) in members.iter().enumerate() {
                if first[i] != i || failed[i].is_some() || !m.tables.contains_key(t.name()) {
                    continue;
                }
                let next = match w.engine.receive_landed(&spec.id, &table, &m.key)? {
                    Ok(row) => {
                        tables[i].push(json!({ "table": table, "run_id": row.run_id, "outcome": "replayed", "rows": row.rows }));
                        continue;
                    }
                    Err(next) => next,
                };
                // The key's own attempts and those of the group its scope names, which a dead
                // leader may have committed (`run.land.receive-torn`).
                let mut digests = vec![receive_digest(&spec.id, &m.key)];
                digests.extend(w.engine.receive_group(&spec.id, &table, &m.key)?);
                match self.torn(w, &dest.store, &table, &digests)? {
                    Some(row) => tables[i].push(json!({ "table": table, "run_id": row.run_id, "outcome": "replayed", "rows": row.rows })),
                    None => pending.push((i, next)),
                }
            }
            let (key, next, grouped) = match pending.as_slice() {
                [] => continue,
                [(i, next)] => (members[*i].key.clone(), next.clone(), false),
                _ => {
                    let group = group_key(pending.iter().map(|(i, _)| members[*i].key.as_str()));
                    let landed = match w.engine.receive_attempt(&format!("{}.{table}", self.base(&group)))? {
                        Ok(row) => Ok(row),
                        Err(next) => self.torn(w, &dest.store, &table, &[receive_digest(&spec.id, &group)])?.ok_or(next),
                    };
                    match landed {
                        Ok(row) => {
                            for (i, _) in &pending {
                                tables[*i].push(json!({ "table": table, "run_id": row.run_id, "outcome": "replayed", "rows": row.rows }));
                            }
                            continue;
                        }
                        Err(next) => (group, next, true),
                    }
                }
            };
            let mut land = |run_id: String, key: &str, keys: Option<Vec<String>>, rows: Vec<contextful_core::run::ports::Row>| -> Landing {
                let landing = self.land_table(w, &mut dest, t.name(), &table, &artifact, &run_id, key, keys, rows);
                opened |= landing.opened;
                landing
            };
            if !grouped {
                let i = pending[0].0;
                let landing = land(next, &key, None, members[i].tables[t.name()].clone());
                landing.answer(&table, &mut tables[i], &mut failed[i], None);
                continue;
            }
            let group = key;
            let keys: Vec<String> = pending.iter().map(|(i, _)| members[*i].key.clone()).collect();
            let rows: Vec<_> = pending.iter().flat_map(|(i, _)| members[*i].tables[t.name()].iter().cloned()).collect();
            let landing = land(next, &group, Some(keys), rows);
            if matches!(landing.result, Err(false)) {
                // A failure that does not retry lands each member alone (`run.land.receive-group`).
                for (i, next) in &pending {
                    let alone = land(next.clone(), &members[*i].key, None, members[*i].tables[t.name()].clone());
                    alone.answer(&table, &mut tables[*i], &mut failed[*i], None);
                }
                continue;
            }
            for (i, _) in &pending {
                landing.answer(&table, &mut tables[*i], &mut failed[*i], Some(members[*i].tables[t.name()].len() as u64));
            }
        }
        // A batch whose push met a held guard left the marker; the next batch pushes what it committed.
        let recorded = (|| -> Result<()> {
            if let (Some(version), Some(audit)) = (self.applied_version, audit) {
                let owned: std::collections::BTreeSet<_> = tables.iter().flatten().filter(|table| table["outcome"] != "replayed")
                    .filter_map(|table| table["run_id"].as_str()).filter(|id| !prior.contains(*id)).collect();
                let mut rows = Vec::new();
                for id in owned {
                    if w.engine.catalog.run(id)?.is_some_and(|row| row.pipeline_id == spec.id) { rows.push(id.to_string()); }
                }
                let key = if members.len() == 1 { members[0].key.clone() } else { group_key(members.iter().map(|member| member.key.as_str())) };
                crate::source_runs::record(&audit, version, spec, &self.base(&key), &rows, failed.iter().any(Option::is_some))?;
            }
            Ok(())
        })();
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
                    None if failed.iter().all(Option::is_none) => return Err(e),
                    None => note(&format!("{e:#}")),
                },
            }
        }
        recorded?;
        Ok(Ok(members
            .iter()
            .enumerate()
            .map(|(i, m)| {
                let at = first[i];
                let retryable = failed[at].as_ref().is_some_and(|(_, r)| *r);
                let answer = json!({ "pipeline": spec.id, "key": m.key, "run_id": self.base(&m.key), "committed": failed[at].is_none(), "retryable": retryable, "tables": tables[at] });
                Landed { answer, failed: failed[at].clone() }
            })
            .collect()))
    }

    /// The attempt, under any of the receive scopes `digests` name over `table`, that committed:
    /// one already closed `Success`, or one whose commit the store holds although its run row
    /// never closed, which closes committed with the rows its commit holds and retires its
    /// owner (`run.land.receive-torn`). Only an attempt whose row did not close reads the store.
    fn torn(&self, w: &Wired, store: &Store, table: &str, digests: &[String]) -> Result<Option<RunRow>> {
        if store.try_schema(table)?.is_none() {
            return Ok(None);
        }
        for digest in digests {
            let stem = format!("receive-{digest}.{table}");
            for n in 1u32.. {
                let id = if n == 1 { stem.clone() } else { format!("{stem}.{n}") };
                let Some(row) = w.engine.catalog.run(&id)? else { break };
                if row.status == RunStatus::Success {
                    return Ok(Some(row));
                }
                if let Some(rows) = store.committed_rows(table, &id)? {
                    note(&format!("{}: attempt `{id}` committed before its leader closed it; it replays", self.spec.id));
                    return Ok(Some(w.engine.receive_settle(&self.spec.id, table, digest, &id, rows)?));
                }
            }
        }
        Ok(None)
    }

    /// Land `rows` of table `name` as run `run_id` under batch `key`: alone, or for the
    /// member `keys` of a group.
    #[allow(clippy::too_many_arguments)]
    fn land_table(&self, w: &Wired, dest: &mut StoreDestination, name: &str, table: &str, artifact: &str, run_id: &str, key: &str, keys: Option<Vec<String>>, rows: Vec<contextful_core::run::ports::Row>) -> Landing {
        let spec = &self.spec;
        let trace = crate::telemetry::RunTrace::open();
        let outcome = (|| -> Result<RunRow> {
            let plan = plan(spec, name, built_in(spec))?;
            dest.store.validate_source_plan(&plan, dest.normalize)?;
            let connector = plan.connector_pin(artifact);
            let run = RunSpec { plan, connector, run_id: run_id.to_string(), site_id: self.site_id.clone(), pid: std::process::id(), boot_id: boot_id(), trace_id: Some(trace.trace_id()) };
            let shape = Chain { ops: spec.transforms.clone(), table: table.to_string() };
            let mut delivered = Delivered(rows);
            Ok(match &keys {
                None => w.engine.run_receive(&run, key, &mut delivered, &shape, dest)?,
                Some(keys) => w.engine.run_receive_group(&run, key, keys, &mut delivered, &shape, dest)?,
            })
        })();
        match outcome {
            Ok(row) => {
                trace.close(&row);
                if row.status == RunStatus::Success {
                    return Landing { run_id: row.run_id.clone(), opened: true, result: Ok(row.rows) };
                }
                let why = format!("{}: {}", row.error_kind.map(|k| k.name()).unwrap_or("unknown"), row.error_message.clone().unwrap_or_default());
                note(&RunError::PipelineTableFailed(format!("table `{table}` failed in run `{}`: {why}", row.run_id)));
                Landing { run_id: row.run_id, opened: true, result: Err(retries(row.error_kind)) }
            }
            Err(e) => {
                note(&RunError::PipelineTableFailed(format!("table `{table}` failed as refused in run `{run_id}`: {e:#}")));
                let opened = w.engine.catalog.run(run_id).ok().flatten().is_some();
                Landing { run_id: run_id.to_string(), opened, result: Err(retries(class(&e))) }
            }
        }
    }
}

/// One table's run for a batch or a group: its id, whether it opened, and the rows it
/// landed or whether its failure class retries.
struct Landing {
    run_id: String,
    opened: bool,
    result: std::result::Result<u64, bool>,
}

impl Landing {
    /// Record the run in a member's `tables` answer and `failed` slot; a group member
    /// answers its own `delivered` rows.
    fn answer(&self, table: &str, tables: &mut Vec<Value>, failed: &mut Option<(String, bool)>, delivered: Option<u64>) {
        match self.result {
            Ok(rows) => tables.push(json!({ "table": table, "run_id": self.run_id, "outcome": "committed", "rows": delivered.unwrap_or(rows) })),
            Err(retryable) => {
                tables.push(json!({ "table": table, "run_id": self.run_id, "outcome": "failed" }));
                *failed = Some((table.to_string(), retryable));
            }
        }
    }
}

/// The most bytes of spooled bodies one group lands (`run.land.receive-group`).
const GROUP_BYTES: u64 = 64 << 20;

/// How long an answer no member took, a batch no group took, or a partial spool write stays
/// in the spool.
const SPOOL_TTL: std::time::Duration = std::time::Duration::from_secs(3600);

/// The key a group of member `keys` lands under: no sender's key, so no resend resumes it, and
/// independent of the members' order, so a dead leader's members land again under it
/// (`run.land.receive-adopt`).
fn group_key<'a>(keys: impl Iterator<Item = &'a str>) -> String {
    let keys: std::collections::BTreeSet<&str> = keys.collect();
    let mut joined = String::from("group");
    for key in keys {
        joined.push('\0');
        joined.push_str(key);
    }
    joined
}

/// A fresh spool id, ordered by the instant it was taken and ending in the admitting
/// credential's tag, so a leader lists its credential's batches without reading others.
fn spool_id(credential: Option<&str>) -> String {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or_default();
    format!("{nanos:024}-{:010}-{:08}-{}", std::process::id(), NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed), credential_tag(credential))
}

/// The spool-id suffix of a credential; `anonymous` for an unauthored batch.
fn credential_tag(credential: Option<&str>) -> String {
    match credential {
        Some(id) => sha256_hex(id.as_bytes())[..16].to_string(),
        None => "anonymous".to_string(),
    }
}

/// Remove spool entries older than [`SPOOL_TTL`]: answers no member took, batches no group
/// took, and partial writes a crash left.
fn collect_stale(spool: &Path) {
    let Ok(entries) = std::fs::read_dir(spool) else { return };
    for path in entries.flatten().map(|e| e.path()).filter(|p| p.extension().is_some_and(|e| e == "answer" || e == "tmp" || e == "batch" || e == "claimed")) {
        let stale = std::fs::metadata(&path).and_then(|m| m.modified()).ok().and_then(|at| at.elapsed().ok()).is_some_and(|age| age > SPOOL_TTL);
        if stale {
            let _ = std::fs::remove_file(&path);
        }
    }
}

/// Write `bytes` to `path` through a synced sibling and a rename, then sync the directory.
fn write_synced(path: &Path, bytes: &[u8]) -> Result<()> {
    use std::io::Write;
    let tmp = path.with_extension("tmp");
    let mut file = File::create(&tmp).with_context(|| format!("creating {}", tmp.display()))?;
    file.write_all(bytes).with_context(|| format!("writing {}", tmp.display()))?;
    file.sync_all().with_context(|| format!("syncing {}", tmp.display()))?;
    std::fs::rename(&tmp, path).with_context(|| format!("renaming {}", path.display()))?;
    #[cfg(unix)]
    if let Some(dir) = path.parent() {
        File::open(dir).and_then(|d| d.sync_all()).with_context(|| format!("syncing {}", dir.display()))?;
    }
    Ok(())
}

/// One decoded batch in the spool: `<id>.batch` while it waits, `<id>.claimed` once a
/// leader lands it (`run.land.receive-spool`).
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
struct Spooled {
    id: String,
    key: String,
    /// The id of the admitting credential; `None` lands unauthored. A group holds one
    /// credential, so the leader's commit boundary is every member's.
    credential: Option<String>,
    bytes: u64,
    tables: std::collections::BTreeMap<String, Vec<contextful_core::run::ports::Row>>,
}

impl Spooled {
    fn path(spool: &Path, id: &str, state: &str) -> PathBuf {
        spool.join(format!("{id}.{state}"))
    }

    /// Spool the batch, synced, before it waits for the land lock.
    fn write(&self, spool: &Path) -> Result<()> {
        std::fs::create_dir_all(spool).with_context(|| format!("creating {}", spool.display()))?;
        write_synced(&Spooled::path(spool, &self.id, "batch"), &serde_json::to_vec(self)?)
    }

    /// Every batch in the spool under the credential `tag`, oldest first, and whether a leader
    /// claimed it: under the held land lock, a claiming leader died.
    fn waiting(spool: &Path, tag: &str) -> Result<Vec<(Spooled, bool)>> {
        let mut found = Vec::new();
        for entry in std::fs::read_dir(spool).with_context(|| format!("listing {}", spool.display()))? {
            let path = entry?.path();
            if !path.extension().is_some_and(|e| e == "batch" || e == "claimed") || !path.file_stem().and_then(|s| s.to_str()).is_some_and(|s| s.ends_with(tag)) {
                continue;
            }
            // A batch withdrawn or claimed since the listing is gone; it is not this group's.
            let Ok(bytes) = std::fs::read(&path) else { continue };
            let claimed = path.extension().is_some_and(|e| e == "claimed");
            match serde_json::from_slice::<Spooled>(&bytes) {
                Ok(spooled) => found.push((spooled, claimed)),
                Err(e) => note(&format!("skipping spooled batch {}: {e}", path.display())),
            }
        }
        // A batch claimed between the two looks lists twice; its claim wins.
        found.sort_by(|(a, x), (b, y)| a.id.cmp(&b.id).then(y.cmp(x)));
        found.dedup_by(|(a, _), (b, _)| a.id == b.id);
        Ok(found)
    }

    /// Claim the batch for the leading group; `false` once its member withdrew it.
    fn claim(&self, spool: &Path) -> Result<bool> {
        let claimed = Spooled::path(spool, &self.id, "claimed");
        match std::fs::rename(Spooled::path(spool, &self.id, "batch"), &claimed) {
            Ok(()) => Ok(true),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(claimed.exists()),
            Err(e) => Err(e).with_context(|| format!("claiming {}", claimed.display())),
        }
    }

    /// Withdraw the batch while no leader has claimed it; `false` once one has.
    fn withdraw(&self, spool: &Path) -> Result<bool> {
        match std::fs::remove_file(Spooled::path(spool, &self.id, "batch")) {
            Ok(()) => Ok(true),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
            Err(e) => Err(e).context("withdrawing a spooled batch"),
        }
    }

    /// Drop the batch from the spool once its group answered.
    fn release(&self, spool: &Path) {
        let _ = std::fs::remove_file(Spooled::path(spool, &self.id, "claimed"));
    }
}

/// A leader's answer to one member, `<id>.answer` in the spool.
#[derive(Debug, serde::Serialize, serde::Deserialize)]
struct Answered {
    answer: Value,
    failed: Option<(String, bool)>,
    error: Option<String>,
    /// The push guard's message when the group's push met it held (`run.land.receive-push-wait`).
    #[serde(default)]
    held: Option<String>,
}

impl Answered {
    fn write(&self, spool: &Path, id: &str) -> Result<()> {
        write_synced(&Spooled::path(spool, id, "answer"), &serde_json::to_vec(self)?)
    }

    /// Take member `id`'s answer, when its group answered it.
    fn take(spool: &Path, id: &str) -> Result<Option<Answered>> {
        let path = Spooled::path(spool, id, "answer");
        let bytes = match std::fs::read(&path) {
            Ok(bytes) => bytes,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(e).with_context(|| format!("reading {}", path.display())),
        };
        let answered = serde_json::from_slice(&bytes).with_context(|| format!("reading {}", path.display()))?;
        let _ = std::fs::remove_file(&path);
        Ok(Some(answered))
    }

    fn landed(self) -> Result<std::result::Result<Landed, Refusal>> {
        if let Some(error) = self.error {
            bail!("{error}");
        }
        Ok(match self.held {
            Some(held) => Err(Refusal { error: RunError::Store(StoreError::SyncPushInFlight(held)), retryable: true }),
            None => Ok(Landed { answer: self.answer, failed: self.failed }),
        })
    }
}

pub(crate) fn receive(args: Receive<'_>, tasks: &Tasks) -> Result<()> {
    let l = args.project.locate(args.declaration.clone())?;
    let held = match crate::sync::pull_before_run_within(&l, Some(PUSH_WAIT_SECS)) {
        Ok(()) => None,
        Err(e) => Some(Refusal::guard_held(&e).ok_or(e)?),
    };
    let receiver = Receiver::open(&args.project, args.declaration, &args.id, args.site_id, args.site_id_env, tasks, false)?;
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
    let receiver = match Receiver::open(project, Some(declaration.to_path_buf()), pipeline, None, None, tasks, true) {
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
