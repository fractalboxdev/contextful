//! `surface.dispatch`: the worker adapter's head rule, step result, signed submission,
//! attempt-fenced callback and heartbeat ledger.

use contextful_core::surface::worker::{
    admit_submission, sign_submission, unit_of, Ledger, Signed, StepOutcome, StepResult, CALLBACK_SKEW_SECS, HEARTBEAT_BEAT_SECS, HEARTBEAT_LAPSE_SECS,
    SIGNATURE_HEADER, STEP_RESULT_CAP, TIMESTAMP_HEADER,
};
use std::collections::BTreeSet;
use contextful_core::pipeline::declare::{collect, dependent_runs, ManifestFile};
use contextful_core::run::RunError;
use contextful_core::surface::SurfaceError;
use contextful_core::time::Instant;
use std::collections::BTreeMap;

fn at(secs: u64) -> Instant {
    Instant::parse("2030-01-01T00:00:00Z").unwrap().plus_secs(secs)
}

fn signed(attempt: u32, when: Instant) -> Signed {
    Signed { run: "r1".into(), step: "orders".into(), attempt, timestamp: when.unix_secs() }
}

fn pointer() -> Vec<u8> {
    StepResult::CatalogRow("orders-r1".into()).encode()
}

/// Where entries are landing steps of one dependent run, the run is the unit and cadence
/// rides its head entry. A due id that is a step raises `DispatchUnitNotAHead` and starts
/// nothing.
#[test]
fn a_due_step_of_a_dependent_run_is_not_a_unit() {
    let block = |id: &str, after: Option<&str>| {
        let after = after.map(|a| format!("after = \"{a}\"\n")).unwrap_or_default();
        format!("[[pipeline]]\nid = \"{id}\"\n{after}tables = [\"{id}\"]\n[pipeline.source]\nname = \"http\"\nconfig = {{ endpoint = \"http://127.0.0.1:1/{id}\" }}\n\n")
    };
    let read = |text: String| collect(&[ManifestFile { path: "contextful.toml".into(), text }]).map(|d| d.into_iter().map(|d| d.spec).collect::<Vec<_>>());
    let specs = read(format!("{}{}{}{}", block("orders", None), block("orders-enrich", Some("orders")), block("orders-score", Some("orders-enrich")), block("filings", None))).unwrap();
    let runs = dependent_runs(&specs).unwrap();
    assert_eq!(runs.steps["orders"], ["orders-enrich", "orders-score"], "steps run by distance from the head");
    assert_eq!(runs.head_of["orders-score"], "orders");
    assert!(!runs.steps.contains_key("filings"));
    // An `after` naming nothing declared, or a chain returning to itself, refuses.
    assert!(matches!(read(block("orders-enrich", Some("orders"))), Err(RunError::PipelineSpecInvalid(_))));
    assert!(matches!(read(format!("{}{}", block("a", Some("b")), block("b", Some("a")))), Err(RunError::PipelineSpecInvalid(_))));

    let steps = runs.head_of;
    assert_eq!(unit_of("orders", &steps).unwrap(), "orders");
    assert_eq!(unit_of("filings", &steps).unwrap(), "filings");
    let e = unit_of("orders-enrich", &steps).unwrap_err();
    assert!(matches!(e, SurfaceError::DispatchUnitNotAHead(_)));
    assert!(e.to_string().starts_with("DispatchUnitNotAHead:") && e.to_string().contains("`orders`"), "{e}");
}

