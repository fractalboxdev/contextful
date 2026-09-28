//! `run.journal`: claims, recording, takeover, blobs and the sweep.

use contextful_core::run::journal::{sha256_hex, EntryKey, Row, BLOB_SWEEP_GRACE_SECS, BLOB_SWEEP_INTERVAL_SECS, INLINE_CUTOFF_BYTES};
use contextful_core::run::ports::Cancellation;
use contextful_core::run::{Failure, RunError};
use contextful_engine::journal::{Journal, Resolved, StepError};
use std::panic::AssertUnwindSafe;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

struct Never;

impl Cancellation for Never {
    fn requested(&self) -> bool {
        false
    }
}

fn always(_: &[u8]) -> bool {
    true
}

fn dead(_: &str) -> Result<bool, Failure> {
    Ok(false)
}

fn live(_: &str) -> Result<bool, Failure> {
    Ok(true)
}

fn key() -> EntryKey {
    EntryKey::new("x-1", "pull-0", b"null")
}

/// A journaled step's value is recorded exactly once and its effect runs at least once: a crash between effect
/// and write re-enters the effect, and every later call under the key returns the recorded value.
// spec: run.journal.step-output@7b591248
#[test]
fn a_crash_before_the_write_re_enters_and_the_record_then_stands() {
    let dir = tempfile::tempdir().unwrap();
    let j = Journal::open(dir.path());
    let effects = AtomicUsize::new(0);
    // The process dies after the effect and before the write.
    let died = std::panic::catch_unwind(AssertUnwindSafe(|| {
        j.step(&key(), "run-a", &live, &Never, &always, &mut || {
            effects.fetch_add(1, Ordering::SeqCst);
            panic!("killed between effect and write")
        })
    }));
    assert!(died.is_err());
    assert!(matches!(j.row(&key()).unwrap(), Some(Row::Pending { .. })));
    // The next caller re-enters the effect once and records its value.
    let mut effect = || {
        effects.fetch_add(1, Ordering::SeqCst);
        Ok(format!("value-{}", effects.load(Ordering::SeqCst)).into_bytes())
    };
    assert_eq!(j.step(&key(), "run-b", &dead, &Never, &always, &mut effect).unwrap(), Resolved::Recorded(b"value-2".to_vec()));
    // Every later call returns the recorded value and runs nothing.
    for run in ["run-b", "run-c"] {
        assert_eq!(j.step(&key(), run, &dead, &Never, &always, &mut effect).unwrap(), Resolved::Replayed(b"value-2".to_vec()));
    }
    assert_eq!(effects.load(Ordering::SeqCst), 2);
}

/// An entry is keyed by `(execution_id, step_label, input_hash)`. The first caller claims the key with a pending
/// row before entering the effect; a racing caller waits for the recorded value instead of computing.
// spec: run.journal.entry-key@c571b2ea
#[test]
fn a_racing_caller_waits_for_the_first_callers_value() {
    let dir = tempfile::tempdir().unwrap();
    let j = Journal::open(dir.path());
    let effects = Arc::new(AtomicUsize::new(0));
    let entered = Arc::new(std::sync::Barrier::new(2));
    let (j1, e1, b1) = (j.clone(), effects.clone(), entered.clone());
    let first = std::thread::spawn(move || {
        j1.step(&key(), "run-a", &live, &Never, &always, &mut || {
            e1.fetch_add(1, Ordering::SeqCst);
            b1.wait();
            std::thread::sleep(std::time::Duration::from_millis(150));
            Ok(b"billed once".to_vec())
        })
    });
    entered.wait();
    // The claim is in place before the effect runs.
    assert!(matches!(j.row(&key()).unwrap(), Some(Row::Pending { run_id, .. }) if run_id == "run-a"));
    let second = j.step(&key(), "run-b", &live, &Never, &always, &mut || {
        effects.fetch_add(1, Ordering::SeqCst);
        Ok(b"billed twice".to_vec())
    });
    assert_eq!(second.unwrap(), Resolved::Replayed(b"billed once".to_vec()));
    assert_eq!(first.join().unwrap().unwrap(), Resolved::Recorded(b"billed once".to_vec()));
    assert_eq!(effects.load(Ordering::SeqCst), 1);
    // Each segment of the key names its own entry.
    for other in [EntryKey::new("x-2", "pull-0", b"null"), EntryKey::new("x-1", "pull-1", b"null"), EntryKey::new("x-1", "pull-0", b"\"p1\"")] {
        assert!(j.row(&other).unwrap().is_none());
    }
}

/// A pending claim whose run owner lease has expired, {{run.record.owner-lease}}, is taken over by the next
/// caller under that key.
// spec: run.journal.claim-takeover@2cfa59aa
#[test]
fn a_claim_whose_holder_lapsed_is_taken_over() {
    let dir = tempfile::tempdir().unwrap();
    let j = Journal::open(dir.path());
    let _ = std::panic::catch_unwind(AssertUnwindSafe(|| j.step(&key(), "run-a", &live, &Never, &always, &mut || panic!("crash"))));
    let lapsed = std::sync::atomic::AtomicBool::new(false);
    let holder_live = |holder: &str| -> Result<bool, Failure> {
        assert_eq!(holder, "run-a");
        Ok(!lapsed.load(Ordering::SeqCst))
    };
    // While the holder's lease is live, a caller waits; once it lapses, the caller takes over.
    let flip = std::thread::scope(|s| {
        s.spawn(|| {
            std::thread::sleep(std::time::Duration::from_millis(100));
            lapsed.store(true, Ordering::SeqCst);
        });
        j.step(&key(), "run-b", &holder_live, &Never, &always, &mut || Ok(b"taken over".to_vec()))
    });
    assert_eq!(flip.unwrap(), Resolved::Recorded(b"taken over".to_vec()));
}

