//! `disclosure.record` and `disclosure.attest`: the hash-linked chain, its numbered
//! segments, the chain tip, the signed root closing each segment, and verification.

use contextful_core::issue::SignatureAlgorithm;
use contextful_policy::audit::{
    query_digest, verify, verify_signed, AuditEntry, AuditError, AuditLog, AuditOptions, FileFsync, Fsync, FsyncPort, SignedRoot, SignedTip,
    AUDIT_SEGMENT_ENTRIES, AUDIT_TIP_IDLE, GENESIS,
};
use contextful_policy::issue::{SeedSigner, SignerKey};
use serde_json::{json, Value};
use std::fs::{self, File};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

/// A deterministic key under `algorithm`, its private scalar every byte `byte`.
fn seeded(byte: u8, algorithm: SignatureAlgorithm) -> SeedSigner {
    let scheme = match algorithm {
        SignatureAlgorithm::Ed25519 => "ed25519",
        SignatureAlgorithm::Es256 => "secp256r1",
    };
    SeedSigner::from_seed(&format!("{scheme}-private/{}", hex::encode([byte; 32]))).unwrap()
}

fn key() -> SeedSigner {
    seeded(7, SignatureAlgorithm::Ed25519)
}

fn attrs(agent: &str, rows: u64) -> Value {
    json!({ "contextful.subject.agent": agent, "contextful.result.rows": rows })
}

fn segment(dir: &Path, n: u64) -> PathBuf {
    dir.join("segments").join(format!("{n:06}.jsonl"))
}

fn root_file(dir: &Path, n: u64) -> PathBuf {
    dir.join("segments").join(format!("{n:06}.root.json"))
}

fn lines(path: &Path) -> Vec<AuditEntry> {
    fs::read_to_string(path).unwrap().lines().map(|l| serde_json::from_str(l).unwrap()).collect()
}

fn write_lines(path: &Path, entries: &[AuditEntry]) {
    let text: String = entries.iter().map(|e| serde_json::to_string(e).unwrap() + "\n").collect();
    fs::write(path, text).unwrap();
}

/// A log of `n` entries in a fresh directory.
fn log_of(n: u64) -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    let log = AuditLog::open(dir.path(), key()).unwrap();
    for i in 0..n {
        log.append(attrs("agent://a", i)).unwrap();
    }
    dir
}

fn broken_at(r: Result<impl std::fmt::Debug, AuditError>) -> u64 {
    match r {
        Err(AuditError::AuditChainBroken { index, .. }) => index,
        other => panic!("expected AuditChainBroken, got {other:?}"),
    }
}

#[test]
fn entries_link_from_genesis_with_seq_starting_at_one() {
    let dir = log_of(3);
    let entries = lines(&segment(dir.path(), 1));
    assert_eq!(entries.iter().map(|e| e.seq).collect::<Vec<_>>(), [1, 2, 3]);
    assert_eq!(entries[0].prev_hash, GENESIS);
    assert_eq!(entries[1].prev_hash, entries[0].entry_hash);
    assert_eq!(entries[2].prev_hash, entries[1].entry_hash);
    assert!(entries.iter().all(|e| e.entry_hash.starts_with("sha256:")));
    let end = verify(dir.path()).unwrap();
    assert_eq!(end.seq, 3);
    assert_eq!(end.entry_hash, entries[2].entry_hash);
    let tip: SignedTip = serde_json::from_str(&fs::read_to_string(dir.path().join("chain.tip")).unwrap()).unwrap();
    assert_eq!((tip.seq, tip.entry_hash.as_str()), (3, entries[2].entry_hash.as_str()));
    assert!(tip.verify(&SignerKey::of(&key())));
}

