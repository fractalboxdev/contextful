//! The conformance suites every store adapter runs, one per port
//! (`run.journal.storage-ports`): each `run.journal` and `run.suspend` rule the port
//! carries, checked against fresh stores from the adapter's factory. A check panics on the
//! first divergence, naming the adapter and the rule.
//!
//! A factory hands back empty stores on every call; a clone of a store shares its state.
//! The restart check reopens a durable adapter over its location instead of cloning it.

use crate::awake::{resume_key, AwakeError, Awaited, Registry};
use crate::journal::{Journal, Resolved, StepError};
use contextful_core::run::journal::{sha256_hex, EntryKey, Row, Stored, BLOB_SWEEP_GRACE_SECS, BLOB_SWEEP_INTERVAL_SECS, INLINE_CUTOFF_BYTES};
use contextful_core::run::ports::{AwakeableStore, BlobStore, Cancellation, JournalStore};
use contextful_core::run::suspend::AwakeableState;
use contextful_core::run::{Failure, FailureTag, RunError};
use contextful_core::time::Instant;
use std::sync::atomic::{AtomicUsize, Ordering};

struct Never;

impl Cancellation for Never {
    fn requested(&self) -> bool {
        false
    }
}

struct Stopped;

impl Cancellation for Stopped {
    fn requested(&self) -> bool {
        true
    }
}

fn always(_: &[u8]) -> bool {
    true
}

fn live(_: &str) -> Result<bool, Failure> {
    Ok(true)
}

fn dead(_: &str) -> Result<bool, Failure> {
    Ok(false)
}

fn now_unix() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0)
}

fn key(execution_id: &str) -> EntryKey {
    EntryKey::new(execution_id, "pull-0", b"null")
}

fn at(s: &str) -> Instant {
    Instant::parse(s).expect("a literal instant")
}

fn large(fill: u8) -> Vec<u8> {
    vec![fill; INLINE_CUTOFF_BYTES + 1]
}

/// The [`BlobStore`] suite: round trip, converging writers, the sweep's references and
/// grace, and the sweep stamp.
pub fn blob_store<B: BlobStore + Clone>(adapter: &str, fresh: &mut dyn FnMut() -> B) {
    // A put reads back; an absent hash reads as none.
    let b = fresh();
    b.put("aa", b"bytes").unwrap();
    assert_eq!(b.get("aa").unwrap().as_deref(), Some(&b"bytes"[..]), "{adapter}: a put blob reads back");
    assert_eq!(b.get("bb").unwrap(), None, "{adapter}: an absent blob reads as none");
    assert_eq!(b.clone().get("aa").unwrap().as_deref(), Some(&b"bytes"[..]), "{adapter}: a reopened store holds the blob");

    // run.journal.blob-write: concurrent writers of one hash converge, none erroring.
    let b = fresh();
    let value = large(b'v');
    let sha = sha256_hex(&value);
    std::thread::scope(|s| {
        for _ in 0..8 {
            s.spawn(|| b.put(&sha, &value).unwrap_or_else(|e| panic!("{adapter}: a converging writer errored: {e}")));
        }
    });
    assert_eq!(b.get(&sha).unwrap().as_deref(), Some(&value[..]), "{adapter}: converging writers leave one whole value");

    // run.journal.blob-sweep: an unreferenced blob goes only past the grace window.
    let b = fresh();
    let now = now_unix();
    for sha in ["orphan", "kept"] {
        b.put(sha, b"bytes").unwrap();
    }
    let kept = vec!["kept".to_string()];
    assert_eq!(b.sweep(&kept, now + 60).unwrap(), Vec::<String>::new(), "{adapter}: a blob inside the grace window survives");
    let grace = i64::try_from(BLOB_SWEEP_GRACE_SECS).unwrap_or(i64::MAX);
    assert_eq!(b.sweep(&kept, now + grace + 60).unwrap(), ["orphan"], "{adapter}: an unreferenced blob past the grace window goes");
    assert!(b.get("kept").unwrap().is_some(), "{adapter}: a referenced blob survives every sweep");
    assert!(b.get("orphan").unwrap().is_none());

    // The sweep stamp persists.
    let b = fresh();
    assert_eq!(b.swept_at().unwrap(), None, "{adapter}: a fresh store has never swept");
    b.mark_swept(now).unwrap();
    assert_eq!(b.clone().swept_at().unwrap(), Some(now), "{adapter}: the sweep stamp persists");
}