/// Concurrent writers of one blob hash converge on one stored value without waiting or erroring, and no partial
/// write survives beside it. The file adapter stages a private temporary file and renames it over the destination.
// spec: run.journal.blob-write@f931628e
#[test]
fn concurrent_blob_writers_converge_on_one_file() {
    let dir = tempfile::tempdir().unwrap();
    let j = Journal::open(dir.path());
    let value = vec![7u8; INLINE_CUTOFF_BYTES + 10];
    let sha = sha256_hex(&value);
    std::thread::scope(|s| {
        for _ in 0..8 {
            s.spawn(|| j.write_blob(&sha, &value).unwrap());
        }
    });
    let names: Vec<String> = std::fs::read_dir(j.blob_dir()).unwrap().map(|e| e.unwrap().file_name().to_string_lossy().into_owned()).collect();
    assert_eq!(names, std::slice::from_ref(&sha), "no staged file survives");
    assert_eq!(std::fs::read(j.blob_path(&sha)).unwrap(), value);
}

/// A row whose blob reference resolves to no file raises `BlobMissing` carrying the reference, never an empty
/// value in place of the recorded one.
// spec: run.journal.missing-blob@4d0749cf
#[test]
fn a_missing_blob_refuses_rather_than_reading_empty() {
    let dir = tempfile::tempdir().unwrap();
    let j = Journal::open(dir.path());
    let big = vec![b'x'; INLINE_CUTOFF_BYTES + 1];
    j.step(&key(), "run-a", &live, &Never, &always, &mut || Ok(big.clone())).unwrap();
    let sha = sha256_hex(&big);
    assert!(j.blob_path(&sha).exists(), "the value above the cutoff is a blob");
    assert_eq!(j.step(&key(), "run-b", &live, &Never, &always, &mut || unreachable!()).unwrap(), Resolved::Replayed(big.clone()));
    std::fs::remove_file(j.blob_path(&sha)).unwrap();
    match j.step(&key(), "run-b", &live, &Never, &always, &mut || Ok(Vec::new())) {
        Err(StepError::Journal(RunError::BlobMissing(m))) => assert!(m.contains(&sha), "{m}"),
        other => panic!("{other:?}"),
    }
}

/// A mark-and-sweep pass every 24 h deletes each blob that no journal row or pending awakeable references and
/// that is older than 1 h.
// spec: run.journal.blob-sweep@a62b9f09
#[test]
fn the_sweep_deletes_unreferenced_blobs_past_the_grace_window_once_a_day() {
    assert_eq!((BLOB_SWEEP_INTERVAL_SECS, BLOB_SWEEP_GRACE_SECS), (86_400, 3_600));
    let dir = tempfile::tempdir().unwrap();
    let j = Journal::open(dir.path());
    let recorded = vec![b'r'; INLINE_CUTOFF_BYTES + 1];
    j.step(&key(), "run-a", &live, &Never, &always, &mut || Ok(recorded.clone())).unwrap();
    for name in ["orphan", "awaited"] {
        j.write_blob(name, b"bytes").unwrap();
    }
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs() as i64;
    let awaited = vec!["awaited".to_string()];
    // Inside the grace window nothing goes.
    assert_eq!(j.sweep_if_due(&awaited, now + 60).unwrap(), Some(vec![]));
    // The pass ran; the next is due 24 h later.
    assert_eq!(j.sweep_if_due(&awaited, now + 7_200).unwrap(), None);
    let deleted = j.sweep_if_due(&awaited, now + 60 + 86_400).unwrap().unwrap();
    assert_eq!(deleted, ["orphan"]);
    assert!(j.blob_path(&sha256_hex(&recorded)).exists(), "a blob a journal row names stays");
    assert!(j.blob_path("awaited").exists(), "a blob an awakeable names stays");
    assert!(!j.blob_path("orphan").exists());
}

#[test]
fn a_failed_effect_releases_its_claim_and_an_unjournaled_value_leaves_no_row() {
    let dir = tempfile::tempdir().unwrap();
    let j = Journal::open(dir.path());
    let failed = j.step(&key(), "run-a", &live, &Never, &always, &mut || Err(Failure::new(contextful_core::run::FailureTag::Transient, "reset")));
    assert!(matches!(failed, Err(StepError::Failed(_))));
    assert!(j.row(&key()).unwrap().is_none());
    let empty = j.step(&key(), "run-a", &live, &Never, &|_| false, &mut || Ok(b"{\"rows\":[]}".to_vec())).unwrap();
    assert_eq!(empty, Resolved::Unrecorded(b"{\"rows\":[]}".to_vec()));
    assert!(j.row(&key()).unwrap().is_none());
}

#[test]
fn a_released_claim_keeps_its_lock_file() {
    let dir = tempfile::tempdir().unwrap();
    let j = Journal::open(dir.path());
    let failed = j.step(&key(), "run-a", &live, &Never, &always, &mut || Err(Failure::new(contextful_core::run::FailureTag::Transient, "reset")));
    assert!(failed.is_err());
    let lock = dir.path().join("journal/x-1").join(format!("{}.lock", key().digest()));
    assert!(lock.exists(), "a lock file unlinked under its lock lets two holders each lock their own inode");
}
