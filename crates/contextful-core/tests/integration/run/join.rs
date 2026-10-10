//! `run.journal.fan-out-join` and the run-path rules the core states over plain data:
//! the join node, the lateness rewind, the caller binding and the admission pin.

use super::row;
use contextful_core::run::advance::rewind;
use contextful_core::run::join::{FailedBranch, FanOut, Join};
use contextful_core::run::journal::{sha256_hex, EntryKey, Row};
use contextful_core::run::own::{admission_pin, resolve_pinned, Artifacts, ConnectorPin, ExecutionOwner, OwnerScope, Pins};
use contextful_core::run::suspend::Awakeable;
use contextful_core::run::{Failure, FailureTag, RunError};
use serde_json::json;

fn join(allow_partial: bool) -> Join {
    Join { label: "merge".into(), allow_partial }
}

fn fanned() -> FanOut<&'static str> {
    FanOut::new()
        .branch("en", Ok("hello"))
        .branch("fr", Err(Failure::new(FailureTag::RateLimited, "the vendor throttled")))
        .branch("de", Ok("hallo"))
}

/// Fan-out bodies rejoin only at an explicit join node; a join reached by a failed branch fails the run unless it
/// declares `allow_partial`, which records each failed branch's label and failure tag on the run record.
// spec: run.journal.fan-out-join@ca72f881
#[test]
fn a_failed_branch_fails_its_join_unless_the_join_allows_partial() {
    // Every branch succeeding passes every output on, labelled, in branch order.
    let whole = FanOut::new().branch("en", Ok(1)).branch("de", Ok(2)).join(&join(false)).unwrap();
    assert_eq!(whole.outputs, [("en".to_string(), 1), ("de".to_string(), 2)]);
    assert!(whole.failed.is_empty());

    // A failed branch fails a join declaring no `allow_partial`, keeping the branch's tag.
    let failed = fanned().join(&join(false)).unwrap_err();
    assert_eq!(failed.tag, FailureTag::RateLimited);
    assert!(failed.message.contains("branch `fr`") && failed.message.contains("join `merge`"), "{}", failed.message);

    // A partial join passes the others on and records the failed branch on the run record.
    let partial = fanned().join(&join(true)).unwrap();
    assert_eq!(partial.outputs, [("en".to_string(), "hello"), ("de".to_string(), "hallo")]);
    let mut record = row("run-1", "2030-01-01T00:00:00Z");
    assert!(record.failed_branches.is_empty());
    partial.record(&mut record);
    assert_eq!(record.failed_branches, [FailedBranch { label: "fr".into(), tag: FailureTag::RateLimited }]);
    let stored = serde_json::to_value(&record).unwrap();
    assert_eq!(stored["failed_branches"], json!([{ "label": "fr", "tag": "RateLimited" }]), "the record carries label and tag");
    let complete = serde_json::to_value(row("run-2", "2030-01-01T00:00:00Z")).unwrap();
    assert!(complete.get("failed_branches").is_none(), "a complete run records no failed branch");
}

#[test]
fn a_rewind_moves_an_instant_or_a_number_back_and_refuses_other_clocks() {
    assert_eq!(rewind(&json!("2030-01-01T00:00:30Z"), 0).unwrap(), json!("2030-01-01T00:00:30Z"));
    assert_eq!(rewind(&json!("2030-01-01T00:00:30Z"), 60).unwrap(), json!("2029-12-31T23:59:30Z"));
    assert_eq!(rewind(&json!(10), 60).unwrap(), json!(-50));
    assert_eq!(rewind(&json!("v-7"), 0).unwrap(), json!("v-7"), "a zero window leaves any clock alone");
    assert!(matches!(rewind(&json!("v-7"), 60), Err(RunError::CursorPositionUnorderable(_))));
}

#[test]
fn a_bound_awakeable_answers_only_its_subject() {
    let open = Awakeable::mint("tok-1", "x-1", "approve", "2030-01-01T00:00:00Z", 60).unwrap();
    assert!(open.answers(None) && open.answers(Some("anyone")), "an unbound token is its whole authority");
    let bound = open.bound_to("reviewer");
    assert!(bound.answers(Some("reviewer")));
    assert!(!bound.answers(None) && !bound.answers(Some("other")));
    let back: Awakeable = serde_json::from_value(serde_json::to_value(&bound).unwrap()).unwrap();
    assert_eq!(back.caller.as_deref(), Some("reviewer"), "the binding persists with the row");
}

#[test]
fn a_pending_row_carries_its_closed_attempts() {
    let key = EntryKey::new("x-1", "pull-0", b"null");
    let fresh = Row::pending(&key, "run-a");
    assert_eq!(fresh, Row::Pending { key: key.clone(), run_id: "run-a".into(), attempts: 0 });
    assert!(serde_json::to_value(&fresh).unwrap().get("attempts").is_none(), "a fresh claim stores as before");
    let counted = Row::Pending { key, run_id: "run-a".into(), attempts: 2 };
    assert_eq!(serde_json::from_value::<Row>(serde_json::to_value(&counted).unwrap()).unwrap(), counted);
}

struct Held(Vec<u8>);

impl Artifacts for Held {
    fn by_hash(&self, hash: &str) -> Result<Option<Vec<u8>>, Failure> {
        Ok((sha256_hex(&self.0) == hash || hash == "moved").then(|| self.0.clone()))
    }
}

#[test]
fn a_pending_owner_s_pin_names_the_artifact_a_replay_resolves() {
    let pin = |hash: &str| ConnectorPin { id: "vendor".into(), version: "1.0.0".into(), world: "native".into(), hash: hash.into() };
    let held = Held(b"build 1".to_vec());
    let recorded = pin(&sha256_hex(b"build 1"));
    let owner = ExecutionOwner {
        execution_id: "x-1".into(),
        scope: OwnerScope::table("feed", "filings"),
        pins: Pins { connector: recorded.clone(), content_hash: "plan".into(), input_hash: String::new() }.into(),
        attempts: vec!["run-1".into()],
        opened_at: super::at("2030-01-01T00:00:00Z"),
    };
    let rebuilt = pin(&sha256_hex(b"build 2"));
    assert_eq!(admission_pin(Some(&owner), &rebuilt), &recorded);
    assert_eq!(admission_pin(None, &rebuilt), &rebuilt);
    assert_eq!(resolve_pinned(&recorded, &held).unwrap(), b"build 1");
    assert_eq!(resolve_pinned(&rebuilt, &held).unwrap_err().tag, FailureTag::UnknownConnector);
    assert!(resolve_pinned(&pin("moved"), &held).unwrap_err().message.contains("hashes to"));
}
