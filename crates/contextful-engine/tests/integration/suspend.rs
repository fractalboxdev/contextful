//! `run.suspend`: the persisted awakeable registry.

use crate::support::at;
use contextful_core::run::journal::{sha256_hex, INLINE_CUTOFF_BYTES};
use contextful_core::run::suspend::AwakeableState;
use contextful_core::run::RunError;
use contextful_engine::awake::{AwakeError, Awaited, Registry};
use contextful_engine::Journal;

fn registry(dir: &std::path::Path) -> Registry {
    Registry::open(dir, Journal::open(dir))
}

fn files(dir: &std::path::Path) -> Vec<String> {
    let mut out = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        for e in std::fs::read_dir(&d).into_iter().flatten().flatten() {
            if e.path().is_dir() {
                stack.push(e.path());
            } else {
                out.push(e.path().strip_prefix(dir).unwrap().to_string_lossy().into_owned());
            }
        }
    }
    out.sort();
    out
}

/// An awakeable suspends a run durably: the engine mints an opaque single-use token and persists a `pending` row
/// through the awakeable store; an external party resumes by posting the token back.
// spec: run.suspend.awakeable@58dd40ee
#[test]
fn a_minted_token_persists_a_pending_row_and_resumes_on_post() {
    let dir = tempfile::tempdir().unwrap();
    let r = registry(dir.path());
    let a = r.suspend("x-1", "approve", "2030-01-01T00:00:00Z", 600).unwrap();
    let b = r.suspend("x-1", "approve", "2030-01-01T00:00:00Z", 600).unwrap();
    assert_ne!(a.token, b.token, "each token is fresh");
    assert_eq!(a.token.len(), 32);
    assert_eq!(files(dir.path()), [format!("awakeables/{}.json", a.token), format!("awakeables/{}.json", b.token)].iter().cloned().collect::<std::collections::BTreeSet<_>>().into_iter().collect::<Vec<_>>());
    assert_eq!(r.state(&a.token, at("2030-01-01T00:01:00Z")).unwrap().state, AwakeableState::Pending);
    assert_eq!(r.awaited("x-1", &a.token, at("2030-01-01T00:01:00Z")).unwrap(), Awaited::Pending);
    assert_eq!(r.resolve(&a.token, b"approved", at("2030-01-01T00:02:00Z")).unwrap(), b"approved");
    assert_eq!(r.awaited("x-1", &a.token, at("2030-01-01T00:02:00Z")).unwrap(), Awaited::Resumed(b"approved".to_vec()));
}

/// A resume payload is recorded as the awaited step's output under key `sha256("awakeable:" + token)`, so a run
/// that resumes and then crashes reads it back without suspending again.
// spec: run.suspend.resume-is-a-step-output@fdd69cd4
#[test]
fn a_resumed_run_reads_its_payload_back_from_the_journal() {
    let dir = tempfile::tempdir().unwrap();
    let r = registry(dir.path());
    let a = r.suspend("x-1", "approve", "2030-01-01T00:00:00Z", 60).unwrap();
    r.resolve(&a.token, b"{\"ok\":true}", at("2030-01-01T00:00:10Z")).unwrap();
    let key = contextful_engine::awake::resume_key("x-1", &a.token);
    assert_eq!(key.input_hash, sha256_hex(format!("awakeable:{}", a.token).as_bytes()));
    assert!(Journal::open(dir.path()).row(&key).unwrap().is_some(), "the payload is a journal entry");
    // After a crash, a fresh registry reads the recorded payload, even past the deadline.
    let reopened = registry(dir.path());
    assert_eq!(reopened.awaited("x-1", &a.token, at("2030-01-01T05:00:00Z")).unwrap(), Awaited::Resumed(b"{\"ok\":true}".to_vec()));
}

/// A token with no row raises `AwakeableUnknown` and allocates no state.
// spec: run.suspend.unknown-token@7024f53a
#[test]
fn an_unknown_token_is_refused_and_writes_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let r = registry(dir.path());
    // A known token exists, so the registry is not empty when the unknown one is refused.
    let known = r.suspend("x-1", "approve", "2030-01-01T00:00:00Z", 60).unwrap();
    let before = files(dir.path());
    for token in ["0123456789abcdef0123456789abcdef", "../escape", ""] {
        assert!(matches!(r.resolve(token, b"x", at("2030-01-01T00:00:01Z")), Err(AwakeError::Refused(RunError::AwakeableUnknown(_)))), "{token}");
        assert!(matches!(r.state(token, at("2030-01-01T00:00:01Z")), Err(AwakeError::Refused(RunError::AwakeableUnknown(_)))));
    }
    assert_eq!(files(dir.path()), before);
    assert!(before.contains(&format!("awakeables/{}.json", known.token)));
}

/// A payload above {{run.journal.inline-cutoff}} lands in the journal's blob store, and the pending row
/// references it.
// spec: run.suspend.payload-offload@533957e6
#[test]
fn a_payload_above_the_cutoff_lands_in_the_blob_store() {
    let dir = tempfile::tempdir().unwrap();
    let r = registry(dir.path());
    let a = r.suspend("x-1", "upload", "2030-01-01T00:00:00Z", 60).unwrap();
    let big = vec![b'p'; INLINE_CUTOFF_BYTES + 1];
    assert_eq!(r.resolve(&a.token, &big, at("2030-01-01T00:00:01Z")).unwrap(), big);
    let row = r.state(&a.token, at("2030-01-01T00:00:02Z")).unwrap();
    let sha = sha256_hex(&big);
    assert_eq!(row.payload.as_ref().and_then(|p| p.blob()), Some(sha.as_str()));
    assert!(Journal::open(dir.path()).blob_path(&sha).exists());
    assert_eq!(r.referenced_blobs().unwrap(), [sha]);
    let raw = std::fs::read_to_string(dir.path().join("awakeables").join(format!("{}.json", a.token))).unwrap();
    assert!(raw.len() < 4096, "the row holds a reference, not the payload");
}

/// A durable awakeable store persists every registry row, so a process reopening it over the same location drops no
/// pending callback.
// spec: run.suspend.survives-restart@24a03fda
#[test]
fn a_restart_keeps_every_pending_callback() {
    let dir = tempfile::tempdir().unwrap();
    let token = registry(dir.path()).suspend("x-1", "approve", "2030-01-01T00:00:00Z", 60).unwrap().token;
    // A new process opens the same directory.
    let after = registry(dir.path());
    assert_eq!(after.state(&token, at("2030-01-01T00:00:30Z")).unwrap().state, AwakeableState::Pending);
    assert_eq!(after.resolve(&token, b"late but in time", at("2030-01-01T00:00:59Z")).unwrap(), b"late but in time");
    // A timeout observed at resolution persists as timed out.
    let expiring = after.suspend("x-2", "approve", "2030-01-01T00:00:00Z", 1).unwrap().token;
    assert!(matches!(after.resolve(&expiring, b"x", at("2030-01-01T00:00:02Z")), Err(AwakeError::Refused(RunError::AwakeableTimedOut(_)))));
    let raw: serde_json::Value = serde_json::from_slice(&std::fs::read(dir.path().join("awakeables").join(format!("{expiring}.json"))).unwrap()).unwrap();
    assert_eq!(raw["state"], "timed_out");
}
