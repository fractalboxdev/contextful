use super::Fixture;
use contextful_core::export::publication::{
    GroupState, PublicationPlan, ReceiverState, SourcePublication,
};
use contextful_core::run::ports::BlobStore;
use contextful_d1::{D1BlobStore, D1PublicationLedger, D1};
use serde_json::json;
use std::collections::BTreeMap;

fn start() -> Fixture {
    let fixture = Fixture::start();
    D1::connect(&fixture.endpoint)
        .unwrap()
        .initialize_recovery("sample", "generation")
        .unwrap();
    fixture
}

fn ledger(fixture: &Fixture, holder: &str) -> D1PublicationLedger {
    let mut ledger = D1PublicationLedger::connect(&fixture.endpoint).unwrap();
    ledger.claim("projection", holder, 30).unwrap();
    ledger
}

fn plan(namespace: &str, base: Option<String>, order: u64) -> PublicationPlan {
    PublicationPlan {
        name: "projection".into(),
        incarnation: "store-1".into(),
        namespace: namespace.into(),
        destination: "receiver".into(),
        read_identity: "reader".into(),
        base,
        order,
        input: json!({"snapshot": order}),
        members: [("a".into(), "items".into()), ("b".into(), "notes".into())].into(),
    }
}

fn bulk(rows: usize, width: usize) -> GroupState {
    let member = |prefix: &str| -> contextful_core::export::ChangeState {
        (0..rows)
            .map(|i| (format!("{{\"id\":\"{prefix}{i}\"}}"), json!({"id": format!("{prefix}{i}"), "body": "x".repeat(width)})))
            .collect()
    };
    [("a".into(), member("a")), ("b".into(), member("b"))].into()
}

fn states(body: &str) -> GroupState {
    [
        ("a".into(), [("{\"id\":1}".into(), json!({"id": 1, "body": body}))].into()),
        ("b".into(), [("{\"id\":2}".into(), json!({"id": 2, "body": body}))].into()),
    ]
    .into()
}

/// Deliver every pending batch; `lose_last` drops the final acknowledgement after the receiver commits.
fn deliver(ledger: &mut D1PublicationLedger, receiver: &mut ReceiverState, lose_last: bool) {
    while let Some(batch) = ledger.batch("projection", "initial").unwrap() {
        receiver.receive(&batch).unwrap();
        let through = batch.events.last().unwrap().sequence;
        let complete = receiver.pending.is_none();
        if lose_last && complete {
            return;
        }
        ledger
            .acknowledge("projection", "initial", &batch.group.id, through)
            .unwrap();
    }
}

#[test]
fn a_group_staged_before_a_crash_redelivers_identical_bytes_after_replacement() {
    let fixture = start();
    let mut first = ledger(&fixture, "container-1");
    first.stage(&plan("initial", None, 1), &states("one"), "reader").unwrap();
    let staged = first.batch("projection", "initial").unwrap().unwrap();
    first.release().unwrap();
    drop(first);

    let mut replacement = ledger(&fixture, "container-2");
    assert_eq!(
        replacement.pending_identity("projection", "initial").unwrap().as_deref(),
        Some("reader")
    );
    let retried = replacement.batch("projection", "initial").unwrap().unwrap();
    assert_eq!(serde_json::to_vec(&retried).unwrap(), serde_json::to_vec(&staged).unwrap());
    let mut receiver = ReceiverState::default();
    deliver(&mut replacement, &mut receiver, false);
    assert_eq!(receiver.state, states("one"));
    assert_eq!(receiver.active.as_ref().map(|g| &g.id), Some(&staged.group.id));
    assert_eq!(
        replacement.active("projection", "initial").unwrap().map(|g| g.id),
        Some(staged.group.id)
    );
    assert_eq!(
        replacement.active_namespace("projection").unwrap().as_deref(),
        Some("initial")
    );
}

