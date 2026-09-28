//! `disclosure.record` and `disclosure.attest`: the hash-linked chain, its numbered
//! segments, the chain tip, the signed root closing each segment, and verification.

use contextful_core::issue::SignatureAlgorithm;
use contextful_policy::audit::{
    prove, query_digest, verify, verify_signed, AuditEntry, AuditError, AuditLog, AuditOptions, ChainFormat, ChainHeader, DigestAlgorithm,
    FileFsync, Fsync, FsyncPort, InclusionProof, SignedRoot, SignedTip, AUDIT_FORMAT, AUDIT_SEGMENT_ENTRIES, AUDIT_SEGMENT_MAX, AUDIT_TIP_IDLE,
    GENESIS,
};
use contextful_policy::issue::{SeedSigner, SignerKey};
use serde_json::{json, Value};
use sha2::Digest;
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
    assert_eq!(entries[0].prev_hash, ChainHeader::default().digest(), "a v1 chain links its first entry to its header");
    assert!(entries.iter().all(|e| e.format == AUDIT_FORMAT));
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
    for chain in [ChainFormat::V0, ChainFormat::V1(ChainHeader::default())] {
        assert_eq!(chain.link(1, GENESIS, a.clone()).entry_hash, chain.link(1, GENESIS, b.clone()).entry_hash);
    }
}

// spec: disclosure.record.segment@ef83fc56
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
    assert_eq!(root.root, merkle(DigestAlgorithm::Sha256, &first));
    assert!(root.verify(&SignerKey::of(&key())));

    let second = lines(&segment(dir.path(), 2));
    assert_eq!(second.iter().map(|e| e.seq).collect::<Vec<_>>(), [AUDIT_SEGMENT_ENTRIES + 1]);
    assert_eq!(second[0].prev_hash, first.last().unwrap().entry_hash);
    assert!(!root_file(dir.path(), 2).exists(), "an open segment carries no root");
    assert_eq!(verify_signed(dir.path(), &SignerKey::of(&key())).unwrap().seq, AUDIT_SEGMENT_ENTRIES + 1);
}

