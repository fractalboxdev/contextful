//! `disclosure.attest.root-replication` and `disclosure.attest.replica-verify`: the chain's
//! signed segment roots, copied off-node to `audit/roots/<segment>.json` in a bucket, and a
//! local chain checked against that copy.
//!
//! A root is written once: a push puts it under `If-None-Match`, so a key already holding
//! another root for its segment is a disagreement, never an overwrite. The replicator runs
//! on its own thread and shares no lock with an append, so a bucket that does not answer
//! delays only the next copy (`disclosure.attest.root-replication`).
//!
//! The copy catches what no local check can: a chain whose roots, `chain.held` and tip
//! signature were deleted reads as unanchored, and a chain cut behind a replicated root
//! reads as complete under a rewritten tip. Against the replica, either breaks at the
//! earliest index the copy covers and the chain does not.

use crate::audit::{self, broken, first_seq, read_root, AuditError, ChainTip, SignedRoot};
use crate::issue::SignerKey;
use contextful_core::store::object::{Condition, ObjectError, ObjectStore, Put};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

/// How often the replicator copies newly signed roots off-node
/// (`disclosure.attest.root-replication`).
pub const ROOT_REPLICATION_INTERVAL: Duration = Duration::from_secs(600);

/// Where replicated roots sit under a project's keyspace.
pub const ROOTS_PREFIX: &str = "audit/roots/";

/// The key, under a project's keyspace, of segment `segment`'s replicated root.
pub fn root_key(segment: u64) -> String {
    format!("{ROOTS_PREFIX}{segment:06}.json")
}

fn bucket_error(e: ObjectError) -> AuditError {
    AuditError::Io(format!("replication bucket: {e}"))
}

/// One project's root keyspace in a bucket.
#[derive(Clone)]
pub struct RootBucket {
    bucket: Arc<dyn ObjectStore>,
    /// Prepended to every key: the sync prefix and the project, ending in `/`, or empty.
    prefix: String,
}

impl RootBucket {
    pub fn new(bucket: Arc<dyn ObjectStore>, prefix: impl Into<String>) -> RootBucket {
        RootBucket { bucket, prefix: prefix.into() }
    }

    fn key(&self, segment: u64) -> String {
        format!("{}{}", self.prefix, root_key(segment))
    }

    /// The segment numbers holding a replicated root.
    fn listed(&self) -> Result<Vec<u64>, AuditError> {
        let under = format!("{}{ROOTS_PREFIX}", self.prefix);
        let keys = self.bucket.list(&under).map_err(bucket_error)?;
        Ok(keys.iter().filter_map(|k| k.strip_prefix(&under)?.strip_suffix(".json")?.parse().ok()).collect())
    }

    /// Copy each signed root of the chain at `dir` the bucket lacks; returns the segments
    /// sent. A key holding another root for its segment raises `AuditChainBroken` at the
    /// segment's closing index.
    pub fn push(&self, dir: &Path) -> Result<Vec<u64>, AuditError> {
        let have = self.listed()?;
        let size = audit::segment_size(dir)?;
        let mut sent = Vec::new();
        for n in audit::root_segments(dir)?.into_iter().filter(|n| !have.contains(n)) {
            let Some(bytes) = read_root(dir, n)? else { continue };
            let key = self.key(n);
            match self.bucket.put(&key, &bytes, Condition::IfNoneMatch).map_err(bucket_error)? {
                Put::Applied(_) => sent.push(n),
                Put::ConditionFailed => {
                    let held = self.bucket.get(&key).map_err(bucket_error)?.map(|(b, _)| b);
                    if held.as_deref() != Some(bytes.as_slice()) {
                        return Err(broken(n * size, format!("`{key}` holds another root for segment {n}")));
                    }
                }
            }
        }
        Ok(sent)
    }

    /// Every replicated root, by segment.
    pub fn roots(&self) -> Result<BTreeMap<u64, SignedRoot>, AuditError> {
        let mut out = BTreeMap::new();
        for n in self.listed()? {
            let key = self.key(n);
            let Some((bytes, _)) = self.bucket.get(&key).map_err(bucket_error)? else { continue };
            let root: SignedRoot =
                serde_json::from_slice(&bytes).map_err(|e| AuditError::Io(format!("replication bucket: `{key}` does not parse: {e}")))?;
            out.insert(n, root);
        }
        Ok(out)
    }
}

/// Every local check of the chain at `dir` — under `key`, every signature too — then each
/// replicated root against it: the root verifies under `key`, the chain reaches the
/// segment it closes, and the local root equals it. The first disagreement raises
/// `AuditChainBroken` at the earliest index it covers (`disclosure.attest.replica-verify`).
pub fn verify_replicated(dir: &Path, replicated: &BTreeMap<u64, SignedRoot>, key: Option<&SignerKey>) -> Result<ChainTip, AuditError> {
    let end = match key {
        Some(k) => audit::verify_signed(dir, k)?,
        None => audit::verify(dir)?,
    };
    let size = audit::segment_size(dir)?;
    for (&n, root) in replicated {
        let closing = n * size;
        if key.is_some_and(|k| !root.verify(k)) {
            return Err(broken(closing, format!("the replicated root of segment {n} does not verify")));
        }
        if end.seq < closing {
            return Err(broken(end.seq.max(first_seq(n, size) - 1) + 1, format!("the chain ends at {} and a replicated root closes segment {n}", end.seq)));
        }
        let local = match read_root(dir, n)? {
            Some(bytes) => serde_json::from_slice::<SignedRoot>(&bytes).ok(),
            None => return Err(broken(closing, format!("segment {n} carries no signed root and the replica holds one"))),
        };
        if local.as_ref() != Some(root) {
            return Err(broken(closing, format!("the signed root of segment {n} disagrees with its replica")));
        }
    }
    Ok(end)
}

struct Stop {
    stopped: Mutex<bool>,
    wake: Condvar,
}

/// The background copy: pushes the chain's new roots once on spawn and again every
/// interval, until dropped. A failed push is reported on stderr and retried on the next
/// tick.
pub struct RootReplicator {
    stop: Arc<Stop>,
    thread: Option<JoinHandle<()>>,
}

impl RootReplicator {
    pub fn spawn(dir: impl Into<PathBuf>, bucket: RootBucket, interval: Duration) -> RootReplicator {
        let dir = dir.into();
        let stop = Arc::new(Stop { stopped: Mutex::new(false), wake: Condvar::new() });
        let signal = stop.clone();
        let thread = std::thread::spawn(move || loop {
            if let Err(e) = bucket.push(&dir) {
                eprintln!("root replication: {e}; retrying in {}s", interval.as_secs_f64());
            }
            let stopped = signal.stopped.lock().unwrap_or_else(|p| p.into_inner());
            let (stopped, _) = signal.wake.wait_timeout_while(stopped, interval, |s| !*s).unwrap_or_else(|p| p.into_inner());
            if *stopped {
                break;
            }
        });
        RootReplicator { stop, thread: Some(thread) }
    }
}

impl Drop for RootReplicator {
    fn drop(&mut self) {
        *self.stop.stopped.lock().unwrap_or_else(|p| p.into_inner()) = true;
        self.stop.wake.notify_all();
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}
