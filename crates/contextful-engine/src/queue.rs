//! The engine's side of the `Queue` port (`topology.compose.queue-extension`): the local
//! spool every profile without a `queue` role uses, an in-process queue for tests, and the
//! drain that routes received hints to the reads they wake.
//!
//! A consumer only wakes or hands a batch to the engine's landing path: an export reads
//! from its own cursor, an object read lists from its held position, and a queued `receive`
//! batch lands through the receive group, so the store's writer stays the engine's.

use crate::fsutil::{replace, FileLock};
use contextful_core::run::journal::sha256_hex;
use contextful_core::run::queue::{BodyRef, BodyStore, Delivery, Queue, QueueMessage};
use contextful_core::run::{Failure, FailureTag};
use contextful_core::trace::Carrier;
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;

/// The lock file serializing a spool's sends and acknowledgements.
const LOCK_FILE: &str = "spool.lock";

/// The directory under a spool holding bodies past its message cap.
const BODIES: &str = "bodies";

/// The largest message the local spool takes, in bytes: a managed queue's 1 MiB, so a body
/// spills on the laptop exactly where it spills on a cluster (`run.land.receive-queued-spill`).
pub const SPOOL_MESSAGE_CAP: usize = 1 << 20;

/// One spooled message and the sequence that tells a resend from the copy it replaced.
#[derive(Serialize, Deserialize)]
struct Spooled {
    seq: u64,
    message: QueueMessage,
}

/// The local spool: one file per message identity under a machine-local directory. A
/// resend under an identity replaces the pending copy, so the spool holds at most one
/// message per table, object and batch key; an acknowledgement removes the copy it
/// received and keeps a resend that replaced it.
pub struct LocalSpool {
    dir: PathBuf,
    counter: AtomicU64,
}

fn storage(path: &Path, e: impl std::fmt::Display) -> Failure {
    Failure::new(FailureTag::Storage, format!("the queue spool `{}`: {e}", path.display()))
}

fn sequence(counter: &AtomicU64) -> u64 {
    let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos() as u64).unwrap_or_default();
    // Strictly increasing within a process even when the clock reads one instant twice.
    let mut last = counter.load(Ordering::SeqCst);
    loop {
        let next = nanos.max(last + 1);
        match counter.compare_exchange(last, next, Ordering::SeqCst, Ordering::SeqCst) {
            Ok(_) => return next,
            Err(seen) => last = seen,
        }
    }
}

impl LocalSpool {
    /// The spool under `dir`, created where absent.
    pub fn open(dir: &Path) -> Result<LocalSpool, Failure> {
        std::fs::create_dir_all(dir).map_err(|e| storage(dir, e))?;
        Ok(LocalSpool { dir: dir.to_path_buf(), counter: AtomicU64::new(0) })
    }

    fn lock(&self) -> Result<FileLock, Failure> {
        FileLock::acquire(&self.dir.join(LOCK_FILE))
    }

    fn file(&self, identity: &str) -> PathBuf {
        self.dir.join(format!("{}.json", sha256_hex(identity.as_bytes())))
    }

    fn body(&self, key: &str) -> PathBuf {
        self.dir.join(BODIES).join(format!("{}.body", sha256_hex(key.as_bytes())))
    }

    fn read(path: &Path) -> Result<Option<Spooled>, Failure> {
        match std::fs::read(path) {
            Ok(bytes) => serde_json::from_slice(&bytes).map(Some).map_err(|e| storage(path, e)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(storage(path, e)),
        }
    }
}

impl Queue for LocalSpool {
    fn send(&self, message: &QueueMessage) -> Result<(), Failure> {
        let _held = self.lock()?;
        let path = self.file(&message.identity());
        let spooled = Spooled { seq: sequence(&self.counter), message: message.clone() };
        replace(&path, &serde_json::to_vec(&spooled).map_err(|e| storage(&path, e))?)
    }