/// A disagreeing digest, entry format or Merkle root, a sequence gap, an absent chain beside `chain.tip` or a signed
/// root, or, under the signed check, an absent or unverified tip or root raises `AuditChainBroken` at the earliest
/// failing index.
// spec: disclosure.attest.broken-chain@4c95d01d
#[test]
fn a_disagreeing_digest_a_gap_or_a_vanished_chain_raises_audit_chain_broken() {
    // A disagreeing digest: entry 2's attributes rewritten after the fact.
    let dir = log_of(3);
    let mut entries = lines(&segment(dir.path(), 1));
    entries[1].attributes = attrs("agent://attacker", 999);
    write_lines(&segment(dir.path(), 1), &entries);
    assert_eq!(broken_at(verify(dir.path())), 2);

    // A disagreeing entry format: entry 2 claims v0 inside a v1 chain, and the header removed
    // leaves v1 entries a v0 walk refuses at the first.
    let dir = log_of(3);
    let mut entries = lines(&segment(dir.path(), 1));
    entries[1] = ChainFormat::V0.link(2, &entries[0].entry_hash, entries[1].attributes.clone());
    write_lines(&segment(dir.path(), 1), &entries);
    assert_eq!(broken_at(verify(dir.path())), 2);
    let dir = log_of(3);
    fs::remove_file(dir.path().join("header.json")).unwrap();
    assert_eq!(broken_at(verify(dir.path())), 1);

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

    // A disagreeing Merkle root: the root names the last entry's digest, as a v0 root does.
    let dir = small_log(8, 8);
    let path = root_file(dir.path(), 1);
    let mut root: SignedRoot = serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
    root.root = lines(&segment(dir.path(), 1)).last().unwrap().entry_hash.clone();
    fs::write(&path, serde_json::to_string(&root).unwrap()).unwrap();
    assert_eq!(broken_at(verify(dir.path())), 8);

    // A closed segment rewritten wholesale, re-linked, under its recomputed root with a garbage signature.
    let dir = tempfile::tempdir().unwrap();
    AuditLog::open(dir.path(), key()).unwrap().append_all((0..AUDIT_SEGMENT_ENTRIES + 1).map(|i| attrs("agent://a", i)).collect()).unwrap();
    let chain = ChainFormat::V1(ChainHeader::default());
    let mut prev = ChainHeader::default().digest();
    let forged: Vec<AuditEntry> = lines(&segment(dir.path(), 1))
        .into_iter()
        .map(|e| {
            let f = chain.link(e.seq, &prev, attrs("agent://attacker", e.seq));
            prev = f.entry_hash.clone();
            f
        })
        .collect();
    write_lines(&segment(dir.path(), 1), &forged);
    let path = root_file(dir.path(), 1);
    let mut root: SignedRoot = serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
    root.root = merkle(DigestAlgorithm::Sha256, &forged);
    root.signature = "00".repeat(64);
    fs::write(&path, serde_json::to_string(&root).unwrap()).unwrap();
    let mut second = lines(&segment(dir.path(), 2));
    second[0] = chain.link(second[0].seq, &prev, second[0].attributes.clone());
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
    AuditOptions { idle: Duration::from_secs(3600), fsync: probe.clone(), ..AuditOptions::default() }
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

/// A chain header under `digest` closing a segment every `segment_entries` entries.
fn header(digest: DigestAlgorithm, segment_entries: u64) -> ChainHeader {
    ChainHeader { format: AUDIT_FORMAT, digest, segment_entries }
}

fn with_header(header: ChainHeader) -> AuditOptions {
    AuditOptions { header, ..AuditOptions::default() }
}

/// A SHA-256 log of `n` entries whose segments close every `size` entries.
fn small_log(size: u64, n: u64) -> tempfile::TempDir {
    small_log_under(DigestAlgorithm::Sha256, size, n)
}

fn small_log_under(digest: DigestAlgorithm, size: u64, n: u64) -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    let log = AuditLog::open_with(dir.path(), key(), with_header(header(digest, size))).unwrap();
    log.append_all((0..n).map(|i| attrs("agent://a", i)).collect()).unwrap();
    dir
}

/// `parts` concatenated under `digest`, computed apart from the library.
fn hash(digest: DigestAlgorithm, parts: &[&[u8]]) -> [u8; 32] {
    match digest {
        DigestAlgorithm::Sha256 => {
            let mut h = sha2::Sha256::new();
            parts.iter().for_each(|p| h.update(p));
            h.finalize().into()
        }
        DigestAlgorithm::Blake3 => {
            let mut h = blake3::Hasher::new();
            parts.iter().for_each(|p| {
                h.update(p);
            });
            *h.finalize().as_bytes()
        }
    }
}

fn prefix(digest: DigestAlgorithm) -> &'static str {
    match digest {
        DigestAlgorithm::Sha256 => "sha256",
        DigestAlgorithm::Blake3 => "blake3",
    }
}

/// The RFC 6962 Merkle tree hash over leaf inputs `leaves`, by the recursive definition.
fn tree_hash(digest: DigestAlgorithm, leaves: &[Vec<u8>]) -> [u8; 32] {
    match leaves.len() {
        0 => hash(digest, &[]),
        1 => hash(digest, &[&[0], &leaves[0]]),
        n => {
            let mut k = 1;
            while k * 2 < n {
                k *= 2;
            }
            hash(digest, &[&[1], &tree_hash(digest, &leaves[..k]), &tree_hash(digest, &leaves[k..])])
        }
    }
}

/// The root a segment of `entries` closes under: the tree hash of their raw digests.
fn merkle(digest: DigestAlgorithm, entries: &[AuditEntry]) -> String {
    let leaves: Vec<Vec<u8>> = entries.iter().map(|e| hex::decode(e.entry_hash.split_once(':').unwrap().1).unwrap()).collect();
    format!("{}:{}", prefix(digest), hex::encode(tree_hash(digest, &leaves)))
}