#[test]
fn the_entry_digest_is_independent_of_attribute_order() {
    let a: Value = serde_json::from_str(r#"{"contextful.result.rows":1,"contextful.tables":["orders"]}"#).unwrap();
    let b: Value = serde_json::from_str(r#"{"contextful.tables":["orders"],"contextful.result.rows":1}"#).unwrap();
    assert_eq!(AuditEntry::link(1, GENESIS, a).entry_hash, AuditEntry::link(1, GENESIS, b).entry_hash);
}

// spec: disclosure.record.segment@10125400
#[test]
fn a_segment_closes_at_4096_entries_under_one_signed_root() {
    let dir = tempfile::tempdir().unwrap();
    let log = AuditLog::open(dir.path(), key()).unwrap();
    let batch: Vec<Value> = (0..AUDIT_SEGMENT_ENTRIES + 1).map(|i| attrs("agent://a", i)).collect();
    log.append_all(batch).unwrap();

    let first = lines(&segment(dir.path(), 1));
    assert_eq!(first.len() as u64, AUDIT_SEGMENT_ENTRIES);
    assert!(first.windows(2).all(|w| w[1].seq == w[0].seq + 1), "seq ascends within the segment");
    assert_eq!(first[0].seq, 1);

    let root: SignedRoot = serde_json::from_str(&fs::read_to_string(root_file(dir.path(), 1)).unwrap()).unwrap();
    assert_eq!(root.count, AUDIT_SEGMENT_ENTRIES);
    assert_eq!(root.root, first.last().unwrap().entry_hash);
    assert!(root.verify(&SignerKey::of(&key())));

    let second = lines(&segment(dir.path(), 2));
    assert_eq!(second.iter().map(|e| e.seq).collect::<Vec<_>>(), [AUDIT_SEGMENT_ENTRIES + 1]);
    assert_eq!(second[0].prev_hash, root.root);
    assert!(!root_file(dir.path(), 2).exists(), "an open segment carries no root");
    assert_eq!(verify_signed(dir.path(), &SignerKey::of(&key())).unwrap().seq, AUDIT_SEGMENT_ENTRIES + 1);
}

/// A disagreeing digest, a sequence gap, an absent chain beside `chain.tip` or a signed root, or, under the
/// signed check opening a log runs, an absent or unverified tip or root raises `AuditChainBroken` at the earliest
/// failing index.
// spec: disclosure.attest.broken-chain@0683f91b
#[test]
fn a_disagreeing_digest_a_gap_or_a_vanished_chain_raises_audit_chain_broken() {
    // A disagreeing digest: entry 2's attributes rewritten after the fact.
    let dir = log_of(3);
    let mut entries = lines(&segment(dir.path(), 1));
    entries[1].attributes = attrs("agent://attacker", 999);
    write_lines(&segment(dir.path(), 1), &entries);
    assert_eq!(broken_at(verify(dir.path())), 2);

    // A sequence gap: entry 2 removed.
    let dir = log_of(3);
    let mut entries = lines(&segment(dir.path(), 1));
    entries.remove(1);
    write_lines(&segment(dir.path(), 1), &entries);
    assert_eq!(broken_at(verify(dir.path())), 2);

    // An absent chain while `chain.tip` exists.
    let dir = log_of(3);
    fs::remove_dir_all(dir.path().join("segments")).unwrap();
    assert_eq!(broken_at(verify(dir.path())), 1);

    // An absent chain while a signed root exists.
    let dir = tempfile::tempdir().unwrap();
    AuditLog::open(dir.path(), key()).unwrap().append_all((0..AUDIT_SEGMENT_ENTRIES).map(|i| attrs("agent://a", i)).collect()).unwrap();
    fs::remove_file(segment(dir.path(), 1)).unwrap();
    fs::remove_file(dir.path().join("chain.tip")).unwrap();
    assert!(root_file(dir.path(), 1).exists());
    assert_eq!(broken_at(verify(dir.path())), 1);

    // Opening a broken chain refuses the same way.
    assert_eq!(broken_at(AuditLog::open(dir.path(), key())), 1);

    // A truncated segment under a rewritten, unsigned tip.
    let dir = log_of(10);
    let mut entries = lines(&segment(dir.path(), 1));
    entries.truncate(7);
    write_lines(&segment(dir.path(), 1), &entries);
    fs::write(dir.path().join("chain.tip"), json!({ "seq": 7, "entry_hash": entries[6].entry_hash }).to_string()).unwrap();
    assert_eq!(verify(dir.path()).unwrap().seq, 7, "the unsigned walk cannot see a truncation");
    assert_eq!(broken_at(verify_signed(dir.path(), &SignerKey::of(&key()))), 7);
    assert_eq!(broken_at(AuditLog::open(dir.path(), key())), 7);

    // A tip signed under another key, and a chain with no tip at all.
    let dir = log_of(3);
    let entries = lines(&segment(dir.path(), 1));
    let forged = SignedTip::sign(3, &entries[2].entry_hash, &seeded(9, SignatureAlgorithm::Ed25519)).unwrap();
    fs::write(dir.path().join("chain.tip"), serde_json::to_string(&forged).unwrap()).unwrap();
    assert_eq!(broken_at(AuditLog::open(dir.path(), key())), 3);
    fs::remove_file(dir.path().join("chain.tip")).unwrap();
    assert_eq!(broken_at(verify_signed(dir.path(), &SignerKey::of(&key()))), 1);

    // A closed segment rewritten wholesale, re-linked, under a root with a garbage signature.
    let dir = tempfile::tempdir().unwrap();
    AuditLog::open(dir.path(), key()).unwrap().append_all((0..AUDIT_SEGMENT_ENTRIES + 1).map(|i| attrs("agent://a", i)).collect()).unwrap();
    let mut prev = GENESIS.to_string();
    let forged: Vec<AuditEntry> = lines(&segment(dir.path(), 1))
        .into_iter()
        .map(|e| {
            let f = AuditEntry::link(e.seq, &prev, attrs("agent://attacker", e.seq));
            prev = f.entry_hash.clone();
            f
        })
        .collect();
    write_lines(&segment(dir.path(), 1), &forged);
    let root = SignedRoot { root: prev.clone(), count: AUDIT_SEGMENT_ENTRIES, signature: "00".repeat(64) };
    fs::write(root_file(dir.path(), 1), serde_json::to_string(&root).unwrap()).unwrap();
    let mut second = lines(&segment(dir.path(), 2));
    second[0] = AuditEntry::link(second[0].seq, &prev, second[0].attributes.clone());
    write_lines(&segment(dir.path(), 2), &second);
    fs::write(dir.path().join("chain.tip"), json!({ "seq": second[0].seq, "entry_hash": second[0].entry_hash }).to_string()).unwrap();
    assert!(verify(dir.path()).is_ok(), "the unsigned walk accepts the forged history");
    assert_eq!(broken_at(AuditLog::open(dir.path(), key())), AUDIT_SEGMENT_ENTRIES);
}

/// One process holds a directory's audit log at a time; opening a log another holds raises `AuditLogHeld`.
// spec: disclosure.record.single-writer@f85b6bac
#[test]
fn a_second_writer_on_one_directory_is_refused() {
    let dir = log_of(2);
    let first = AuditLog::open(dir.path(), key()).unwrap();
    match AuditLog::open(dir.path(), key()) {
        Err(AuditError::AuditLogHeld(m)) => assert!(m.contains("audit.lock"), "{m}"),
        other => panic!("expected AuditLogHeld, got {other:?}"),
    }
    first.append(attrs("agent://a", 3)).unwrap();
    drop(first);
    let second = AuditLog::open(dir.path(), key()).unwrap();
    assert_eq!(second.append(attrs("agent://b", 4)).unwrap().seq, 4);
    assert_eq!(verify_signed(dir.path(), &SignerKey::of(&key())).unwrap().seq, 4);
}

#[test]
fn every_trailing_truncation_under_a_rewritten_tip_is_detected() {
    const N: u64 = 40;
    let dir = log_of(N);
    let full = lines(&segment(dir.path(), 1));
    let foreign = seeded(9, SignatureAlgorithm::Ed25519);
    let (mut undetected, mut cases) = (0u64, 0u64);
    for k in 1..N {
        let kept = &full[..(N - k) as usize];
        let end = kept.last().unwrap();
        write_lines(&segment(dir.path(), 1), kept);
        let unsigned = json!({ "seq": end.seq, "entry_hash": end.entry_hash }).to_string();
        let forged = serde_json::to_string(&SignedTip::sign(end.seq, &end.entry_hash, &foreign).unwrap()).unwrap();
        for tip in [unsigned, forged] {
            fs::write(dir.path().join("chain.tip"), tip).unwrap();
            cases += 1;
            match verify_signed(dir.path(), &SignerKey::of(&key())) {
                Err(AuditError::AuditChainBroken { .. }) => {}
                other => {
                    undetected += 1;
                    eprintln!("{k} trailing entries removed went undetected: {other:?}");
                }
            }
        }
    }
    assert_eq!(broken_at(AuditLog::open(dir.path(), key())), 1, "opening runs the signed check");
    contextful_eval::record::emit("audit-truncation-detected", undetected as f64, cases, 0);
    assert_eq!(undetected, 0);
}

#[test]
fn reordered_entries_break_the_chain_at_the_first_moved_entry() {
    let dir = log_of(3);
    let mut entries = lines(&segment(dir.path(), 1));
    entries.swap(1, 2);
    write_lines(&segment(dir.path(), 1), &entries);
    assert_eq!(broken_at(verify(dir.path())), 2);
}

#[test]
fn a_tip_beyond_the_chain_end_or_disagreeing_with_its_entry_breaks_the_chain() {
    let dir = log_of(3);
    let mut entries = lines(&segment(dir.path(), 1));
    entries.pop();
    write_lines(&segment(dir.path(), 1), &entries);
    assert_eq!(broken_at(verify(dir.path())), 3);

    let dir = log_of(3);
    fs::write(dir.path().join("chain.tip"), json!({ "seq": 2, "entry_hash": GENESIS }).to_string()).unwrap();
    assert_eq!(broken_at(verify(dir.path())), 2);
}

#[test]
fn a_root_under_a_foreign_key_or_over_another_digest_fails_signed_verification() {
    let dir = tempfile::tempdir().unwrap();
    AuditLog::open(dir.path(), key()).unwrap().append_all((0..AUDIT_SEGMENT_ENTRIES).map(|i| attrs("agent://a", i)).collect()).unwrap();
    let foreign = SignerKey::of(&seeded(9, SignatureAlgorithm::Ed25519));
    assert_eq!(broken_at(verify_signed(dir.path(), &foreign)), AUDIT_SEGMENT_ENTRIES);

    let path = root_file(dir.path(), 1);
    let mut root: SignedRoot = serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
    root.root = GENESIS.to_string();
    assert!(!root.verify(&SignerKey::of(&key())));
    fs::write(&path, serde_json::to_string(&root).unwrap()).unwrap();
    assert_eq!(broken_at(verify(dir.path())), AUDIT_SEGMENT_ENTRIES);
}

#[test]
fn roots_and_tips_sign_through_the_signing_port_under_either_scheme() {
    for algorithm in [SignatureAlgorithm::Ed25519, SignatureAlgorithm::Es256] {
        let dir = tempfile::tempdir().unwrap();
        let log = AuditLog::open(dir.path(), seeded(7, algorithm)).unwrap();
        log.append_all((0..AUDIT_SEGMENT_ENTRIES + 1).map(|i| attrs("agent://a", i)).collect()).unwrap();
        drop(log);
        let key = SignerKey::of(&seeded(7, algorithm));
        let root: SignedRoot = serde_json::from_str(&fs::read_to_string(root_file(dir.path(), 1)).unwrap()).unwrap();
        assert!(root.verify(&key), "{algorithm} root");
        assert_eq!(verify_signed(dir.path(), &key).unwrap().seq, AUDIT_SEGMENT_ENTRIES + 1);
        // A reopen verifies under the port's key and continues.
        assert_eq!(AuditLog::open(dir.path(), seeded(7, algorithm)).unwrap().append(attrs("agent://b", 0)).unwrap().seq, AUDIT_SEGMENT_ENTRIES + 2);
        // Another key, under either scheme, verifies nothing.
        for other in [SignatureAlgorithm::Ed25519, SignatureAlgorithm::Es256] {
            assert_eq!(broken_at(verify_signed(dir.path(), &SignerKey::of(&seeded(9, other)))), AUDIT_SEGMENT_ENTRIES);
        }
    }
}

#[test]
fn a_reopened_log_continues_the_linkage_from_its_tail() {
    let dir = log_of(2);
    let before = verify(dir.path()).unwrap();
    let log = AuditLog::open(dir.path(), key()).unwrap();
    assert_eq!(log.tip(), before);
    let next = log.append(attrs("agent://c", 3)).unwrap();
    assert_eq!(next.seq, 3);
    assert_eq!(next.prev_hash, before.entry_hash);
    assert_eq!(verify(dir.path()).unwrap().seq, 3);
}

#[test]
fn an_append_that_fails_to_persist_leaves_disk_and_tip_at_the_prior_entry() {
    let dir = tempfile::tempdir().unwrap();
    let probe = Arc::new(Probe::default());
    let log = AuditLog::open_with(dir.path(), key(), quiet(&probe)).unwrap();
    log.append(attrs("agent://a", 1)).unwrap();
    let before = log.tip();

    probe.fail_at(probe.segment_syncs());
    match log.append(attrs("agent://b", 2)) {
        Err(AuditError::AuditEntryUnpersisted(_)) => {}
        other => panic!("expected AuditEntryUnpersisted, got {other:?}"),
    }
    assert_eq!(log.tip(), before);
    assert_eq!(lines(&segment(dir.path(), 1)).len(), 1, "the unpersisted line is truncated away");

    let next = log.append(attrs("agent://b", 2)).unwrap();
    assert_eq!(next.seq, 2);
    assert_eq!(next.prev_hash, before.entry_hash);
    drop(log);
    // Two entries were acknowledged, the log's first and this one; verification reaches both.
    let lost = 2 - verify_signed(dir.path(), &SignerKey::of(&key())).unwrap().seq.min(2);
    contextful_eval::record::emit("audit-append-durable", lost as f64, 2, 0);
    assert_eq!(lost, 0);
}

#[test]
fn the_query_digest_is_keyed_under_the_audit_key() {
    let d = query_digest(b"audit-key", "select * from orders");
    assert!(d.starts_with("hmac-sha256:"));
    assert_eq!(d.len(), "hmac-sha256:".len() + 64);
    assert_eq!(d, query_digest(b"audit-key", "select * from orders"));
    assert_ne!(d, query_digest(b"other-key", "select * from orders"));
    assert_ne!(d, query_digest(b"audit-key", "select * from customers"));
}

#[test]
fn a_float_attribute_verifies_after_the_line_is_read_back() {
    let dir = tempfile::tempdir().unwrap();
    let log = AuditLog::open(dir.path(), key()).unwrap();
    let batch: Vec<Value> = (1..2000).map(|i| json!({ "contextful.duration_ms": f64::from(i) / 7.0 })).collect();
    log.append_all(batch).unwrap();
    log.append(json!({ "contextful.duration_ms": 632.0 / 7.0 })).unwrap();
    drop(log);
    assert_eq!(verify(dir.path()).unwrap().seq, 2000);
    assert_eq!(AuditLog::open(dir.path(), key()).unwrap().tip().seq, 2000);
}

#[test]
fn a_torn_unterminated_tail_past_the_tip_is_dropped_on_open() {
    let dir = log_of(2);
    let before = verify(dir.path()).unwrap();
    // A crash inside the write leaves part of entry 3 with no newline.
    let mut f = fs::OpenOptions::new().append(true).open(segment(dir.path(), 1)).unwrap();
    std::io::Write::write_all(&mut f, br#"{"seq":3,"prev_ha"#).unwrap();
    drop(f);
    let log = AuditLog::open(dir.path(), key()).unwrap();
    assert_eq!(log.tip(), before);
    assert_eq!(log.append(attrs("agent://c", 3)).unwrap().seq, 3);
    assert_eq!(verify_signed(dir.path(), &SignerKey::of(&key())).unwrap().seq, 3);

    // A malformed line that ends in a newline is no torn write, and still breaks the chain.
    let dir = log_of(2);
    let mut f = fs::OpenOptions::new().append(true).open(segment(dir.path(), 1)).unwrap();
    std::io::Write::write_all(&mut f, b"{\"seq\":3}\n").unwrap();
    drop(f);
    assert_eq!(broken_at(AuditLog::open(dir.path(), key())), 3);
}

/// Records every sync the log issues, in order. A held probe parks each segment sync until
/// released; `fail_at(k)` fails the segment sync numbered `k`, counting from 0.
#[derive(Default)]
struct Probe {
    calls: Mutex<Vec<Fsync>>,
    held: Mutex<bool>,
    released: Condvar,
    fail: Mutex<Option<usize>>,
}

impl Probe {
    fn hold(&self) {
        *self.held.lock().unwrap() = true;
    }

    fn release(&self) {
        *self.held.lock().unwrap() = false;
        self.released.notify_all();
    }

    fn fail_at(&self, k: usize) {
        *self.fail.lock().unwrap() = Some(k);
    }

    fn count(&self, of: impl Fn(&Fsync) -> bool) -> usize {
        self.calls.lock().unwrap().iter().filter(|f| of(f)).count()
    }

    fn segment_syncs(&self) -> usize {
        self.count(|f| matches!(f, Fsync::Segment(_)))
    }
}

impl FsyncPort for Probe {
    fn sync(&self, what: Fsync, _file: &File) -> std::io::Result<()> {
        let prior = {
            let mut calls = self.calls.lock().unwrap();
            let prior = calls.iter().filter(|f| matches!(f, Fsync::Segment(_))).count();
            calls.push(what);
            prior
        };
        if matches!(what, Fsync::Segment(_)) {
            let mut held = self.held.lock().unwrap();
            while *held {
                held = self.released.wait(held).unwrap();
            }
            if *self.fail.lock().unwrap() == Some(prior) {
                return Err(std::io::Error::other("injected sync failure"));
            }
        }
        Ok(())
    }
}

/// Options routing every sync through `probe`, with an idle interval no test outlasts.
fn quiet(probe: &Arc<Probe>) -> AuditOptions {
    AuditOptions { idle: Duration::from_secs(3600), fsync: probe.clone() }
}

/// Wait up to 10 s for `cond`.
fn until(what: &str, cond: impl Fn() -> bool) {
    let start = Instant::now();
    while !cond() {
        assert!(start.elapsed() < Duration::from_secs(10), "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(1));
    }
}

fn tip_file(dir: &Path) -> SignedTip {
    serde_json::from_str(&fs::read_to_string(dir.join("chain.tip")).unwrap()).unwrap()
}

/// One append parks inside its segment sync while `members` more appends queue behind it;
/// releasing it commits the lone append, then the queued members as one group. Returns the
/// lone append's result and the members' results.
fn lead_then_group(
    log: &AuditLog<SeedSigner>,
    probe: &Probe,
    members: u64,
) -> (Result<AuditEntry, AuditError>, Vec<Result<AuditEntry, AuditError>>) {
    probe.hold();
    let parked = probe.segment_syncs() + 1;
    std::thread::scope(|s| {
        let lead = s.spawn(|| log.append(attrs("agent://lead", 0)));
        until("the lone append to reach its sync", || probe.segment_syncs() == parked);
        let group: Vec<_> = (0..members).map(|i| s.spawn(move || log.append(attrs("agent://member", i)))).collect();
        until("every member to queue", || log.queued() == members as usize);
        probe.release();
        (lead.join().unwrap(), group.into_iter().map(|h| h.join().unwrap()).collect())
    })
}

// spec: disclosure.record.group-commit@6723eced
#[test]
fn an_append_group_shares_one_segment_sync_and_releases_or_refuses_together() {
    const MEMBERS: u64 = 15;
    let dir = tempfile::tempdir().unwrap();
    let probe = Arc::new(Probe::default());
    let log = AuditLog::open_with(dir.path(), key(), quiet(&probe)).unwrap();

    // Two groups: the lone append, then fifteen members behind one sync.
    let (lead, group) = lead_then_group(&log, &probe, MEMBERS);
    assert_eq!(lead.unwrap().seq, 1);
    let mut seqs: Vec<u64> = group.iter().map(|r| r.as_ref().unwrap().seq).collect();
    seqs.sort_unstable();
    assert_eq!(seqs, (2..=MEMBERS + 1).collect::<Vec<_>>());
    assert_eq!(probe.segment_syncs(), 2, "one segment sync per group");

    // Two more groups, the second's sync failing: every member raises, none persists.
    probe.fail_at(3);
    let (lead, group) = lead_then_group(&log, &probe, MEMBERS);
    let lead = lead.unwrap();
    assert_eq!(lead.seq, MEMBERS + 2);
    assert_eq!(group.len() as u64, MEMBERS);
    for r in &group {
        assert!(matches!(r, Err(AuditError::AuditEntryUnpersisted(_))), "{r:?}");
    }
    assert_eq!(log.tip().seq, lead.seq);
    assert_eq!(lines(&segment(dir.path(), 1)).len() as u64, lead.seq, "the failed group's lines are truncated away");

    let groups = 4;
    let per_group = probe.segment_syncs() as f64 / groups as f64;
    contextful_eval::record::emit("audit-sync-per-group", per_group, groups, 0);
    assert_eq!(probe.segment_syncs(), groups as usize);
    assert_eq!(log.append(attrs("agent://after", 0)).unwrap().seq, lead.seq + 1);
    drop(log);
    assert_eq!(verify_signed(dir.path(), &SignerKey::of(&key())).unwrap().seq, lead.seq + 1);
}

// spec: disclosure.record.segment-open@9e7d7401
#[test]
fn an_append_group_that_creates_a_segment_adds_one_directory_sync() {
    let dir = tempfile::tempdir().unwrap();
    let probe = Arc::new(Probe::default());
    let log = AuditLog::open_with(dir.path(), key(), quiet(&probe)).unwrap();
    let opens = || probe.count(|f| matches!(f, Fsync::SegmentOpen(_)));
    assert_eq!(opens(), 0, "opening an empty log creates no segment");

    log.append(attrs("agent://a", 0)).unwrap();
    assert_eq!((opens(), probe.segment_syncs()), (1, 1));
    for i in 1..6 {
        log.append(attrs("agent://a", i)).unwrap();
    }
    assert_eq!((opens(), probe.segment_syncs()), (1, 6), "appends into an open segment sync no directory");

    log.append_all((6..=AUDIT_SEGMENT_ENTRIES).map(|i| attrs("agent://a", i)).collect()).unwrap();
    let calls = probe.calls.lock().unwrap().clone();
    let opened: Vec<Fsync> = calls.iter().copied().filter(|f| matches!(f, Fsync::SegmentOpen(_))).collect();
    assert_eq!(opened, [Fsync::SegmentOpen(1), Fsync::SegmentOpen(2)]);
    assert_eq!(probe.segment_syncs(), 8, "the crossing group syncs each segment it reaches once");
}

// spec: disclosure.record.tip-signing@b8b6b5ec
#[test]
fn the_tip_signs_at_segment_close_on_idle_and_at_export() {
    assert_eq!(AUDIT_TIP_IDLE, Duration::from_secs(1));
    assert_eq!(AuditOptions::default().idle, AUDIT_TIP_IDLE);
    let signer_key = SignerKey::of(&key());
    let dir = tempfile::tempdir().unwrap();
    let probe = Arc::new(Probe::default());
    let log = AuditLog::open_with(dir.path(), key(), quiet(&probe)).unwrap();
    assert_eq!(tip_file(dir.path()).seq, 0, "opening signs the genesis tip");
    let at_open = probe.count(|f| matches!(f, Fsync::Tip));

    // Groups closing no segment write no tip.
    for i in 0..3 {
        log.append(attrs("agent://a", i)).unwrap();
    }
    assert_eq!(probe.count(|f| matches!(f, Fsync::Tip)), at_open);
    assert_eq!(tip_file(dir.path()).seq, 0);
    assert_eq!(verify_signed(dir.path(), &signer_key).unwrap().seq, 3, "an unsigned tail past the tip verifies");

    // A group closing segment 1 signs the tip at its last entry.
    log.append_all((3..=AUDIT_SEGMENT_ENTRIES).map(|i| attrs("agent://a", i)).collect()).unwrap();
    let closed = tip_file(dir.path());
    assert_eq!(closed.seq, AUDIT_SEGMENT_ENTRIES + 1);
    assert!(closed.verify(&signer_key));

    // Export signs the tip at the chain end.
    log.append(attrs("agent://b", 0)).unwrap();
    assert_eq!(tip_file(dir.path()).seq, AUDIT_SEGMENT_ENTRIES + 1);
    let exported = log.export().unwrap();
    assert_eq!(exported.seq, AUDIT_SEGMENT_ENTRIES + 2);
    assert!(exported.verify(&signer_key));
    assert_eq!(tip_file(dir.path()), exported);
    drop(log);

    // Idle: under the default options the tip reaches the chain end 1 s after the last append.
    let log = AuditLog::open(dir.path(), key()).unwrap();
    let last = log.append(attrs("agent://c", 0)).unwrap();
    let appended = Instant::now();
    assert_eq!(tip_file(dir.path()).seq, exported.seq);
    until("the idle tip", || tip_file(dir.path()).seq == last.seq);
    assert!(appended.elapsed() >= Duration::from_millis(900), "the tip signed {:?} after the append", appended.elapsed());
    assert!(tip_file(dir.path()).verify(&signer_key));
}

/// Counts segment syncs and makes every sync durable.
#[derive(Default)]
struct Counted {
    segment: AtomicU64,
}

impl FsyncPort for Counted {
    fn sync(&self, what: Fsync, file: &File) -> std::io::Result<()> {
        if matches!(what, Fsync::Segment(_)) {
            self.segment.fetch_add(1, Ordering::Relaxed);
        }
        FileFsync.sync(what, file)
    }
}

/// The nearest-rank `q` quantile of sorted `xs`.
fn quantile(xs: &[Duration], q: f64) -> Duration {
    xs[((q * xs.len() as f64).ceil() as usize).clamp(1, xs.len()) - 1]
}

/// Append latency under real syncs in a temporary directory: each of `writers` threads
/// times 200 appends after warm-up (`assurance.measure.timing-iterations`); a batch
/// reports p50, p95 and p99 over every append it timed, and the figure is the median
/// batch of 5 (`assurance.measure.timing-batches`).
#[test]
fn append_latency_under_group_commit_at_one_and_sixteen_writers() {
    const REPEATS: usize = 200;
    const BATCHES: usize = 5;
    for (writers, id) in [(1u64, "audit-append-latency-lone"), (16, "audit-append-latency")] {
        let dir = tempfile::tempdir().unwrap();
        let counted = Arc::new(Counted::default());
        let options = AuditOptions { fsync: counted.clone(), ..AuditOptions::default() };
        let log = AuditLog::open_with(dir.path(), key(), options).unwrap();
        let round = |repeats: usize| -> Vec<Duration> {
            std::thread::scope(|s| {
                let hands: Vec<_> = (0..writers)
                    .map(|w| {
                        let log = &log;
                        s.spawn(move || {
                            (0..repeats)
                                .map(|i| {
                                    let at = Instant::now();
                                    log.append(attrs("agent://bench", w * 1_000_000 + i as u64)).unwrap();
                                    at.elapsed()
                                })
                                .collect::<Vec<_>>()
                        })
                    })
                    .collect();
                hands.into_iter().flat_map(|h| h.join().unwrap()).collect()
            })
        };
        round(20);
        let (before, started) = (counted.segment.load(Ordering::Relaxed), Instant::now());
        let mut batches: Vec<[Duration; 3]> = (0..BATCHES)
            .map(|_| {
                let mut xs = round(REPEATS);
                xs.sort_unstable();
                [quantile(&xs, 0.50), quantile(&xs, 0.95), quantile(&xs, 0.99)]
            })
            .collect();
        let wall = started.elapsed();
        let syncs = counted.segment.load(Ordering::Relaxed) - before;
        let appends = writers * (REPEATS * BATCHES) as u64;
        batches.sort_unstable_by_key(|b| b[2]);
        let [p50, p95, p99] = batches[BATCHES / 2];
        eprintln!(
            "audit append, {writers} writer(s): p50 {} us, p95 {} us, p99 {} us (median of {BATCHES} batches of {} appends); {:.1} appends per segment sync; {:.0} appends/s",
            p50.as_micros(),
            p95.as_micros(),
            p99.as_micros(),
            writers * REPEATS as u64,
            appends as f64 / syncs.max(1) as f64,
            appends as f64 / wall.as_secs_f64(),
        );
        contextful_eval::record::emit(id, p99.as_micros() as f64, appends, 0);
        assert!(syncs <= appends, "a group issues at most one segment sync");
    }
}
