//! `read.subscribe`: the notification, the resumable per-table position, commit order and
//! the undelivered-notification bound.

use super::at;
use contextful_core::read::subscribe::{deliver, Cursor, Message, Notification, QUEUE_BOUND};
use contextful_core::read::ReadError;
use contextful_core::store::lay_out::RunManifest;
use serde_json::json;

fn run(table: &str, id: &str, seq: i64) -> RunManifest {
    serde_json::from_value(json!({
        "run_id": id, "table": table, "node_id": "ingest-a",
        "committed_at": format!("2030-01-01T00:00:{:02}Z", seq % 60), "commit_seq": seq,
    }))
    .unwrap()
}

fn notified(messages: &[Message]) -> Vec<String> {
    messages
        .iter()
        .filter_map(|m| match m {
            Message::Notification(n) => Some(n.position()),
            Message::Overrun { .. } => None,
        })
        .collect()
}

/// A notification is {{type:contextful_core::read::subscribe::Notification}}; its `frontier` is the
/// pair `{as_of, commit_seq}`: the run's commit instant and the `_commit_seq` value its rows carry.
// spec: read.subscribe.notification@ed567af2
#[test]
fn a_notification_names_its_run_and_the_frontier_including_it() {
    let n = Notification::of(&run("events", "r7", 7));
    assert_eq!((n.table.as_str(), n.run_id.as_str(), n.node_id.as_str()), ("events", "r7", "ingest-a"));
    assert_eq!(n.committed_at, at("2030-01-01T00:00:07Z"));
    assert_eq!((n.frontier.as_of, n.frontier.commit_seq), (at("2030-01-01T00:00:07Z"), 7));
    assert_eq!(n.position(), "r7/ingest-a");
}

/// A notification carries no row content and no row count.
// spec: read.subscribe.no-content@86a78507
#[test]
fn a_notification_carries_no_rows_and_no_count() {
    let wire = serde_json::to_value(Message::Notification(Notification::of(&run("events", "r1", 1)))).unwrap();
    let mut keys: Vec<&str> = wire.as_object().unwrap().keys().map(String::as_str).collect();
    keys.sort();
    assert_eq!(keys, ["committed_at", "frontier", "node_id", "run_id", "table", "type"], "{wire}");
    let mut frontier: Vec<&str> = wire["frontier"].as_object().unwrap().keys().map(String::as_str).collect();
    frontier.sort();
    assert_eq!(frontier, ["as_of", "commit_seq"], "{wire}");
}

/// Notifications of one table arrive in ascending `commit_seq`, and each committed run
/// notifies exactly once per subscription.
// spec: read.subscribe.commit-order@a9ab8822
#[test]
fn notifications_arrive_in_commit_order_once_each() {
    let runs = vec![run("events", "r3", 3), run("events", "r1", 1), run("events", "r2", 2)];
    let mut cursors = vec![Cursor::start("events", &runs, None).unwrap()];
    let first = deliver(&mut cursors, std::slice::from_ref(&runs), QUEUE_BOUND);
    assert_eq!(notified(&first), ["r1/ingest-a", "r2/ingest-a", "r3/ingest-a"]);
    assert!(deliver(&mut cursors, std::slice::from_ref(&runs), QUEUE_BOUND).is_empty(), "a delivered run never notifies again");
    let mut grown = runs.clone();
    grown.push(run("events", "r4", 4));
    assert_eq!(notified(&deliver(&mut cursors, &[grown], QUEUE_BOUND)), ["r4/ingest-a"]);
}

/// An `after` position naming no committed run of its table raises
/// `SubscriptionPositionUnavailable`, naming the table's oldest resolvable position.
// spec: read.subscribe.position-unavailable@fac51a31
#[test]
fn an_unresolvable_position_names_the_oldest_resolvable_one() {
    let runs = vec![run("events", "r5", 5), run("events", "r4", 4)];
    match Cursor::start("events", &runs, Some("r1/ingest-a")) {
        Err(e @ ReadError::SubscriptionPositionUnavailable(_)) => {
            assert_eq!(e.identifier(), "SubscriptionPositionUnavailable");
            assert!(e.to_string().contains("r4/ingest-a"), "{e}");
        }
        other => panic!("{other:?}"),
    }
    let empty = Cursor::start("events", &[], Some("r1/ingest-a")).unwrap_err();
    assert!(empty.to_string().contains("none"), "{empty}");
}

/// A subscription holding more than 1000 notifications undelivered sends 1000, then a
/// terminal `overrun` message naming each table's last delivered position, and ends.
// spec: read.subscribe.overrun@905b2d6c
#[test]
fn past_the_bound_a_terminal_overrun_names_the_last_delivered_position() {
    assert_eq!(QUEUE_BOUND, 1000);
    let runs: Vec<RunManifest> = (1..=1001).map(|i| run("events", &format!("r{i:04}"), i)).collect();
    let mut cursors = vec![Cursor::start("events", &runs, None).unwrap(), Cursor::start("quiet", &[], None).unwrap()];
    let messages = deliver(&mut cursors, &[runs.clone(), Vec::new()], QUEUE_BOUND);
    assert_eq!(notified(&messages).len(), 1000);
    match messages.last() {
        Some(Message::Overrun { positions }) => {
            assert_eq!(positions["events"].as_deref(), Some("r1000/ingest-a"));
            assert_eq!(positions["quiet"], None);
        }
        other => panic!("{other:?}"),
    }
    // Resuming from the terminal position loses nothing.
    let mut resumed = vec![Cursor::start("events", &runs, Some("r1000/ingest-a")).unwrap()];
    let rest = deliver(&mut resumed, &[runs], QUEUE_BOUND);
    assert!(!rest.is_empty());
    assert_eq!(notified(&rest), ["r1001/ingest-a"]);
    assert!(!rest.iter().any(|m| matches!(m, Message::Overrun { .. })));
}