#[test]
fn a_lost_group_acknowledgement_reoffers_the_committed_batch_after_replacement() {
    let fixture = start();
    let mut first = ledger(&fixture, "container-1");
    first.stage(&plan("initial", None, 1), &states("one"), "reader").unwrap();
    let mut receiver = ReceiverState::default();
    deliver(&mut first, &mut receiver, true);
    let activated = receiver.clone();
    assert_eq!(activated.state, states("one"));
    first.release().unwrap();
    drop(first);

    let mut replacement = ledger(&fixture, "container-2");
    assert!(replacement.active("projection", "initial").unwrap().is_none());
    deliver(&mut replacement, &mut receiver, false);
    assert_eq!(receiver, activated);
    let active = replacement.active("projection", "initial").unwrap().unwrap();
    assert_eq!(Some(&active), receiver.active.as_ref());

    replacement
        .stage(&plan("initial", Some(active.id.clone()), 2), &states("two"), "reader")
        .unwrap();
    deliver(&mut replacement, &mut receiver, false);
    assert_eq!(receiver.state, states("two"));
}

#[test]
fn a_receiver_never_activates_a_partially_delivered_group() {
    let fixture = start();
    let mut sender = ledger(&fixture, "container-1");
    sender.stage(&plan("initial", None, 1), &states("one"), "reader").unwrap();
    let batch = sender.batch("projection", "initial").unwrap().unwrap();
    let mut receiver = ReceiverState::default();
    let mut prefix = batch.clone();
    prefix.events.truncate(2);
    receiver.receive(&prefix).unwrap();
    assert!(receiver.active.is_none());
    assert!(receiver.state.is_empty());
}

#[test]
fn a_transferred_group_lease_fences_the_prior_writer() {
    let fixture = start();
    let mut stale = D1PublicationLedger::connect(&fixture.endpoint).unwrap();
    stale.claim("projection", "container-1", 1).unwrap();
    std::thread::sleep(std::time::Duration::from_millis(1500));
    let mut current = ledger(&fixture, "container-2");
    let refused = stale
        .stage(&plan("initial", None, 1), &states("one"), "reader")
        .unwrap_err();
    assert!(refused.message.contains("ExecutionWriterFenced"), "{}", refused.message);
    assert!(current.pending_identity("projection", "initial").unwrap().is_none());
    current.stage(&plan("initial", None, 1), &states("one"), "reader").unwrap();
}

#[test]
fn a_fenced_writer_cannot_complete_a_group_its_successor_completed() {
    let fixture = start();
    let mut stale = D1PublicationLedger::connect(&fixture.endpoint).unwrap();
    stale.claim("projection", "container-1", 1).unwrap();
    stale.stage(&plan("initial", None, 1), &states("one"), "reader").unwrap();
    let offered = stale.batch("projection", "initial").unwrap().unwrap();
    std::thread::sleep(std::time::Duration::from_millis(1500));
    let mut current = ledger(&fixture, "container-2");
    let mut receiver = ReceiverState::default();
    deliver(&mut current, &mut receiver, false);
    let active = current.active("projection", "initial").unwrap();
    assert!(stale
        .acknowledge(
            "projection",
            "initial",
            &offered.group.id,
            offered.events.last().unwrap().sequence
        )
        .is_err());
    assert_eq!(current.active("projection", "initial").unwrap(), active);
    assert_eq!(current.active_namespace("projection").unwrap().as_deref(), Some("initial"));
    assert!(current.batch("projection", "initial").unwrap().is_none());
}