/// A v0 chain of `n` entries written as a v0 writer leaves it: no header, roots over the
/// last entry digest, and a signed tip at the end.
fn v0_chain(dir: &Path, n: u64) -> Vec<AuditEntry> {
    fs::create_dir_all(dir.join("segments")).unwrap();
    let mut prev = GENESIS.to_string();
    let all: Vec<AuditEntry> = (1..=n)
        .map(|seq| {
            let e = ChainFormat::V0.link(seq, &prev, attrs("agent://v0", seq));
            prev = e.entry_hash.clone();
            e
        })
        .collect();
    for (i, chunk) in all.chunks(AUDIT_SEGMENT_ENTRIES as usize).enumerate() {
        let k = i as u64 + 1;
        write_lines(&segment(dir, k), chunk);
        if chunk.len() as u64 == AUDIT_SEGMENT_ENTRIES {
            let root = SignedRoot::sign(&chunk.last().unwrap().entry_hash, AUDIT_SEGMENT_ENTRIES, &key()).unwrap();
            fs::write(root_file(dir, k), serde_json::to_string(&root).unwrap()).unwrap();
        }
    }
    let tip = SignedTip::sign(n, &prev, &key()).unwrap();
    fs::write(dir.join("chain.tip"), serde_json::to_string(&tip).unwrap()).unwrap();
    all
}

/// A v1 entry carries `format: 1`, and its `entry_hash` is the chain header's digest over the RFC 8785 canonical
/// JSON of the entry's `format`, `seq`, `prev_hash` and `attributes`.
// spec: disclosure.record.entry-format@bda9d3a6
#[test]
fn a_v1_entry_digests_the_rfc_8785_form_of_its_whole_entry() {
    // Keys sort by UTF-16 code unit (U+1F600 before U+E000), numbers take the ECMAScript form,
    // and U+2028 stays literal.
    let attributes = json!({
        "z": 1e21,
        "b": [1.5, "é\u{2028}"],
        "a": { "y": 0.000001, "x": 1e-7 },
        "\u{e000}": 1,
        "\u{1f600}": 2,
    });
    let canonical = |prev: &str| {
        format!(
            "{{\"attributes\":{{\"a\":{{\"x\":1e-7,\"y\":0.000001}},\"b\":[1.5,\"é\u{2028}\"],\"z\":1e+21,\"\u{1f600}\":2,\"\u{e000}\":1}},\"format\":1,\"prev_hash\":\"{prev}\",\"seq\":1}}"
        )
    };
    for digest in [DigestAlgorithm::Sha256, DigestAlgorithm::Blake3] {
        let dir = tempfile::tempdir().unwrap();
        let log = AuditLog::open_with(dir.path(), key(), with_header(header(digest, AUDIT_SEGMENT_ENTRIES))).unwrap();
        let entry = log.append(attributes.clone()).unwrap();
        drop(log);
        assert_eq!(entry.format, AUDIT_FORMAT);
        let expected = hash(digest, &[canonical(&entry.prev_hash).as_bytes()]);
        assert_eq!(entry.entry_hash, format!("{}:{}", prefix(digest), hex::encode(expected)), "{digest:?}");
        assert_eq!(lines(&segment(dir.path(), 1)), std::slice::from_ref(&entry), "the line reads back as written");
        assert_eq!(verify_signed(dir.path(), &SignerKey::of(&key())).unwrap().entry_hash, entry.entry_hash);
    }
}

