//! `run.own`: when a closing run releases its owner.

use contextful_core::run::own::releases;
use contextful_core::run::record::RunStatus;

#[test]
fn execution_admission_orders_leases_and_refuses_conflicting_fences() {
    use contextful_core::coordinate::{ExecutionAdmission, Lease};
    use contextful_core::time::Instant;
    let lease = |key: &str, fence| Lease { key: key.into(), holder: "holder".into(), fence, expires_at: Instant::from_unix_secs(60).unwrap() };
    let admission = ExecutionAdmission::new(Vec::new(), vec![lease("z", 1), lease("a", 2), lease("a", 2)], false).unwrap();
    assert_eq!(admission.leases().iter().map(|l| l.key.as_str()).collect::<Vec<_>>(), vec!["a", "z"]);
    assert!(ExecutionAdmission::new(Vec::new(), vec![lease("a", 1), lease("a", 2)], false).is_err());
}

/// `success`, and a failure that wrote no batch, release the owner; every other status holds it.
// spec: run.own.pin-release@dbab6ab5
#[test]
fn success_and_an_empty_failure_release_every_other_status_holds() {
    assert!(releases(RunStatus::Success, 0));
    assert!(releases(RunStatus::Success, 3));
    assert!(releases(RunStatus::Failed, 0));
    assert!(!releases(RunStatus::Failed, 1));
    for s in [RunStatus::PartialFailure, RunStatus::Pending, RunStatus::Running, RunStatus::Waiting] {
        assert!(!releases(s, 0), "{s}");
        assert!(!releases(s, 2), "{s}");
    }
}

/// A seeding scope is a table or chunk scope whose table segment carries `#seed`; the live table's scopes and a host
/// scope are not.
#[test]
fn a_seeding_scope_is_named_by_its_seed_table_segment() {
    use contextful_core::run::own::OwnerScope;
    assert!(OwnerScope::seed("feed", "filings").seeding());
    assert!(OwnerScope::chunk("feed", "filings#seed", "c0").seeding());
    assert!(!OwnerScope::table("feed", "filings").seeding());
    assert!(!OwnerScope::chunk("feed", "filings", "c0").seeding());
    assert!(!OwnerScope::host("job:seed#seed").seeding());
}

/// A delivered batch runs under an owner scope its batch key fixes: one key keeps one scope
/// across resends, and two keys or the live table never share an owner.
#[test]
fn a_receive_scope_is_fixed_by_its_batch_key_and_apart_from_the_table() {
    use contextful_core::run::own::OwnerScope;
    let first = OwnerScope::receive("otel", "otel_spans", "flush-1");
    assert_eq!(first, OwnerScope::receive("otel", "otel_spans", "flush-1"));
    assert_ne!(first, OwnerScope::receive("otel", "otel_spans", "flush-2"));
    assert_ne!(first, OwnerScope::table("otel", "otel_spans"));
    assert_ne!(first, OwnerScope::receive("otel", "otel_events", "flush-1"));
    assert_eq!(first.pipeline_table().map(|(p, _)| p), Some("otel"));
    // The scope's table segment carries no byte of the sender's key.
    let (_, table) = OwnerScope::receive("otel", "otel_spans", "../k/../y").pipeline_table().map(|(p, t)| (p.to_string(), t.to_string())).unwrap();
    assert!(table.starts_with("otel_spans#receive-") && !table.contains('/') && !table.contains(".."), "{table}");
}

/// Each receive refusal's display opens with its identifier.
#[test]
fn receive_refusals_name_their_identifiers() {
    use contextful_core::run::RunError;
    assert!(RunError::ReceiveBatchTooLarge("x".into()).to_string().starts_with("ReceiveBatchTooLarge: "));
    assert!(RunError::ReceiveBackpressure("x".into()).to_string().starts_with("ReceiveBackpressure: "));
    assert!(RunError::ReceiveWriteNotGranted("x".into()).to_string().starts_with("ReceiveWriteNotGranted: "));
    assert!(RunError::ReceiveBatchKeyExpired("x".into()).to_string().starts_with("ReceiveBatchKeyExpired: "));
    assert!(RunError::ReceiveBatchKeyMissing("x".into()).to_string().starts_with("ReceiveBatchKeyMissing: "));
    assert!(RunError::ReceivePipelineUnknown("x".into()).to_string().starts_with("ReceivePipelineUnknown: "));
}