    /// The oldest `max` pending messages, by the sequence each was spooled under.
    fn receive(&self, max: usize) -> Result<Vec<Delivery>, Failure> {
        let mut pending = Vec::new();
        for entry in std::fs::read_dir(&self.dir).map_err(|e| storage(&self.dir, e))? {
            let path = entry.map_err(|e| storage(&self.dir, e))?.path();
            let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
            if name.starts_with('.') || !name.ends_with(".json") {
                continue;
            }
            if let Some(spooled) = Self::read(&path)? {
                pending.push((spooled.seq, name, spooled.message));
            }
        }
        pending.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.cmp(&b.1)));
        Ok(pending.into_iter().take(max).map(|(seq, name, message)| Delivery { receipt: format!("{name}@{seq}"), message }).collect())
    }

    fn ack(&self, receipt: &str) -> Result<(), Failure> {
        let Some((name, seq)) = receipt.rsplit_once('@') else { return Ok(()) };
        if name.contains(['/', '\\']) || name.starts_with('.') {
            return Ok(());
        }
        let _held = self.lock()?;
        let path = self.dir.join(name);
        match Self::read(&path)? {
            Some(spooled) if spooled.seq.to_string() == seq => std::fs::remove_file(&path).map_err(|e| storage(&path, e)),
            _ => Ok(()),
        }
    }

    fn message_cap(&self) -> usize {
        SPOOL_MESSAGE_CAP
    }
}

/// A spilled body sits beside the spool, `bodies/<digest of its key>.body`.
impl BodyStore for LocalSpool {
    fn put(&self, key: &str, body: &[u8]) -> Result<(), Failure> {
        let path = self.body(key);
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).map_err(|e| storage(dir, e))?;
        }
        replace(&path, body)
    }

    fn get(&self, key: &str) -> Result<Option<Vec<u8>>, Failure> {
        let path = self.body(key);
        match std::fs::read(&path) {
            Ok(bytes) => Ok(Some(bytes)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(storage(&path, e)),
        }
    }

    fn delete(&self, key: &str) -> Result<(), Failure> {
        let path = self.body(key);
        match std::fs::remove_file(&path) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(storage(&path, e)),
            _ => Ok(()),
        }
    }
}

/// An in-process queue with a managed queue's delivery: every send is its own message, a
/// received message redelivers until acknowledged, and nothing collapses a resend.
#[derive(Default)]
pub struct MemoryQueue {
    held: Mutex<(u64, Vec<(u64, QueueMessage)>)>,
    bodies: Mutex<std::collections::BTreeMap<String, Vec<u8>>>,
}

/// Spilled bodies held in process.
impl BodyStore for MemoryQueue {
    fn put(&self, key: &str, body: &[u8]) -> Result<(), Failure> {
        self.bodies.lock().unwrap_or_else(|e| e.into_inner()).insert(key.to_string(), body.to_vec());
        Ok(())
    }

    fn get(&self, key: &str) -> Result<Option<Vec<u8>>, Failure> {
        Ok(self.bodies.lock().unwrap_or_else(|e| e.into_inner()).get(key).cloned())
    }

    fn delete(&self, key: &str) -> Result<(), Failure> {
        self.bodies.lock().unwrap_or_else(|e| e.into_inner()).remove(key);
        Ok(())
    }
}

impl MemoryQueue {
    /// The messages not yet acknowledged, oldest first.
    pub fn pending(&self) -> Vec<QueueMessage> {
        self.held.lock().unwrap_or_else(|e| e.into_inner()).1.iter().map(|(_, m)| m.clone()).collect()
    }
}

impl Queue for MemoryQueue {
    fn send(&self, message: &QueueMessage) -> Result<(), Failure> {
        let mut held = self.held.lock().unwrap_or_else(|e| e.into_inner());
        held.0 += 1;
        let id = held.0;
        held.1.push((id, message.clone()));
        Ok(())
    }

