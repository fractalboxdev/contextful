//! `disclosure.attest.root-replication` and `disclosure.attest.replica-verify`: signed
//! segment roots copied to a bucket on a tick, and a local chain checked against them.

use contextful_core::issue::SignatureAlgorithm;
use contextful_core::store::object::{Condition, ObjectError, ObjectStore, Put};
use contextful_policy::audit::{AuditError, AuditLog, AuditOptions, ChainHeader, SignedTip};
use contextful_policy::issue::{SeedSigner, SignerKey};
use contextful_policy::replica::{root_key, verify_replicated, RootBucket, RootReplicator, ROOT_REPLICATION_INTERVAL};
use serde_json::json;
use std::collections::BTreeMap;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

fn key() -> SeedSigner {
    SeedSigner::from_seed(&format!("ed25519-private/{}", hex::encode([7u8; 32]))).unwrap()
}

/// An in-memory bucket that answers every call with a transport failure while `down`.
#[derive(Default)]
struct FakeBucket {
    objects: Mutex<BTreeMap<String, Vec<u8>>>,
    down: AtomicBool,
}

impl FakeBucket {
    fn reachable(&self) -> Result<(), ObjectError> {
        if self.down.load(Ordering::SeqCst) {
            return Err(ObjectError::Transport("connection refused".into()));
        }
        Ok(())
    }

    fn keys(&self) -> Vec<String> {
        self.objects.lock().unwrap().keys().cloned().collect()
    }
}

impl ObjectStore for FakeBucket {
    fn get(&self, key: &str) -> Result<Option<(Vec<u8>, String)>, ObjectError> {
        self.reachable()?;
        Ok(self.objects.lock().unwrap().get(key).map(|b| (b.clone(), format!("{}", b.len()))))
    }

    fn put(&self, key: &str, bytes: &[u8], condition: Condition) -> Result<Put, ObjectError> {
        self.reachable()?;
        let mut objects = self.objects.lock().unwrap();
        if condition == Condition::IfNoneMatch && objects.contains_key(key) {
            return Ok(Put::ConditionFailed);
        }
        objects.insert(key.to_string(), bytes.to_vec());
        Ok(Put::Applied(format!("{}", bytes.len())))
    }

    fn delete(&self, key: &str) -> Result<(), ObjectError> {
        self.reachable()?;
        self.objects.lock().unwrap().remove(key);
        Ok(())
    }

    fn list(&self, prefix: &str) -> Result<Vec<String>, ObjectError> {
        self.reachable()?;
        Ok(self.objects.lock().unwrap().keys().filter(|k| k.starts_with(prefix)).cloned().collect())
    }
}

/// A held log under `segment_entries`-entry segments.
fn held(dir: &Path, segment_entries: u64) -> AuditLog<SeedSigner> {
    let options = AuditOptions { header: ChainHeader { segment_entries, ..ChainHeader::default() }, ..AuditOptions::default() };
    AuditLog::open_with(dir, key(), options).unwrap()
}

fn reads(n: u64) -> Vec<serde_json::Value> {
    (0..n).map(|i| json!({ "contextful.result.rows": i })).collect()
}

fn broken_at(r: Result<impl std::fmt::Debug, AuditError>) -> u64 {
    match r {
        Err(AuditError::AuditChainBroken { index, .. }) => index,
        other => panic!("expected AuditChainBroken, got {other:?}"),
    }
}