/// The [`JournalStore`] suite, run through the journal over the adapter pair: the
/// port's own atomicity, then exactly-once record, claim wait and dead-holder takeover,
/// release on failure, inline placement, `BlobMissing`, collection and the daily sweep.
pub fn journal_store<J: JournalStore + Clone, B: BlobStore + Clone>(adapter: &str, fresh: &mut dyn FnMut() -> (J, B)) {
    let journal = |fresh: &mut dyn FnMut() -> (J, B)| {
        let (rows, blobs) = fresh();
        Journal::over(rows, blobs)
    };

    // The port: a claim is exclusive, a takeover names its holder, the first record stands,
    // and a release drops only its own claim.
    let (rows, _) = fresh();
    let k = key("x-1");
    assert!(rows.create_pending(&k, "run-a").unwrap(), "{adapter}: the first claim lands");
    assert!(!rows.create_pending(&k, "run-b").unwrap(), "{adapter}: a second claim finds the first");
    assert!(!rows.replace_if_pending(&k, "run-z", "run-b").unwrap(), "{adapter}: a takeover naming another holder writes nothing");
    assert!(rows.replace_if_pending(&k, "run-a", "run-b").unwrap(), "{adapter}: a takeover naming the holder passes the claim");
    rows.release(&k, "run-a").unwrap();
    assert_eq!(rows.read(&k).unwrap(), Some(Row::pending(&k, "run-b")), "{adapter}: a release drops only its own claim");
    let first = Stored::place(b"first");
    assert_eq!(rows.record(&k, &first).unwrap(), None, "{adapter}: a record over a claim lands");
    assert_eq!(rows.record(&k, &Stored::place(b"second")).unwrap(), Some(first.clone()), "{adapter}: the first record stands");
    assert!(!rows.replace_if_pending(&k, "run-b", "run-c").unwrap(), "{adapter}: a recorded row admits no takeover");
    rows.release(&k, "run-b").unwrap();
    assert_eq!(rows.clone().read(&k).unwrap(), Some(Row::Recorded { key: k.clone(), value: first }), "{adapter}: a release leaves a recorded row");

    // run.journal.step-output and entry-key: racing callers run the effect once and read one value.
    let j = journal(fresh);
    let effects = AtomicUsize::new(0);
    let resolved: Vec<Resolved> = std::thread::scope(|s| {
        let handles: Vec<_> = (0..8)
            .map(|i| {
                let (j, effects) = (&j, &effects);
                s.spawn(move || {
                    j.step(&key("x-1"), &format!("run-{i}"), &live, &Never, &always, &mut || {
                        effects.fetch_add(1, Ordering::SeqCst);
                        std::thread::sleep(std::time::Duration::from_millis(30));
                        Ok(b"once".to_vec())
                    })
                    .unwrap()
                })
            })
            .collect();
        handles.into_iter().map(|h| h.join().unwrap()).collect()
    });
    assert_eq!(effects.load(Ordering::SeqCst), 1, "{adapter}: racing callers ran the effect more than once");
    assert_eq!(resolved.iter().filter(|r| matches!(r, Resolved::Recorded(_))).count(), 1, "{adapter}: exactly one caller records");
    assert!(resolved.iter().all(|r| r.bytes() == b"once"), "{adapter}: every caller reads the recorded value");

    // run.journal.claim-takeover: a live holder's claim is waited on, a dead one's taken over.
    let j = journal(fresh);
    assert!(j.row_store().create_pending(&key("x-1"), "run-a").unwrap());
    let waited = j.step(&key("x-1"), "run-b", &live, &Stopped, &always, &mut || panic!("{adapter}: a live holder's claim was computed over"));
    assert!(matches!(waited, Err(StepError::Failed(f)) if f.tag == FailureTag::Canceled), "{adapter}: a live holder's claim is waited on");
    let taken = j.step(&key("x-1"), "run-b", &dead, &Never, &always, &mut || Ok(b"taken over".to_vec())).unwrap();
    assert_eq!(taken, Resolved::Recorded(b"taken over".to_vec()), "{adapter}: a dead holder's claim is taken over");

    // A failed effect releases its claim; an unjournaled value leaves no row, and an
    // execution whose every claim was released is no execution.
    let j = journal(fresh);
    let failed = j.step(&key("x-2"), "run-a", &live, &Never, &always, &mut || Err(Failure::new(FailureTag::Transient, "reset")));
    assert!(matches!(failed, Err(StepError::Failed(_))), "{adapter}: a failed effect fails the step");
    assert!(j.row(&key("x-2")).unwrap().is_none(), "{adapter}: a failed effect releases its claim");
    let unjournaled = j.step(&key("x-1"), "run-a", &live, &Never, &|_| false, &mut || Ok(b"{}".to_vec())).unwrap();
    assert_eq!(unjournaled, Resolved::Unrecorded(b"{}".to_vec()));
    assert!(j.row(&key("x-1")).unwrap().is_none(), "{adapter}: an unjournaled value leaves no row");
    assert_eq!(j.row_store().executions().unwrap(), Vec::<String>::new(), "{adapter}: only an execution holding a row lists");

    // run.journal.inline-cutoff and missing-blob.
    let j = journal(fresh);
    let small = vec![b's'; INLINE_CUTOFF_BYTES];
    j.step(&key("x-1"), "run-a", &live, &Never, &always, &mut || Ok(small.clone())).unwrap();
    assert!(matches!(j.row(&key("x-1")).unwrap(), Some(Row::Recorded { value, .. }) if value.blob().is_none()), "{adapter}: a value at the cutoff is inline");
    let value = large(b'l');
    let sha = sha256_hex(&value);
    j.step(&key("x-2"), "run-a", &live, &Never, &always, &mut || Ok(value.clone())).unwrap();
    assert!(matches!(j.row(&key("x-2")).unwrap(), Some(Row::Recorded { value, .. }) if value.blob() == Some(sha.as_str())), "{adapter}: a larger value is a sha256 blob");
    assert_eq!(j.blob_store().get(&sha).unwrap().as_deref(), Some(&value[..]));
    let grace = i64::try_from(BLOB_SWEEP_GRACE_SECS).unwrap_or(i64::MAX);
    j.blob_store().sweep(&[], now_unix() + grace + 60).unwrap();
    match j.step(&key("x-2"), "run-b", &live, &Never, &always, &mut || panic!("{adapter}: a recorded value was recomputed")) {
        Err(StepError::Journal(RunError::BlobMissing(m))) => assert!(m.contains(&sha), "{adapter}: `BlobMissing` names the reference: {m}"),
        other => panic!("{adapter}: a missing blob read as {other:?}"),
    }

    // run.retry.attempt-counter: a schedule's closed attempts persist on its pending row,
    // survive a takeover, and leave with a release or a record.
    let j = journal(fresh);
    let k = key("x-3");
    assert_eq!(j.attempts(&k).unwrap(), 0, "{adapter}: a step with no row has closed no attempt");
    j.note_attempts(&k, "run-a", 2).unwrap();
    assert_eq!(j.row(&k).unwrap(), Some(Row::Pending { key: k.clone(), run_id: "run-a".into(), attempts: 2 }), "{adapter}: the count persists on a pending row");
    j.note_attempts(&k, "run-z", 4).unwrap();
    assert_eq!(j.attempts(&k).unwrap(), 2, "{adapter}: another run's count leaves the claim alone");
    assert!(j.row_store().replace_if_pending(&k, "run-a", "run-b").unwrap());
    assert_eq!(j.attempts(&k).unwrap(), 2, "{adapter}: a takeover keeps the count");
    j.note_attempts(&k, "run-b", 3).unwrap();
    assert_eq!(j.attempts(&k).unwrap(), 3, "{adapter}: the holder's count moves");
    j.row_store().release(&k, "run-b").unwrap();
    assert!(j.row_store().create_pending(&k, "run-c").unwrap(), "{adapter}: a release drops the counted claim");
    assert_eq!(j.attempts(&k).unwrap(), 0, "{adapter}: a fresh claim starts its count at zero");
    j.note_attempts(&k, "run-c", 1).unwrap();
    assert_eq!(j.row_store().record(&k, &Stored::place(b"v")).unwrap(), None);
    j.note_attempts(&k, "run-c", 2).unwrap();
    assert!(matches!(j.row(&k).unwrap(), Some(Row::Recorded { .. })), "{adapter}: a recorded row takes no count");
    assert_eq!(j.attempts(&k).unwrap(), 0, "{adapter}: a recorded step carries no count");

    // run.journal.collection: retiring an execution deletes its rows and no other's.
    let j = journal(fresh);
    for execution in ["x-1", "x-2"] {
        j.step(&key(execution), "run-a", &live, &Never, &always, &mut || Ok(b"v".to_vec())).unwrap();
    }
    j.collect("x-1").unwrap();
    j.collect("x-9").unwrap();
    assert_eq!((j.recorded("x-1").unwrap(), j.recorded("x-2").unwrap()), (0, 1), "{adapter}: retirement deletes only its execution's rows");
    assert_eq!(j.row_store().executions().unwrap(), ["x-2"], "{adapter}: a retired execution holds no rows");

    // run.journal.blob-sweep: once a day, sparing what a row or an awakeable references.
    assert_eq!((BLOB_SWEEP_INTERVAL_SECS, BLOB_SWEEP_GRACE_SECS), (86_400, 3_600));
    let j = journal(fresh);
    let recorded = large(b'r');
    j.step(&key("x-1"), "run-a", &live, &Never, &always, &mut || Ok(recorded.clone())).unwrap();
    for name in ["orphan", "awaited"] {
        j.write_blob(name, b"bytes").unwrap();
    }
    let now = now_unix();
    let awaited = vec!["awaited".to_string()];
    assert_eq!(j.sweep_if_due(&awaited, now + 60).unwrap(), Some(vec![]), "{adapter}: a blob inside the grace window survives");
    assert_eq!(j.sweep_if_due(&awaited, now + 7_200).unwrap(), None, "{adapter}: the next pass waits 24 h");
    assert_eq!(j.sweep_if_due(&awaited, now + 60 + 86_400).unwrap(), Some(vec!["orphan".to_string()]), "{adapter}: the daily pass takes the orphan");
    assert!(j.blob_store().get(&sha256_hex(&recorded)).unwrap().is_some(), "{adapter}: a blob a journal row names stays");
    assert!(j.blob_store().get("awaited").unwrap().is_some(), "{adapter}: a blob an awakeable names stays");
}