    fn receive(&self, max: usize) -> Result<Vec<Delivery>, Failure> {
        let held = self.held.lock().unwrap_or_else(|e| e.into_inner());
        Ok(held.1.iter().take(max).map(|(id, message)| Delivery { receipt: id.to_string(), message: message.clone() }).collect())
    }

    fn ack(&self, receipt: &str) -> Result<(), Failure> {
        self.held.lock().unwrap_or_else(|e| e.into_inner()).1.retain(|(id, _)| id.to_string() != receipt);
        Ok(())
    }

    fn message_cap(&self) -> usize {
        SPOOL_MESSAGE_CAP
    }
}

/// One queued `receive` batch as a consumer lands it: its body read back, `None` when a
/// spilled body is gone because an earlier delivery's outcome recorded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Batch {
    pub pipeline: String,
    pub batch_key: String,
    pub credential: Option<String>,
    pub queued_at: i64,
    pub body: Option<Vec<u8>>,
    /// The route span the batch queued under (`disclosure.record.queued-parent`).
    pub trace: Option<Carrier>,
}

/// Queue `body` as a `receive` batch (`run.land.receive-queued-spill`): inline when the
/// message fits the queue's cap, else written to `bodies` under the batch key first and
/// referenced. The message carries `trace`, the route span's W3C carriers alone
/// (`disclosure.record.queued-content`).
#[allow(clippy::too_many_arguments)]
pub fn enqueue(queue: &dyn Queue, bodies: &dyn BodyStore, pipeline: &str, batch_key: &str, credential: Option<&str>, queued_at: i64, trace: Option<&Carrier>, body: &[u8]) -> Result<(), Failure> {
    let credential = credential.map(|c| contextful_core::run::queue::Credential(c.to_string()));
    let trace = trace.cloned();
    let inline = QueueMessage::Receive { pipeline: pipeline.into(), batch_key: batch_key.into(), credential: credential.clone(), queued_at, body: BodyRef::Inline { body: body.to_vec() }, trace: trace.clone() };
    if inline.encoded_len() <= queue.message_cap() {
        return queue.send(&inline);
    }
    let key = spill_key(pipeline, batch_key);
    bodies.put(&key, body)?;
    queue.send(&QueueMessage::Receive { pipeline: pipeline.into(), batch_key: batch_key.into(), credential, queued_at, body: BodyRef::Spilled { key }, trace })
}

/// The key a spilled body sits under: its pipeline and batch key, so a resend overwrites it.
fn spill_key(pipeline: &str, batch_key: &str) -> String {
    format!("receive/{pipeline}/{}", sha256_hex(batch_key.as_bytes()))
}

/// What a drained batch wakes or lands. Each wake reads from its own durable position and
/// receives no rows; a queued `receive` batch lands through the engine's receive group.
pub trait Wake {
    /// Run every export reading one of `tables` from its cursor (`run.export.queue-wake`).
    fn exports(&mut self, tables: &BTreeSet<String>) -> Result<(), Failure>;
    /// Fire every object read whose address covers one of `arrivals`, each `(bucket, key)`,
    /// from its held position (`run.land.object-arrival-wake`).
    fn arrivals(&mut self, arrivals: &BTreeSet<(String, String)>) -> Result<(), Failure>;
    /// Land `batches`, pushing what they commit, and answer each in order: `Ok` once its
    /// outcome records, committed or refused; a failure leaves it queued for redelivery
    /// (`run.land.receive-queued-consume`).
    fn batches(&mut self, batches: &[Batch]) -> Vec<Result<(), Failure>>;
    /// Whether this drain takes post-commit wake-ups; one that does not leaves each queued,
    /// unacknowledged and uncounted, for a drain that does (`surface.fire.cycle-drain`).
    fn wakes_exports(&self) -> bool {
        true
    }
}