/// Wait up to `bound` for `done`.
fn within(bound: Duration, done: impl Fn() -> bool) -> bool {
    let start = Instant::now();
    while start.elapsed() < bound {
        if done() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    done()
}

#[test]
fn the_replication_tick_is_ten_minutes() {
    assert_eq!(ROOT_REPLICATION_INTERVAL, Duration::from_secs(600));
    assert_eq!(root_key(3), "audit/roots/000003.json");
}

/// A segment closing while the replicator runs reaches the bucket within one tick, under
/// the prefix, byte for byte the local root; the open segment's absent root sends nothing.
// spec: disclosure.attest.root-replication@82344430
#[test]
fn a_closed_segment_root_lands_within_one_tick() {
    let dir = tempfile::tempdir().unwrap();
    let bucket = Arc::new(FakeBucket::default());
    let tick = Duration::from_millis(100);
    let replicator = RootReplicator::spawn(dir.path(), RootBucket::new(bucket.clone(), "team/research/"), tick);
    let log = held(dir.path(), 4);
    log.append_all(reads(5)).unwrap();
    assert!(within(tick * 3, || bucket.keys() == ["team/research/audit/roots/000001.json"]), "{:?}", bucket.keys());
    let local = std::fs::read(dir.path().join("segments/000001.root.json")).unwrap();
    assert_eq!(bucket.objects.lock().unwrap()["team/research/audit/roots/000001.json"], local);
    drop(replicator);
}

/// With the bucket down, appends keep committing; the failed push retries on the next tick
/// once the bucket answers.
#[test]
fn a_bucket_outage_blocks_no_append_and_the_next_tick_retries() {
    let dir = tempfile::tempdir().unwrap();
    let bucket = Arc::new(FakeBucket::default());
    bucket.down.store(true, Ordering::SeqCst);
    let tick = Duration::from_millis(50);
    let replicator = RootReplicator::spawn(dir.path(), RootBucket::new(bucket.clone(), ""), tick);
    let log = held(dir.path(), 2);
    for i in 0..6 {
        log.append(json!({ "contextful.result.rows": i })).unwrap();
    }
    std::thread::sleep(tick * 3);
    assert_eq!(log.tip().seq, 6);
    assert!(bucket.objects.lock().unwrap().is_empty());
    bucket.down.store(false, Ordering::SeqCst);
    assert!(within(tick * 4, || bucket.keys().len() == 3), "{:?}", bucket.keys());
    drop(replicator);
}

/// A push sends only the roots the bucket lacks, and reports what it sent.
#[test]
fn a_push_sends_only_the_missing_roots() {
    let dir = tempfile::tempdir().unwrap();
    held(dir.path(), 2).append_all(reads(4)).unwrap();
    let bucket = RootBucket::new(Arc::new(FakeBucket::default()), "p/");
    assert_eq!(bucket.push(dir.path()).unwrap(), [1, 2]);
    assert_eq!(bucket.push(dir.path()).unwrap(), Vec::<u64>::new());
    assert_eq!(bucket.roots().unwrap().keys().copied().collect::<Vec<_>>(), [1, 2]);
}

/// A chain whose roots, tip and held record are deleted reads as unanchored to every local
/// check, and breaks against the replicated roots at the segment they close.
// spec: disclosure.attest.replica-verify@fff89422
#[test]
fn deleted_roots_and_tip_break_against_the_replica() {
    let dir = tempfile::tempdir().unwrap();
    let log = held(dir.path(), 2);
    log.append_all(reads(5)).unwrap();
    drop(log);
    let bucket = RootBucket::new(Arc::new(FakeBucket::default()), "");
    bucket.push(dir.path()).unwrap();
    let replicated = bucket.roots().unwrap();
    let k = SignerKey::of(&key());
    assert_eq!(verify_replicated(dir.path(), &replicated, Some(&k)).unwrap().seq, 5);

    for n in 1..=2 {
        std::fs::remove_file(dir.path().join(format!("segments/{n:06}.root.json"))).unwrap();
    }
    std::fs::remove_file(dir.path().join("chain.held")).unwrap();
    let tip = std::fs::read_to_string(dir.path().join("chain.tip")).unwrap();
    let mut tip: serde_json::Value = serde_json::from_str(&tip).unwrap();
    tip.as_object_mut().unwrap().remove("signature");
    std::fs::write(dir.path().join("chain.tip"), tip.to_string()).unwrap();
    assert_eq!(contextful_policy::audit::verify(dir.path()).unwrap().seq, 5, "locally the chain reads unanchored");
    assert_eq!(broken_at(verify_replicated(dir.path(), &replicated, None)), 2);
}

/// A local chain truncated behind a replicated root breaks at the first missing entry.
#[test]
fn a_truncated_chain_breaks_at_its_first_missing_entry() {
    let dir = tempfile::tempdir().unwrap();
    held(dir.path(), 2).append_all(reads(4)).unwrap();
    let bucket = RootBucket::new(Arc::new(FakeBucket::default()), "");
    bucket.push(dir.path()).unwrap();
    let replicated = bucket.roots().unwrap();
    for f in ["segments/000002.jsonl", "segments/000002.root.json"] {
        std::fs::remove_file(dir.path().join(f)).unwrap();
    }
    let tip = contextful_policy::audit::entries(dir.path()).unwrap()[1].clone();
    let signed = SignedTip::sign(tip.seq, &tip.entry_hash, &key()).unwrap();
    std::fs::write(dir.path().join("chain.tip"), serde_json::to_string(&signed).unwrap()).unwrap();
    std::fs::remove_file(dir.path().join("chain.held")).unwrap();
    assert_eq!(broken_at(verify_replicated(dir.path(), &replicated, None)), 3);
}

/// A replicated root that does not verify under the key breaks the chain at its segment.
#[test]
fn a_replicated_root_under_another_key_breaks() {
    let dir = tempfile::tempdir().unwrap();
    held(dir.path(), 2).append_all(reads(2)).unwrap();
    let bucket = RootBucket::new(Arc::new(FakeBucket::default()), "");
    bucket.push(dir.path()).unwrap();
    let other = SeedSigner::generate(SignatureAlgorithm::Ed25519);
    assert_eq!(broken_at(verify_replicated(dir.path(), &bucket.roots().unwrap(), Some(&SignerKey::of(&other)))), 2);
}
