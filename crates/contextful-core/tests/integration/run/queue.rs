//! The `Queue` port's messages: their wire form and the identity a resend shares.

use contextful_core::run::queue::{BodyRef, QueueMessage};
use serde_json::json;

#[test]
fn each_message_kind_round_trips_through_its_tagged_wire_form() {
    let messages = [
        (
            QueueMessage::Receive { pipeline: "events".into(), batch_key: "b-1".into(), body: BodyRef::Inline { body: b"{\"id\":1}\n".to_vec() } },
            json!({ "kind": "receive", "pipeline": "events", "batch_key": "b-1", "body": { "at": "inline", "body": "eyJpZCI6MX0K" } }),
        ),
        (
            QueueMessage::Receive { pipeline: "events".into(), batch_key: "b-2".into(), body: BodyRef::Spilled { key: "receive/events/b-2".into() } },
            json!({ "kind": "receive", "pipeline": "events", "batch_key": "b-2", "body": { "at": "spilled", "key": "receive/events/b-2" } }),
        ),
        (QueueMessage::ObjectArrival { bucket: "lake".into(), key: "feed/01.jsonl".into() }, json!({ "kind": "object_arrival", "bucket": "lake", "key": "feed/01.jsonl" })),
        (
            QueueMessage::Committed { table: "spans".into(), commit_seq: Some(7), run: Some("load-1".into()) },
            json!({ "kind": "committed", "table": "spans", "commit_seq": 7, "run": "load-1" }),
        ),
    ];
    for (message, wire) in messages {
        assert_eq!(serde_json::to_value(&message).unwrap(), wire);
        assert_eq!(serde_json::from_value::<QueueMessage>(wire).unwrap(), message);
    }
}

#[test]
fn a_resend_shares_its_originals_identity_and_distinct_subjects_do_not() {
    let wake = |table: &str, seq: i64, run: &str| QueueMessage::Committed { table: table.into(), commit_seq: Some(seq), run: Some(run.into()) };
    assert_eq!(wake("spans", 1, "load-1").identity(), wake("spans", 9, "load-9").identity(), "a wake-up names its table alone");
    assert_ne!(wake("spans", 1, "load-1").identity(), wake("documents", 1, "load-1").identity());
    let arrival = |bucket: &str, key: &str| QueueMessage::ObjectArrival { bucket: bucket.into(), key: key.into() };
    assert_eq!(arrival("lake", "feed/01.jsonl").identity(), arrival("lake", "feed/01.jsonl").identity());
    assert_ne!(arrival("lake", "feed/01.jsonl").identity(), arrival("lake", "feed/02.jsonl").identity());
    assert_ne!(arrival("lake", "a/b").identity(), arrival("lake/a", "b").identity(), "the separator keeps bucket and key apart");
    let batch = |key: &str, body: BodyRef| QueueMessage::Receive { pipeline: "events".into(), batch_key: key.into(), body };
    assert_eq!(batch("b-1", BodyRef::Inline { body: vec![1] }).identity(), batch("b-1", BodyRef::Spilled { key: "k".into() }).identity());
    assert_ne!(batch("b-1", BodyRef::Inline { body: vec![1] }).identity(), batch("b-2", BodyRef::Inline { body: vec![1] }).identity());
}