/// Appending a v1 entry whose attributes hold an integer beyond ±(2^53 − 1) raises `AuditAttributeInexact` and
/// appends nothing; verifying a chain holding such an entry raises {{disclosure.attest.broken-chain}}.
// spec: disclosure.record.inexact-integer@e172e4f3
#[test]
fn a_v1_attribute_integer_beyond_2_53_raises_audit_attribute_inexact() {
    const EXACT: u64 = (1 << 53) - 1;
    let inexact = |r: Result<_, AuditError>| matches!(r, Err(AuditError::AuditAttributeInexact(_)));
    let rows = |n: Value| json!({ "contextful.subject.agent": "agent://a", "contextful.result.rows": n });

    // RFC 8785 writes 2^53 + 1 and 2^53 as one number, so their digests agree.
    let chain = ChainFormat::V1(ChainHeader::default());
    let genesis = chain.genesis().entry_hash;
    assert_eq!(
        chain.link(1, &genesis, rows(json!(EXACT + 2))).entry_hash,
        chain.link(1, &genesis, rows(json!(EXACT + 1))).entry_hash
    );

    let dir = tempfile::tempdir().unwrap();
    let log = AuditLog::open(dir.path(), key()).unwrap();
    log.append(rows(json!(EXACT))).unwrap();
    log.append(rows(json!(-(EXACT as i64)))).unwrap();
    log.append(rows(json!(1e300))).unwrap();
    for value in [
        rows(json!(EXACT + 1)),
        rows(json!(u64::MAX)),
        rows(json!(-(EXACT as i64) - 1)),
        json!({ "nested": [{ "id": EXACT + 2 }] }),
    ] {
        assert!(inexact(log.append(value.clone()).map(|_| ())), "{value}");
        assert!(inexact(log.append_all(vec![rows(json!(1)), value.clone()]).map(|_| ())), "a batch holding {value}");
    }
    assert_eq!(log.tip().seq, 3, "a refused append leaves the chain as it was");
    assert_eq!(log.append(rows(json!(4))).unwrap().seq, 4);
    drop(log);
    assert_eq!(lines(&segment(dir.path(), 1)).len(), 4);
    assert_eq!(verify_signed(dir.path(), &SignerKey::of(&key())).unwrap().seq, 4);

    // An entry written with such an integer, and its edit to a neighbour, both break the chain.
    let mut entries = lines(&segment(dir.path(), 1));
    let rewritten = chain.link(5, &entries[3].entry_hash, rows(json!(EXACT + 2)));
    entries.push(rewritten.clone());
    write_lines(&segment(dir.path(), 1), &entries);
    fs::remove_file(dir.path().join("chain.tip")).unwrap();
    assert_eq!(broken_at(verify(dir.path())), 5);
    entries[4].attributes = rows(json!(EXACT + 1));
    write_lines(&segment(dir.path(), 1), &entries);
    assert_eq!(broken_at(verify(dir.path())), 5);
    assert!(!chain.digest_agrees(&rewritten));

    // A v0 chain digests the integer's exact decimal form and admits it.
    let dir = tempfile::tempdir().unwrap();
    v0_chain(dir.path(), 1);
    let log = AuditLog::open(dir.path(), key()).unwrap();
    assert_eq!(log.append(rows(json!(EXACT + 2))).unwrap().seq, 2);
}

