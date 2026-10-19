//! The engine's side of the `Queue` port (`topology.compose.queue-extension`): the local
//! spool every profile without a `queue` role uses, an in-process queue for tests, and the
//! drain that routes received hints to the reads they wake.
//!
//! A consumer only wakes: an export reads from its own cursor and an object read lists
//! from its held position, so the store's writer stays the engine's landing path.

use crate::fsutil::{replace, FileLock};
use contextful_core::run::journal::sha256_hex;
use contextful_core::run::queue::{Delivery, Queue, QueueMessage};
use contextful_core::run::{Failure, FailureTag};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;

/// The lock file serializing a spool's sends and acknowledgements.
const LOCK_FILE: &str = "spool.lock";

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
}

/// An in-process queue with a managed queue's delivery: every send is its own message, a
/// received message redelivers until acknowledged, and nothing collapses a resend.
#[derive(Default)]
pub struct MemoryQueue {
    held: Mutex<(u64, Vec<(u64, QueueMessage)>)>,
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
}

/// What a drained batch wakes. Each call reads from its own durable position; neither
/// receives rows.
pub trait Wake {
    /// Run every export reading one of `tables` from its cursor (`run.export.queue-wake`).
    fn exports(&mut self, tables: &BTreeSet<String>) -> Result<(), Failure>;
    /// Fire every object read whose address covers one of `arrivals`, each `(bucket, key)`,
    /// from its held position (`run.land.object-arrival-wake`).
    fn arrivals(&mut self, arrivals: &BTreeSet<(String, String)>) -> Result<(), Failure>;
}

/// One drain's tally: the deliveries each kind acknowledged, and those left queued.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Drained {
    pub wakes: usize,
    pub arrivals: usize,
    /// Deliveries no wake consumes, or whose wake failed; each redelivers.
    pub left: usize,
}

/// Receive up to `max` deliveries, wake each read they name once, and acknowledge the
/// deliveries whose wake succeeded. A failed wake leaves its deliveries queued and its
/// failure is returned after the other kind's acknowledgements; a `receive` batch stays
/// queued for the landing consumer.
pub fn drain(queue: &dyn Queue, max: usize, wake: &mut dyn Wake) -> Result<Drained, Failure> {
    let deliveries = queue.receive(max)?;
    let (mut tables, mut objects) = (BTreeSet::new(), BTreeSet::new());
    let (mut wakes, mut arrivals, mut left) = (Vec::new(), Vec::new(), 0);
    for d in deliveries {
        match &d.message {
            QueueMessage::Committed { table, .. } => {
                tables.insert(table.clone());
                wakes.push(d.receipt);
            }
            QueueMessage::ObjectArrival { bucket, key } => {
                objects.insert((bucket.clone(), key.clone()));
                arrivals.push(d.receipt);
            }
            QueueMessage::Receive { .. } => left += 1,
        }
    }
    let mut failed = None;
    // An arrival lands first, so the wake-up its commit sends finds the exports current.
    let landed = if objects.is_empty() { Ok(()) } else { wake.arrivals(&objects) };
    let arrivals = settle(queue, arrivals, landed, &mut left, &mut failed)?;
    let delivered = if tables.is_empty() { Ok(()) } else { wake.exports(&tables) };
    let wakes = settle(queue, wakes, delivered, &mut left, &mut failed)?;
    match failed {
        Some(f) => Err(f),
        None => Ok(Drained { wakes, arrivals, left }),
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