#[test]
fn a_large_sender_spills_to_r2_and_survives_the_blob_sweep() {
    let fixture = start();
    let mut sender = ledger(&fixture, "container-1");
    let large = bulk(600, 1024);
    sender.stage(&plan("initial", None, 1), &large, "reader").unwrap();
    let db = D1::connect(&fixture.endpoint).unwrap();
    let row = db
        .query(
            "SELECT body, blob FROM contextful_publication WHERE role = 'sender'",
            vec![],
        )
        .unwrap()
        .rows
        .remove(0);
    let digest = row["blob"].as_str().unwrap().to_owned();
    assert!(row["body"].as_str().unwrap().len() < 1024);
    assert!(db.head_blob(&digest).unwrap());
    let blobs = D1BlobStore::connect(&fixture.endpoint).unwrap();
    assert!(blobs.sweep(&[], i64::MAX).unwrap().is_empty());
    db.verify_recovery("sample", "generation").unwrap();
    drop(sender);

    let mut replacement = ledger(&fixture, "container-2");
    let mut receiver = ReceiverState::default();
    deliver(&mut replacement, &mut receiver, false);
    assert_eq!(receiver.state, large);
    replacement
        .stage(
            &plan("initial", receiver.active.as_ref().map(|g| g.id.clone()), 2),
            &states("small"),
            "reader",
        )
        .unwrap();
    deliver(&mut replacement, &mut receiver, false);
    assert!(blobs.sweep(&[], i64::MAX).unwrap().contains(&digest));
    assert!(!db.head_blob(&digest).unwrap());
    let row = db
        .query("SELECT blob FROM contextful_publication WHERE role = 'sender'", vec![])
        .unwrap()
        .rows
        .remove(0);
    assert!(row["blob"].is_null());
}

#[test]
fn a_sender_state_over_the_d1_bound_refuses_before_staging() {
    let fixture = start();
    let mut sender = ledger(&fixture, "container-1");
    let refused = sender
        .stage(&plan("initial", None, 1), &bulk(3000, 1024), "reader")
        .unwrap_err();
    assert!(
        refused.message.contains("PublicationStorageUnsupported"),
        "{}",
        refused.message
    );
    assert!(sender.pending_identity("projection", "initial").unwrap().is_none());
    assert!(sender.batch("projection", "initial").unwrap().is_none());
}

#[test]
fn a_producer_frontier_survives_replacement_and_refuses_rewritten_history() {
    let fixture = start();
    let sender = ledger(&fixture, "container-1");
    let source = SourcePublication {
        run: "run-1".into(),
        input: json!({"snapshot": 1}),
        frontiers: BTreeMap::from([("items".into(), "f1".into()), ("notes".into(), "f2".into())]),
    };
    sender.record_source("projection", &source).unwrap();
    sender.record_source("projection", &source).unwrap();
    let rewritten = SourcePublication {
        frontiers: BTreeMap::from([("items".into(), "other".into())]),
        ..source.clone()
    };
    assert!(sender.record_source("projection", &rewritten).is_err());
    let incarnation = sender.incarnation("store-1").unwrap();
    drop(sender);
    let replacement = ledger(&fixture, "container-2");
    assert_eq!(replacement.source("projection").unwrap(), Some(source));
    assert_eq!(replacement.incarnation("store-2").unwrap(), incarnation);
}

#[test]
fn a_checkpoint_restores_identical_group_bytes_from_d1() {
    let fixture = start();
    let mut sender = ledger(&fixture, "container-1");
    sender.stage(&plan("initial", None, 1), &states("one"), "reader").unwrap();
    let checkpoint = sender.checkpoint("projection", "initial").unwrap();
    let first = sender.batch("projection", "initial").unwrap().unwrap();
    let mut receiver = ReceiverState::default();
    deliver(&mut sender, &mut receiver, false);
    sender
        .restore("projection", "initial", &checkpoint, "store-1", "reader")
        .unwrap();
    assert_eq!(sender.batch("projection", "initial").unwrap().unwrap(), first);
    assert!(sender
        .restore("projection", "initial", &checkpoint, "store-1", "other")
        .is_err());
}

#[test]
fn a_version_two_execution_schema_gains_the_publication_table_on_initialize() {
    let fixture = start();
    let db = D1::connect(&fixture.endpoint).unwrap();
    db.query("DROP TABLE contextful_publication", vec![]).unwrap();
    db.query("UPDATE contextful_execution_schema SET version = 2", vec![]).unwrap();
    assert!(db.verify_recovery("sample", "generation").is_err());
    db.initialize_recovery("sample", "generation").unwrap();
    db.verify_recovery("sample", "generation").unwrap();
    D1PublicationLedger::connect(&fixture.endpoint).unwrap();
}