/// `header.json`, written before a new chain's first entry, fixes the chain's format, its digest, `sha256` or
/// `blake3`, and a segment size of at most 65536 entries; the first v1 entry's `prev_hash` is the canonical header's
/// digest.
// spec: disclosure.record.chain-header@5ec9601b
#[test]
fn a_chain_header_fixes_the_digest_and_segment_size_and_roots_the_first_link() {
    let dir = log_of(1);
    let written: ChainHeader = serde_json::from_str(&fs::read_to_string(dir.path().join("header.json")).unwrap()).unwrap();
    assert_eq!(written, ChainHeader::default());
    assert_eq!(written, header(DigestAlgorithm::Sha256, AUDIT_SEGMENT_ENTRIES));
    let canonical = r#"{"digest":"sha256","format":1,"segment_entries":4096}"#;
    assert_eq!(written.digest(), format!("sha256:{}", hex::encode(sha2::Sha256::digest(canonical.as_bytes()))));
    assert_eq!(lines(&segment(dir.path(), 1))[0].prev_hash, written.digest());

    // BLAKE3 under 3-entry segments: two closed segments and one open.
    let dir = small_log_under(DigestAlgorithm::Blake3, 3, 7);
    for n in 1..=2 {
        let root: SignedRoot = serde_json::from_str(&fs::read_to_string(root_file(dir.path(), n)).unwrap()).unwrap();
        assert_eq!(root.count, 3);
        assert_eq!(root.root, merkle(DigestAlgorithm::Blake3, &lines(&segment(dir.path(), n))));
    }
    assert!(!root_file(dir.path(), 3).exists());
    assert!(lines(&segment(dir.path(), 3)).iter().all(|e| e.entry_hash.starts_with("blake3:")));
    assert_eq!(verify_signed(dir.path(), &SignerKey::of(&key())).unwrap().seq, 7);

    // The chain's header outlives the options a later open names.
    let log = AuditLog::open_with(dir.path(), key(), with_header(ChainHeader::default())).unwrap();
    let next = log.append(attrs("agent://b", 0)).unwrap();
    assert!(next.entry_hash.starts_with("blake3:"));
    drop(log);
    assert_eq!(lines(&segment(dir.path(), 3)).len(), 2);
    assert!(!root_file(dir.path(), 3).exists());

    // A rewritten header breaks the chain at its first entry.
    fs::write(dir.path().join("header.json"), serde_json::to_string(&header(DigestAlgorithm::Sha256, 3)).unwrap()).unwrap();
    assert_eq!(broken_at(verify(dir.path())), 1);

    // A segment size at the bound opens.
    let dir = tempfile::tempdir().unwrap();
    let log = AuditLog::open_with(dir.path(), key(), with_header(header(DigestAlgorithm::Sha256, AUDIT_SEGMENT_MAX))).unwrap();
    assert_eq!(log.append(attrs("agent://a", 0)).unwrap().seq, 1);
    assert_eq!(AUDIT_SEGMENT_MAX, 65_536);
}

/// Opening or verifying a chain whose header names another format or digest, or a segment size outside
/// {{disclosure.record.chain-header}}, raises `AuditHeaderUnsupported`.
// spec: disclosure.record.header-unsupported@437479de
#[test]
fn a_header_naming_another_format_digest_or_segment_size_raises_audit_header_unsupported() {
    let unsupported = |r: Result<_, AuditError>| matches!(r, Err(AuditError::AuditHeaderUnsupported(_)));
    for text in [
        r#"{"format":2,"digest":"sha256","segment_entries":4096}"#.to_string(),
        r#"{"format":1,"digest":"md5","segment_entries":4096}"#.to_string(),
        r#"{"format":1,"digest":"sha256","segment_entries":0}"#.to_string(),
        format!(r#"{{"format":1,"digest":"sha256","segment_entries":{}}}"#, AUDIT_SEGMENT_MAX + 1),
        "sha256".to_string(),
    ] {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("header.json"), &text).unwrap();
        assert!(unsupported(verify(dir.path()).map(|_| ())), "{text}");
        assert!(unsupported(AuditLog::open(dir.path(), key()).map(|_| ())), "{text}");
    }
    for size in [0, AUDIT_SEGMENT_MAX + 1] {
        let dir = tempfile::tempdir().unwrap();
        assert!(unsupported(AuditLog::open_with(dir.path(), key(), with_header(header(DigestAlgorithm::Sha256, size))).map(|_| ())));
        assert!(!dir.path().join("header.json").exists(), "a refused header is never written");
    }
}

