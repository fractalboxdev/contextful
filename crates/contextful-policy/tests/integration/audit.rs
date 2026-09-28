//! `disclosure.record` and `disclosure.attest`: the hash-linked chain, its numbered
//! segments, the chain tip, the signed root closing each segment, and verification.

use contextful_policy::audit::{query_digest, verify, verify_signed, AuditEntry, AuditError, AuditLog, SignedRoot, SignedTip, AUDIT_SEGMENT_ENTRIES, GENESIS};
use ed25519_dalek::SigningKey;
use serde_json::{json, Value};
use std::fs;
use std::path::{Path, PathBuf};

fn key() -> SigningKey {
    SigningKey::from_bytes(&[7; 32])
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
    let mut log = AuditLog::open(dir.path(), key()).unwrap();
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
    assert!(tip.verify(&key().verifying_key()));
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
    let mut log = AuditLog::open(dir.path(), key()).unwrap();
    let batch: Vec<Value> = (0..AUDIT_SEGMENT_ENTRIES + 1).map(|i| attrs("agent://a", i)).collect();
    log.append_all(batch).unwrap();

    let first = lines(&segment(dir.path(), 1));
    assert_eq!(first.len() as u64, AUDIT_SEGMENT_ENTRIES);
    assert!(first.windows(2).all(|w| w[1].seq == w[0].seq + 1), "seq ascends within the segment");
    assert_eq!(first[0].seq, 1);

    let root: SignedRoot = serde_json::from_str(&fs::read_to_string(root_file(dir.path(), 1)).unwrap()).unwrap();
    assert_eq!(root.count, AUDIT_SEGMENT_ENTRIES);
    assert_eq!(root.root, first.last().unwrap().entry_hash);
    assert!(root.verify(&key().verifying_key()));

    let second = lines(&segment(dir.path(), 2));
    assert_eq!(second.iter().map(|e| e.seq).collect::<Vec<_>>(), [AUDIT_SEGMENT_ENTRIES + 1]);
    assert_eq!(second[0].prev_hash, root.root);
    assert!(!root_file(dir.path(), 2).exists(), "an open segment carries no root");
    assert_eq!(verify_signed(dir.path(), &key().verifying_key()).unwrap().seq, AUDIT_SEGMENT_ENTRIES + 1);
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
    assert_eq!(broken_at(verify_signed(dir.path(), &key().verifying_key())), 7);
    assert_eq!(broken_at(AuditLog::open(dir.path(), key())), 7);

    // A tip signed under another key, and a chain with no tip at all.
    let dir = log_of(3);
    let entries = lines(&segment(dir.path(), 1));
    let forged = SignedTip::sign(3, &entries[2].entry_hash, &SigningKey::from_bytes(&[9; 32])).unwrap();
    fs::write(dir.path().join("chain.tip"), serde_json::to_string(&forged).unwrap()).unwrap();
    assert_eq!(broken_at(AuditLog::open(dir.path(), key())), 3);
    fs::remove_file(dir.path().join("chain.tip")).unwrap();
    assert_eq!(broken_at(verify_signed(dir.path(), &key().verifying_key())), 1);

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
    let mut first = AuditLog::open(dir.path(), key()).unwrap();
    match AuditLog::open(dir.path(), key()) {
        Err(AuditError::AuditLogHeld(m)) => assert!(m.contains("audit.lock"), "{m}"),
        other => panic!("expected AuditLogHeld, got {other:?}"),
    }
    first.append(attrs("agent://a", 3)).unwrap();
    drop(first);
    let mut second = AuditLog::open(dir.path(), key()).unwrap();
    assert_eq!(second.append(attrs("agent://b", 4)).unwrap().seq, 4);
    assert_eq!(verify_signed(dir.path(), &key().verifying_key()).unwrap().seq, 4);
}

#[test]
fn every_trailing_truncation_under_a_rewritten_tip_is_detected() {
    const N: u64 = 40;
    let dir = log_of(N);
    let full = lines(&segment(dir.path(), 1));
    let foreign = SigningKey::from_bytes(&[9; 32]);
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
            match verify_signed(dir.path(), &key().verifying_key()) {
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
    let foreign = SigningKey::from_bytes(&[9; 32]).verifying_key();
    assert_eq!(broken_at(verify_signed(dir.path(), &foreign)), AUDIT_SEGMENT_ENTRIES);

    let path = root_file(dir.path(), 1);
    let mut root: SignedRoot = serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
    root.root = GENESIS.to_string();
    assert!(!root.verify(&key().verifying_key()));
    fs::write(&path, serde_json::to_string(&root).unwrap()).unwrap();
    assert_eq!(broken_at(verify(dir.path())), AUDIT_SEGMENT_ENTRIES);
}

#[test]
fn a_reopened_log_continues_the_linkage_from_its_tail() {
    let dir = log_of(2);
    let before = verify(dir.path()).unwrap();
    let mut log = AuditLog::open(dir.path(), key()).unwrap();
    assert_eq!(log.tip(), &before);
    let next = log.append(attrs("agent://c", 3)).unwrap();
    assert_eq!(next.seq, 3);
    assert_eq!(next.prev_hash, before.entry_hash);
    assert_eq!(verify(dir.path()).unwrap().seq, 3);
}

#[test]
fn an_append_that_fails_to_persist_leaves_disk_and_tip_at_the_prior_entry() {
    let dir = log_of(1);
    let before = verify(dir.path()).unwrap();
    let mut log = AuditLog::open(dir.path(), key()).unwrap();

    // `chain.tip` replaced by a non-empty directory: the tip cannot be renamed into place.
    let tip = dir.path().join("chain.tip");
    fs::remove_file(&tip).unwrap();
    fs::create_dir(&tip).unwrap();
    fs::write(tip.join("occupied"), "").unwrap();
    match log.append(attrs("agent://b", 2)) {
        Err(AuditError::AuditEntryUnpersisted(_)) => {}
        other => panic!("expected AuditEntryUnpersisted, got {other:?}"),
    }
    assert_eq!(log.tip(), &before);
    assert_eq!(lines(&segment(dir.path(), 1)).len(), 1, "the unpersisted line is truncated away");

    fs::remove_dir_all(&tip).unwrap();
    let next = log.append(attrs("agent://b", 2)).unwrap();
    assert_eq!(next.seq, 2);
    assert_eq!(next.prev_hash, before.entry_hash);
    // Two entries were acknowledged, the log's first and this one; verification reaches both.
    let lost = 2 - verify(dir.path()).unwrap().seq.min(2);
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
    let mut log = AuditLog::open(dir.path(), key()).unwrap();
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
    let mut log = AuditLog::open(dir.path(), key()).unwrap();
    assert_eq!(log.tip(), &before);
    assert_eq!(log.append(attrs("agent://c", 3)).unwrap().seq, 3);
    assert_eq!(verify_signed(dir.path(), &key().verifying_key()).unwrap().seq, 3);

    // A malformed line that ends in a newline is no torn write, and still breaks the chain.
    let dir = log_of(2);
    let mut f = fs::OpenOptions::new().append(true).open(segment(dir.path(), 1)).unwrap();
    std::io::Write::write_all(&mut f, b"{\"seq\":3}\n").unwrap();
    drop(f);
    assert_eq!(broken_at(AuditLog::open(dir.path(), key())), 3);
}