/// A completion callback carries a pointer, an object-store key or a catalog row id, or the failed step's last
/// error line, as JSON of at most 1 MiB; any other callback body raises `StepResultRefused` and records nothing.
// spec: surface.dispatch.step-result@79d2718c
#[test]
fn a_step_result_is_a_pointer_under_the_cap() {
    assert_eq!(STEP_RESULT_CAP, 1024 * 1024);
    for r in [StepResult::Pointer("p".into()), StepResult::ObjectKey("runs/r1/a1/out".into()), StepResult::CatalogRow("row-9".into())] {
        assert_eq!(StepResult::decode(&r.encode()).unwrap(), r);
    }
    // A pointer padded to exactly the cap reads; one byte past it refuses.
    let shell = StepResult::Pointer(String::new()).encode().len();
    let fits = StepResult::Pointer("x".repeat(STEP_RESULT_CAP - shell)).encode();
    assert_eq!(fits.len(), STEP_RESULT_CAP);
    assert!(StepResult::decode(&fits).is_ok());
    let over = StepResult::Pointer("x".repeat(STEP_RESULT_CAP - shell + 1)).encode();
    assert!(matches!(StepResult::decode(&over), Err(SurfaceError::StepResultRefused(_))));
    // A failure line is the one other body a callback carries.
    let failed = StepOutcome::Failed { failed: "LeaseHeld: orders".into() };
    assert_eq!(StepOutcome::decode(&failed.encode()).unwrap(), failed);
    assert!(matches!(StepOutcome::decode(&over), Err(SurfaceError::StepResultRefused(_))));
    // Bulk rows in place of a pointer refuse.
    assert!(matches!(StepResult::decode(br#"{"rows":[{"id":1}]}"#), Err(SurfaceError::StepResultRefused(_))));

    // Through the ledger, an oversized callback records nothing and the step stays open.
    let now = at(0);
    let mut l = Ledger::new(vec!["w1".into()]);
    l.submit("r1", "orders", now).unwrap();
    assert!(matches!(l.accept(&signed(1, now), &over, now), Err(SurfaceError::StepResultRefused(_))));
    assert_eq!(l.result("r1", "orders"), None);
    assert!(l.accept(&signed(1, now), &pointer(), now).is_ok());
}

/// The relay accepts a callback timestamp within 300 s of its own clock.
// spec: surface.dispatch.callback-skew@d1b310c7
#[test]
fn the_relay_accepts_a_timestamp_within_the_skew() {
    assert_eq!(CALLBACK_SKEW_SECS, 300);
    let now = at(1_000);
    for offset in [0i64, 300, -300] {
        let mut l = Ledger::new(vec!["w1".into()]);
        l.submit("r1", "orders", now).unwrap();
        let s = Signed { timestamp: now.unix_secs() + offset, ..signed(1, now) };
        assert!(l.accept(&s, &pointer(), now).is_ok(), "offset {offset}");
    }
}

/// A heartbeat or callback whose signature fails, whose attempt is not the step's current attempt, whose
/// timestamp falls outside {{surface.dispatch.callback-skew}}, or whose step has closed, raises
/// `DispatchCallbackRejected` and changes no step.
// spec: surface.dispatch.callback-rejected@877665e1
#[test]
fn a_superseded_or_skewed_callback_changes_no_step() {
    let key = b"worker-key";
    let s = signed(1, at(0));
    let sig = s.sign(key);
    // HMAC-SHA256 over "run\nstep\nattempt\ntimestamp".
    assert_eq!(sig, "39eac3769e7d29d8195aa980704d362a128539bf4c57a4644b2213c93cf25f32");
    assert!(s.verifies(key, &sig));
    assert!(!s.verifies(b"other-key", &sig));
    assert!(!Signed { attempt: 2, ..s.clone() }.verifies(key, &sig), "the attempt is under the signature");
    let headers: BTreeMap<&str, String> = s.headers(key).into_iter().collect();
    let (read, read_sig) = Signed::from_headers(|h| headers.get(h).cloned()).unwrap();
    assert_eq!((read, read_sig), (s.clone(), sig));

    let now = at(0);
    let mut l = Ledger::new(vec!["w1".into(), "w2".into()]);
    l.submit("r1", "orders", now).unwrap();
    // Skewed either way by one second past the bound.
    for offset in [301i64, -301] {
        let skewed = Signed { timestamp: now.unix_secs() + offset, ..signed(1, now) };
        assert!(matches!(l.accept(&skewed, &pointer(), now), Err(SurfaceError::DispatchCallbackRejected(_))));
        assert!(matches!(l.heartbeat(&skewed, now), Err(SurfaceError::DispatchCallbackRejected(_))));
    }
    // An attempt the ledger never issued.
    assert!(matches!(l.accept(&signed(2, now), &pointer(), now), Err(SurfaceError::DispatchCallbackRejected(_))));
    assert_eq!(l.result("r1", "orders"), None);
    // Attempt 1 goes silent and attempt 2 takes the step; attempt 1's late callback is refused.
    let later = now.plus_secs(HEARTBEAT_LAPSE_SECS + 1);
    assert_eq!(l.lapsed(later).len(), 1);
    let e = l.accept(&signed(1, later), &pointer(), later).unwrap_err();
    assert!(e.to_string().starts_with("DispatchCallbackRejected:") && e.to_string().contains("superseded by attempt 2"), "{e}");
    assert_eq!(l.result("r1", "orders"), None);
    assert_eq!(l.attempt("r1", "orders"), Some(2));
    let won = StepResult::CatalogRow("orders-r1-a2".into());
    assert_eq!(l.accept(&signed(2, later), &won.encode(), later).unwrap(), StepOutcome::Done(won.clone()));
    // A repeat of the accepted callback answers the recorded result.
    assert_eq!(l.accept(&signed(2, later), &pointer(), later).unwrap(), StepOutcome::Done(won));
    // Once the step closes, its final attempt and every earlier one are refused alike.
    l.close("r1", "orders");
    let e = l.accept(&signed(2, later), &pointer(), later).unwrap_err();
    assert!(e.to_string().starts_with("DispatchCallbackRejected:") && e.to_string().contains("closed under attempt 2"), "{e}");
    assert!(matches!(l.heartbeat(&signed(2, later), later), Err(SurfaceError::DispatchCallbackRejected(_))));
    let e = l.accept(&signed(1, later), &pointer(), later).unwrap_err();
    assert!(e.to_string().contains("superseded by attempt 2"), "{e}");
    assert!(l.lapsed(later.plus_secs(10 * HEARTBEAT_LAPSE_SECS)).is_empty(), "a closed step never moves");
    // A closed step holds no worker: w1 is least loaded again.
    assert_eq!(l.submit("r1", "filings", later).unwrap().worker, "w1");
}

/// Each worker refusing in turn is excluded from the step's next placement, until none is left.
#[test]
fn a_reassignment_skips_every_excluded_worker() {
    let mut l = Ledger::new(vec!["w1".into(), "w2".into(), "w3".into()]);
    assert_eq!(l.submit("r1", "orders", at(0)).unwrap().worker, "w1");
    let next = l.reassign("r1", "orders", at(0), &BTreeSet::new()).unwrap();
    assert_eq!((next.worker.as_str(), next.attempt), ("w2", 2));
    let next = l.reassign("r1", "orders", at(0), &BTreeSet::from(["w1"])).unwrap();
    assert_eq!((next.worker.as_str(), next.attempt), ("w3", 3));
    assert_eq!(l.reassign("r1", "orders", at(0), &BTreeSet::from(["w1", "w2"])), None);
    assert_eq!(l.attempt("r1", "orders"), Some(3), "a step no worker can take keeps its attempt");
}

/// A submission admits only under the key's HMAC over its timestamp and body, within the skew,
/// naming a callback on the worker's own relay.
#[test]
fn a_submission_is_signed_fresh_and_calls_back_only_to_the_relay() {
    let key = b"worker-key";
    let relay = "http://10.0.0.1:8788";
    let callback = "http://10.0.0.1:8788/awake/0a1b2c";
    let body = br#"{"run":"r1","step":"orders"}"#;
    let now = at(1_000);
    let headers = |key: &[u8], ts: i64, body: &[u8]| -> BTreeMap<&'static str, String> {
        BTreeMap::from([(TIMESTAMP_HEADER, ts.to_string()), (SIGNATURE_HEADER, sign_submission(key, ts, body))])
    };
    let admit = |h: &BTreeMap<&'static str, String>, callback: &str, body: &[u8]| admit_submission(key, relay, callback, |k| h.get(k).cloned(), body, now);
    let fresh = headers(key, now.unix_secs(), body);
    admit(&fresh, callback, body).unwrap();
    admit(&headers(key, now.unix_secs() - CALLBACK_SKEW_SECS as i64, body), callback, body).unwrap();
    let rejected = |r: Result<(), SurfaceError>| matches!(r, Err(SurfaceError::DispatchSubmitRejected(_)));
    assert!(rejected(admit(&BTreeMap::new(), callback, body)), "unsigned");
    assert!(rejected(admit(&headers(b"other", now.unix_secs(), body), callback, body)), "another key");
    assert!(rejected(admit(&fresh, callback, br#"{"run":"r1","step":"filings"}"#)), "the body is under the signature");
    assert!(rejected(admit(&headers(key, now.unix_secs() - CALLBACK_SKEW_SECS as i64 - 1, body), callback, body)), "stale");
    for foreign in ["http://attacker.example/awake/0a1b2c", "http://10.0.0.1:8789/awake/0a1b2c", "http://10.0.0.1:8788/elsewhere", "http://10.0.0.1:8788/awake/", "http://10.0.0.1:8788/awake/0a/../x"] {
        let e = admit(&fresh, foreign, body).unwrap_err();
        assert!(e.to_string().starts_with("DispatchSubmitRejected:") && e.to_string().contains("[control] relay"), "{foreign}: {e}");
    }
}

/// A worker heartbeat, a signed `GET` on the step's awakeable route, runs every 15 s.
#[test]
fn heartbeats_every_fifteen_seconds_keep_a_step_in_place() {
    assert_eq!(HEARTBEAT_BEAT_SECS, 15);
    let mut l = Ledger::new(vec!["w1".into(), "w2".into()]);
    l.submit("r1", "orders", at(0)).unwrap();
    for beat in 1..=20 {
        let now = at(beat * HEARTBEAT_BEAT_SECS);
        l.heartbeat(&signed(1, now), now).unwrap();
        assert!(l.lapsed(now).is_empty(), "a beating worker keeps its step at {beat}");
    }
    assert_eq!(l.attempt("r1", "orders"), Some(1));
}

/// A heartbeat lapse past 60 s reschedules that worker's in-flight work onto another
/// matching worker under the next attempt number.
#[test]
fn a_lapse_moves_the_step_to_another_worker_under_the_next_attempt() {
    assert_eq!(HEARTBEAT_LAPSE_SECS, 60);
    let mut l = Ledger::new(vec!["w1".into(), "w2".into()]);
    let first = l.submit("r1", "orders", at(0)).unwrap();
    assert_eq!((first.attempt, first.worker.as_str()), (1, "w1"));
    let other = l.submit("r1", "filings", at(0)).unwrap();
    assert_eq!(other.worker, "w2", "the least-loaded worker takes the next step");
    l.heartbeat(&Signed { step: "filings".into(), ..signed(1, at(50)) }, at(50)).unwrap();
    // Exactly 60 s of silence keeps the step; one second more moves it, once.
    assert!(l.lapsed(at(60)).is_empty());
    let moved = l.lapsed(at(61));
    assert_eq!(moved.len(), 1);
    assert_eq!((moved[0].step.as_str(), moved[0].attempt, moved[0].worker.as_str()), ("orders", 2, "w2"));
    assert!(l.lapsed(at(62)).is_empty(), "the new attempt starts its own heartbeat window");
    // With no other worker, a silent step stays where it is.
    let mut alone = Ledger::new(vec!["w1".into()]);
    alone.submit("r1", "orders", at(0)).unwrap();
    assert!(alone.lapsed(at(120)).is_empty());
    assert_eq!(alone.attempt("r1", "orders"), Some(1));
}