/// A chain holding entries without `header.json` is a v0 chain: it verifies and appends under v0 rules, each entry
/// digest over `seq`, `prev_hash` and attributes and each root its segment's last entry digest.
// spec: disclosure.record.v0-chain@c806b311
#[test]
fn a_v0_chain_verifies_and_appends_under_v0_rules() {
    let dir = tempfile::tempdir().unwrap();
    let written = v0_chain(dir.path(), AUDIT_SEGMENT_ENTRIES + 1);
    assert_eq!(verify_signed(dir.path(), &SignerKey::of(&key())).unwrap().seq, AUDIT_SEGMENT_ENTRIES + 1);

    let log = AuditLog::open(dir.path(), key()).unwrap();
    let next = log.append(attrs("agent://v0", 0)).unwrap();
    drop(log);
    let last = written.last().unwrap();
    assert_eq!(next, ChainFormat::V0.link(last.seq + 1, &last.entry_hash, attrs("agent://v0", 0)));
    assert_eq!(next.format, 0);
    assert!(!dir.path().join("header.json").exists(), "a v0 chain gains no header");
    assert!(!fs::read_to_string(segment(dir.path(), 2)).unwrap().contains("format"), "a v0 line carries no format");
    assert_eq!(verify_signed(dir.path(), &SignerKey::of(&key())).unwrap().seq, AUDIT_SEGMENT_ENTRIES + 2);

    // v0 rules still catch a rewrite.
    let mut entries = lines(&segment(dir.path(), 1));
    entries[4].attributes = attrs("agent://attacker", 0);
    write_lines(&segment(dir.path(), 1), &entries);
    assert_eq!(broken_at(verify(dir.path())), 5);

    // A chain the v0 writer left on disk, byte for byte: four entries, its signed tip, and
    // the signed root it gives the last entry digest over a 4096-entry segment.
    let dir = tempfile::tempdir().unwrap();
    fs::create_dir_all(dir.path().join("segments")).unwrap();
    fs::write(segment(dir.path(), 1), V0_SEGMENT).unwrap();
    fs::write(dir.path().join("chain.tip"), V0_TIP).unwrap();
    let written = lines(&segment(dir.path(), 1));
    let mut prev = GENESIS.to_string();
    for e in &written {
        assert_eq!(e, &ChainFormat::V0.link(e.seq, &prev, e.attributes.clone()), "entry {} under the v0 digest", e.seq);
        prev = e.entry_hash.clone();
    }
    assert_eq!(written[2].attributes["contextful.result.rows"], json!(9_007_199_254_740_993u64), "v0 keeps every integer");
    assert_eq!(prev, "sha256:447b5841d4f80e512e4c850fdc94f57c8a03fc3d0781e0224da67f6ceec1c2cf");
    let end = verify_signed(dir.path(), &SignerKey::of(&key())).unwrap();
    assert_eq!((end.seq, end.entry_hash.as_str()), (4, prev.as_str()));
    let root: SignedRoot = serde_json::from_str(V0_ROOT).unwrap();
    assert_eq!(root, SignedRoot::sign(&prev, AUDIT_SEGMENT_ENTRIES, &key()).unwrap());
    assert!(root.verify(&SignerKey::of(&key())));
    let log = AuditLog::open(dir.path(), key()).unwrap();
    let next = log.append(attrs("agent://v0", 5)).unwrap();
    drop(log);
    assert_eq!(next, ChainFormat::V0.link(5, &prev, attrs("agent://v0", 5)));
    assert_eq!(verify_signed(dir.path(), &SignerKey::of(&key())).unwrap().seq, 5);
}

/// The v0 writer's segment, tip and root, under [`key`].
const V0_SEGMENT: &str = include_str!("../fixtures/audit-v0/chain/segments/000001.jsonl");
const V0_TIP: &str = include_str!("../fixtures/audit-v0/chain/chain.tip");
const V0_ROOT: &str = include_str!("../fixtures/audit-v0/root.json");

/// A v1 segment root is the RFC 6962 Merkle tree hash, under the header's digest, over the segment's raw entry
/// digests in `seq` order.
// spec: disclosure.attest.merkle-root@4e9b04e0
#[test]
fn a_v1_segment_root_is_the_rfc_6962_tree_hash_of_its_entry_digests() {
    for digest in [DigestAlgorithm::Sha256, DigestAlgorithm::Blake3] {
        for size in [1u64, 2, 5, 8, 13] {
            let dir = small_log_under(digest, size, size * 2);
            for n in 1..=2 {
                let root: SignedRoot = serde_json::from_str(&fs::read_to_string(root_file(dir.path(), n)).unwrap()).unwrap();
                assert_eq!(root.root, merkle(digest, &lines(&segment(dir.path(), n))), "{digest:?}, {size} entries, segment {n}");
            }
        }
    }
}