/// The [`AwakeableStore`] suite, run through the registry over the adapter and a journal:
/// the token's single resolution, its deadline and payload offload.
pub fn awakeable_store<A: AwakeableStore + Clone, J: JournalStore + Clone, B: BlobStore + Clone>(adapter: &str, fresh: &mut dyn FnMut() -> (A, J, B)) {
    let (store, rows, blobs) = fresh();
    let r = Registry::over(store.clone(), Journal::over(rows.clone(), blobs.clone()));

    // run.suspend.unknown-token: an unknown token refuses and allocates nothing.
    for token in ["0123abcd", "../escape"] {
        assert!(matches!(r.resolve(token, b"x", at("2030-01-01T00:00:00Z")), Err(AwakeError::Refused(RunError::AwakeableUnknown(_)))), "{adapter}: `{token}` is unknown");
        assert!(matches!(r.state(token, at("2030-01-01T00:00:00Z")), Err(AwakeError::Refused(RunError::AwakeableUnknown(_)))));
    }
    assert!(store.update("0123abcd", &mut |_| Ok(true)).unwrap().is_none(), "{adapter}: an update of an unknown token finds nothing");
    assert!(store.rows().unwrap().is_empty(), "{adapter}: an unknown token allocates no row");

    // run.suspend.awakeable and resume-is-a-step-output.
    let a = r.suspend("x-1", "approve", "2030-01-01T00:00:00Z", 60).unwrap();
    assert_eq!(store.get(&a.token).unwrap().map(|row| row.state), Some(AwakeableState::Pending), "{adapter}: the pending row persists");
    assert_eq!(r.awaited("x-1", &a.token, at("2030-01-01T00:00:10Z")).unwrap(), Awaited::Pending);
    assert_eq!(r.resolve(&a.token, b"yes", at("2030-01-01T00:00:10Z")).unwrap(), b"yes");
    let journal = Journal::over(rows.clone(), blobs.clone());
    assert!(matches!(journal.row(&resume_key("x-1", &a.token)).unwrap(), Some(Row::Recorded { .. })), "{adapter}: the payload is a journal entry under the resume key");
    // run.suspend.idempotent-resolution and conflicting-resolution.
    assert_eq!(r.resolve(&a.token, b"yes", at("2030-01-01T00:00:20Z")).unwrap(), b"yes", "{adapter}: the identical payload answers the recorded value");
    assert!(matches!(r.resolve(&a.token, b"no", at("2030-01-01T00:00:20Z")), Err(AwakeError::Refused(RunError::AwakeableAlreadyResolved(_)))));
    assert_eq!(r.awaited("x-1", &a.token, at("2030-01-01T00:00:30Z")).unwrap(), Awaited::Resumed(b"yes".to_vec()), "{adapter}: a conflicting payload leaves the recorded one");

    // run.suspend.expired-token and timeout-is-sticky: an expiry observed at resolution persists.
    let e = r.suspend("x-2", "approve", "2030-01-01T00:00:00Z", 1).unwrap();
    assert!(matches!(r.resolve(&e.token, b"late", at("2030-01-01T00:00:02Z")), Err(AwakeError::Refused(RunError::AwakeableTimedOut(_)))));
    assert_eq!(store.get(&e.token).unwrap().map(|row| row.state), Some(AwakeableState::TimedOut), "{adapter}: the timeout persists");
    assert_eq!(r.awaited("x-2", &e.token, at("2030-01-01T00:00:00Z")).unwrap(), Awaited::TimedOut, "{adapter}: a timeout is sticky under an earlier instant");

    // run.suspend.payload-offload: a payload above the cutoff lands as a blob the row references.
    let big = r.suspend("x-3", "upload", "2030-01-01T00:00:00Z", 60).unwrap();
    let payload = large(b'p');
    let sha = sha256_hex(&payload);
    assert_eq!(r.resolve(&big.token, &payload, at("2030-01-01T00:00:10Z")).unwrap(), payload);
    assert_eq!(blobs.get(&sha).unwrap().as_deref(), Some(&payload[..]), "{adapter}: the payload is a blob");
    assert!(r.referenced_blobs().unwrap().contains(&sha), "{adapter}: the row references the blob the sweep spares");

    assert_eq!(store.rows().unwrap().len(), 3, "{adapter}: one row per minted token");
}