/// The {{run.own.pinned-plan-changed}} refusal names restoring the recorded build or plan and, under a host or table scope,
/// {{run.own.rewind}} with the pending execution id; it names no recovery the command line lacks.
// spec: run.own.pin-mismatch-recovery@fabd7b09
#[test]
fn a_pin_mismatch_names_only_recoveries_the_command_line_provides() {
    use contextful_core::run::own::{ConnectorPin, ExecutionOwner, OwnerPins, OwnerScope, Pins, PlanPins};
    use contextful_core::run::RunError;
    use contextful_core::time::Instant;
    let plan = |r: &str| -> OwnerPins { PlanPins { plan_ref: r.into(), identities: Default::default() }.into() };
    let build = |hash: &str| -> OwnerPins {
        Pins { connector: ConnectorPin { id: "vendor".into(), version: "1".into(), world: "native".into(), hash: "artifact".into() }, content_hash: hash.into(), input_hash: String::new() }.into()
    };
    let owner = |scope: OwnerScope, pins: OwnerPins| ExecutionOwner { execution_id: "x-1".into(), scope, pins, attempts: Vec::new(), opened_at: Instant::from_unix_secs(0).unwrap(), next_attempt_at: None };
    let message = |owner: ExecutionOwner, asked: OwnerPins| match owner.check_pins(&asked) {
        Err(RunError::ExecutionPinMismatch(m)) => m,
        other => panic!("expected ExecutionPinMismatch, got {other:?}"),
    };
    let rewind = "or retire it with `contextful run rewind x-1 --reason <text>`";
    let host = message(owner(OwnerScope::host("job:score"), plan("plan-a")), plan("plan-b"));
    assert!(host.ends_with(&format!("Restore the recorded plan and resume, {rewind}")), "{host}");
    let table = message(owner(OwnerScope::table("feed", "filings"), build("hash-a")), build("hash-b"));
    assert!(table.ends_with(&format!("Restore the recorded build and resume, {rewind}")), "{table}");
    let chunk = message(owner(OwnerScope::chunk("feed", "filings", "c0"), build("hash-a")), build("hash-b"));
    assert!(chunk.ends_with("Restore the recorded build and resume"), "{chunk}");
}

/// The reference a declaration alone determines: a verified body owner's `input` identity, else its plan reference.
#[test]
fn the_declared_reference_is_the_input_identity_else_the_plan_reference() {
    use contextful_core::run::own::{OwnerPins, PlanPins};
    let bare: OwnerPins = PlanPins { plan_ref: "plan-a".into(), identities: Default::default() }.into();
    assert_eq!(bare.declared_ref(), "plan-a");
    let verified: OwnerPins = PlanPins { plan_ref: "owner-a".into(), identities: [("input".to_string(), "input-a".to_string())].into() }.into();
    assert_eq!(verified.declared_ref(), "input-a");
}

/// A run's first commit lands under its run id and each later one under `<run id>.<n>`; a run id whose later
/// commits would pass the store's 128-char bound raises `RunCheckpointIdTooLong`.
#[test]
fn a_checkpoint_run_id_past_the_store_bound_is_refused() {
    use contextful_core::run::own::{segment_run_id, RUN_ID_MAX_CHARS};
    use contextful_core::run::RunError;
    assert_eq!(RUN_ID_MAX_CHARS, 128);
    assert_eq!(segment_run_id("run-1", 0).unwrap(), "run-1");
    assert_eq!(segment_run_id("run-1", 3).unwrap(), "run-1.000003");
    let fits = "r".repeat(121);
    assert_eq!(segment_run_id(&fits, 1).unwrap().len(), 128);
    let long = "r".repeat(122);
    assert_eq!(segment_run_id(&long, 0).unwrap(), long, "the first commit lands under the run id itself");
    let refused = segment_run_id(&long, 1).unwrap_err();
    assert!(matches!(refused, RunError::RunCheckpointIdTooLong(_)), "{refused}");
    assert!(refused.to_string().contains("129-char"), "{refused}");
}