/// A v1 signed root carries its signing algorithm, `Ed25519` or `ES256`, and signs it with the header digest,
/// segment number, entry count and root.
// spec: disclosure.attest.root-tag@82d85637
#[test]
fn a_v1_root_names_and_signs_its_algorithm_header_and_segment() {
    for algorithm in [SignatureAlgorithm::Ed25519, SignatureAlgorithm::Es256] {
        let dir = tempfile::tempdir().unwrap();
        let signer = seeded(7, algorithm);
        let log = AuditLog::open_with(dir.path(), seeded(7, algorithm), with_header(header(DigestAlgorithm::Sha256, 4))).unwrap();
        log.append_all((0..9).map(|i| attrs("agent://a", i)).collect()).unwrap();
        drop(log);
        let key = SignerKey::of(&signer);
        let root: SignedRoot = serde_json::from_str(&fs::read_to_string(root_file(dir.path(), 2)).unwrap()).unwrap();
        assert_eq!(root.format, AUDIT_FORMAT);
        assert_eq!(root.alg, Some(algorithm));
        assert_eq!(root.header.as_deref(), Some(header(DigestAlgorithm::Sha256, 4).digest().as_str()));
        assert_eq!(root.segment, Some(2));
        assert!(root.verify(&key), "{algorithm}");

        let other = match algorithm {
            SignatureAlgorithm::Ed25519 => SignatureAlgorithm::Es256,
            SignatureAlgorithm::Es256 => SignatureAlgorithm::Ed25519,
        };
        for tampered in [
            SignedRoot { alg: Some(other), ..root.clone() },
            SignedRoot { alg: None, ..root.clone() },
            SignedRoot { segment: Some(1), ..root.clone() },
            SignedRoot { header: Some(ChainHeader::default().digest()), ..root.clone() },
            SignedRoot { count: 3, ..root.clone() },
        ] {
            assert!(!tampered.verify(&key), "{tampered:?}");
        }
        // Segment 1's root copied over segment 2's names the wrong segment.
        fs::copy(root_file(dir.path(), 1), root_file(dir.path(), 2)).unwrap();
        assert_eq!(broken_at(verify(dir.path())), 8);
    }
}

/// `prove` returns one entry, its chain header, its RFC 6962 audit path and its segment's signed root; the proof
/// verifies offline under the signer's public key alone.
// spec: disclosure.attest.inclusion-proof@371c6a1b
#[test]
fn an_inclusion_proof_verifies_offline_with_at_most_12_hashes_in_a_4096_entry_segment() {
    let dir = log_of(AUDIT_SEGMENT_ENTRIES + 1);
    let key = SignerKey::of(&key());
    let mut seqs: Vec<u64> = (1..=AUDIT_SEGMENT_ENTRIES).step_by(97).collect();
    seqs.extend([2, 3, 2048, 2049, AUDIT_SEGMENT_ENTRIES - 1, AUDIT_SEGMENT_ENTRIES]);
    let proofs: Vec<String> = seqs
        .iter()
        .map(|&seq| {
            let proof = prove(dir.path(), seq).unwrap();
            assert_eq!(proof.entry.seq, seq);
            serde_json::to_string(&proof).unwrap()
        })
        .collect();
    drop(dir);
    // The chain is gone: each proof verifies from its own bytes and the public key.
    let mut hashes = 0;
    for text in &proofs {
        let proof: InclusionProof = serde_json::from_str(text).unwrap();
        proof.verify(&key).unwrap_or_else(|e| panic!("seq {}: {e}", proof.entry.seq));
        hashes = hashes.max(proof.path.len());
    }
    contextful_eval::record::emit("audit-inclusion-proof", hashes as f64, proofs.len() as u64, 0);
    assert_eq!(hashes, 12);

    // Every entry of a segment whose size is no power of two, under either digest.
    for digest in [DigestAlgorithm::Sha256, DigestAlgorithm::Blake3] {
        let dir = small_log_under(digest, 7, 14);
        for seq in 1..=14 {
            prove(dir.path(), seq).unwrap().verify(&key).unwrap_or_else(|e| panic!("{digest:?} seq {seq}: {e}"));
        }
    }
}