/// The restart check of `run.suspend.survives-restart`, for a durable adapter: each call
/// of `open` opens the adapter's stores over one fixed location, as a new process does.
/// An in-process adapter has no location to reopen and runs no restart check.
pub fn awakeable_restart<A: AwakeableStore, J: JournalStore, B: BlobStore>(adapter: &str, open: &mut dyn FnMut() -> (A, J, B)) {
    let registry = |open: &mut dyn FnMut() -> (A, J, B)| {
        let (store, rows, blobs) = open();
        Registry::over(store, Journal::over(rows, blobs))
    };
    let before = registry(open);
    let pending = before.suspend("x-1", "approve", "2030-01-01T00:00:00Z", 60).unwrap();
    let resolved = before.suspend("x-2", "approve", "2030-01-01T00:00:00Z", 60).unwrap();
    let payload = large(b'r');
    assert_eq!(before.resolve(&resolved.token, &payload, at("2030-01-01T00:00:10Z")).unwrap(), payload);
    drop(before);

    let after = registry(open);
    assert_eq!(after.state(&pending.token, at("2030-01-01T00:00:30Z")).unwrap().state, AwakeableState::Pending, "{adapter}: a restart drops no pending callback");
    assert_eq!(after.awaited("x-2", &resolved.token, at("2030-01-01T00:00:30Z")).unwrap(), Awaited::Resumed(payload.clone()), "{adapter}: a resolution survives a restart");
    assert_eq!(after.resolve(&pending.token, b"after", at("2030-01-01T00:00:59Z")).unwrap(), b"after");
    drop(after);

    let last = registry(open);
    assert_eq!(last.awaited("x-1", &pending.token, at("2030-01-01T00:01:30Z")).unwrap(), Awaited::Resumed(b"after".to_vec()), "{adapter}: a resolution after a restart persists");
}