/// One drain's tally: the deliveries each kind acknowledged, and those left queued.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Drained {
    pub wakes: usize,
    pub arrivals: usize,
    /// Queued `receive` batches whose outcome recorded.
    pub batches: usize,
    /// Deliveries whose wake or landing failed; each redelivers.
    pub left: usize,
}

/// Receive up to `max` deliveries, wake each read they name once, land the queued `receive`
/// batches, and acknowledge each delivery whose wake succeeded or whose outcome recorded,
/// deleting a settled batch's spilled body from `bodies` after its acknowledgement. A
/// failure leaves its deliveries queued and is returned after the other acknowledgements.
pub fn drain(queue: &dyn Queue, bodies: &dyn BodyStore, max: usize, wake: &mut dyn Wake) -> Result<Drained, Failure> {
    let deliveries = queue.receive(max)?;
    let (mut tables, mut objects) = (BTreeSet::new(), BTreeSet::new());
    let (mut wakes, mut arrivals, mut left) = (Vec::new(), Vec::new(), 0);
    let mut queued: Vec<(String, Option<String>, Batch)> = Vec::new();
    let mut failed = None;
    for d in deliveries {
        match d.message {
            QueueMessage::Committed { .. } if !wake.wakes_exports() => {}
            QueueMessage::Committed { table, .. } => {
                tables.insert(table);
                wakes.push(d.receipt);
            }
            QueueMessage::ObjectArrival { bucket, key } => {
                objects.insert((bucket, key));
                arrivals.push(d.receipt);
            }
            QueueMessage::Receive { pipeline, batch_key, credential, queued_at, body, trace } => {
                let (spill, body) = match body {
                    BodyRef::Inline { body } => (None, Ok(Some(body))),
                    BodyRef::Spilled { key } => {
                        let read = bodies.get(&key);
                        (Some(key), read)
                    }
                };
                match body {
                    Ok(body) => queued.push((d.receipt, spill, Batch { pipeline, batch_key, credential: credential.map(|c| c.0), queued_at, body, trace })),
                    Err(f) => {
                        left += 1;
                        failed.get_or_insert(f);
                    }
                }
            }
        }
    }
    // An arrival lands first, so the wake-up its commit sends finds the exports current.
    let landed = if objects.is_empty() { Ok(()) } else { wake.arrivals(&objects) };
    let arrivals = settle(queue, arrivals, landed, &mut left, &mut failed)?;
    let mut batches = 0;
    if !queued.is_empty() {
        let given: Vec<Batch> = queued.iter().map(|(_, _, b)| b.clone()).collect();
        let answers = wake.batches(&given);
        for (i, (receipt, spill, _)) in queued.into_iter().enumerate() {
            match answers.get(i).cloned().unwrap_or_else(|| Err(Failure::new(FailureTag::Transient, "the consumer answered no outcome for the batch"))) {
                Ok(()) => {
                    queue.ack(&receipt)?;
                    if let Some(key) = spill {
                        bodies.delete(&key)?;
                    }
                    batches += 1;
                }
                Err(f) => {
                    left += 1;
                    failed.get_or_insert(f);
                }
            }
        }
    }
    let delivered = if tables.is_empty() { Ok(()) } else { wake.exports(&tables) };
    let wakes = settle(queue, wakes, delivered, &mut left, &mut failed)?;
    match failed {
        Some(f) => Err(f),
        None => Ok(Drained { wakes, arrivals, batches, left }),
    }
}

/// Acknowledge `receipts` after a successful wake; count them as left after a failed one.
fn settle(queue: &dyn Queue, receipts: Vec<String>, result: Result<(), Failure>, left: &mut usize, failed: &mut Option<Failure>) -> Result<usize, Failure> {
    match result {
        Ok(()) => {
            for receipt in &receipts {
                queue.ack(receipt)?;
            }
            Ok(receipts.len())
        }
        Err(f) => {
            *left += receipts.len();
            failed.get_or_insert(f);
            Ok(0)
        }
    }
}