/// A proof whose entry, audit path, header, root or root signature disagrees raises `AuditProofInvalid`.
// spec: disclosure.attest.proof-invalid@22d38afb
#[test]
fn a_proof_disagreeing_anywhere_raises_audit_proof_invalid() {
    let dir = small_log(8, 8);
    let key = SignerKey::of(&key());
    let proof = prove(dir.path(), 3).unwrap();
    proof.verify(&key).unwrap();
    let neighbour = prove(dir.path(), 4).unwrap();

    let mut cases: Vec<(&str, InclusionProof)> = Vec::new();
    let mut p = proof.clone();
    p.entry.attributes = attrs("agent://attacker", 0);
    cases.push(("rewritten attributes", p));
    let mut p = proof.clone();
    p.entry = neighbour.entry.clone();
    cases.push(("another entry under this path", p));
    let mut p = proof.clone();
    p.path[1] = "00".repeat(32);
    cases.push(("a flipped path hash", p));
    let mut p = proof.clone();
    p.path.pop();
    cases.push(("a truncated path", p));
    let mut p = proof.clone();
    p.path.push("00".repeat(32));
    cases.push(("an extended path", p));
    let mut p = proof.clone();
    p.header = header(DigestAlgorithm::Blake3, 8);
    cases.push(("another header", p));
    let mut p = proof.clone();
    p.root.root = neighbour.entry.entry_hash.clone();
    cases.push(("another root", p));
    let mut p = proof.clone();
    p.root = SignedRoot::sign_v1(&proof.header.digest(), 1, &proof.root.root, 8, &seeded(9, SignatureAlgorithm::Ed25519)).unwrap();
    cases.push(("a root signed under another key", p));
    let mut p = proof.clone();
    p.entry.format = 0;
    cases.push(("a v0 entry", p));

    for (what, p) in cases {
        match p.verify(&key) {
            Err(AuditError::AuditProofInvalid(_)) => {}
            other => panic!("{what}: expected AuditProofInvalid, got {other:?}"),
        }
    }
    match proof.verify(&SignerKey::of(&seeded(9, SignatureAlgorithm::Ed25519))) {
        Err(AuditError::AuditProofInvalid(_)) => {}
        other => panic!("a foreign key: expected AuditProofInvalid, got {other:?}"),
    }
}

/// Proving an entry of a v0 chain, of a segment carrying no signed root, or outside the chain raises
/// `AuditProofUnavailable`.
// spec: disclosure.attest.proof-unavailable@cbdaaec3
#[test]
fn proving_an_unrooted_or_absent_entry_raises_audit_proof_unavailable() {
    let unavailable = |r: Result<InclusionProof, AuditError>| matches!(r, Err(AuditError::AuditProofUnavailable(_)));
    let dir = small_log(4, 6);
    assert!(prove(dir.path(), 4).is_ok());
    assert!(unavailable(prove(dir.path(), 5)), "segment 2 is open");
    assert!(unavailable(prove(dir.path(), 0)));
    assert!(unavailable(prove(dir.path(), 7)), "past the chain end");
    assert!(unavailable(prove(dir.path(), 9)), "past the chain end, in an absent segment");

    let dir = tempfile::tempdir().unwrap();
    v0_chain(dir.path(), AUDIT_SEGMENT_ENTRIES);
    assert!(unavailable(prove(dir.path(), 1)), "a v0 root is no tree");
}
