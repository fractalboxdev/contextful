//! `read.subscribe` over a store root: readable runs only, the frontier, resumption across a
//! fold, and a commit another store handle lands.

use crate::support::{at, decl, Fixture};
use contextful_context::land::{land_batches, Batch, Position, RunContext};
use contextful_context::subscribe::Subscription;
use contextful_context::Store;
use contextful_core::read::subscribe::Message;
use contextful_core::store::bound_time::{Bound, Bounds};
use contextful_core::store::lay_out::NodeId;
use contextful_core::store::reserve::Injection;
use serde_json::json;
use std::collections::BTreeMap;

fn tables(names: &[&str]) -> Vec<String> {
    names.iter().map(|n| n.to_string()).collect()
}

fn after(table: &str, position: &str) -> BTreeMap<String, String> {
    BTreeMap::from([(table.to_string(), position.to_string())])
}

fn notified(messages: &[Message]) -> Vec<String> {
    messages
        .iter()
        .map(|m| match m {
            Message::Notification(n) => format!("{}:{}", n.table, n.position()),
            Message::Overrun { .. } => "overrun".into(),
        })
        .collect()
}

fn folded(f: &Fixture, d: &contextful_core::store::declare::TableDecl, now: &str) {
    let outcome = contextful_context::fold::fold(&f.store, d, at(now)).unwrap();
    assert!(matches!(outcome, contextful_core::store::fold::FoldOutcome::Folded { .. }), "{outcome}");
}

fn land(f: &Fixture, run: &str, minute: u32) {
    f.land(&decl("name = \"events\""), run, json!([{ "id": run }]), &format!("2030-01-01T00:{minute:02}:00Z")).unwrap();
}

/// A run notifies only once {{store.lay-out.uncommitted-run}} no longer holds it: a run with
/// no manifest, or a logged run absent from its node's commit log, never notifies.
// spec: read.subscribe.after-commit@2f9d9cce
#[test]
fn a_logged_run_absent_from_its_commit_log_never_notifies() {
    let f = Fixture::new();
    land(&f, "plain", 1);
    contextful_context::commit_log::open_fence(&f.store, "feed", "ingest-a", "events", 1).unwrap();
    let ctx = RunContext {
        node: NodeId::parse("ingest-a").unwrap(),
        injection: Injection { run_id: "leased".into(), site_id: "site-a".into(), batch_seq: None, authored_by: None, authored_agent: None, taint: None },
        committed_at: at("2030-01-01T00:02:00Z"),
    };
    let position = Position { pipeline_id: Some("feed".into()), cursor: Some(json!("c1")), fence: Some(1), logged: true, replace_frontier: false };
    let rows = vec![json!({ "id": "leased" }).as_object().unwrap().clone()];
    land_batches(&f.store, &decl("name = \"events\""), &[Batch { rows, types: Default::default() }], &ctx, &position, &|| Ok(())).unwrap();
    assert!(f.table_dir("events").join("data/runs/leased/ingest-a/_manifest.json").is_file());
    // A run directory holding parts and no manifest is in flight.
    std::fs::create_dir_all(f.table_dir("events").join("data/runs/inflight/ingest-a")).unwrap();
    let mut sub = Subscription::open(&f.store, &tables(&["events"]), &BTreeMap::new()).unwrap();
    assert_eq!(notified(&sub.poll().unwrap()), ["events:plain/ingest-a"]);
    land(&f, "later", 3);
    assert_eq!(notified(&sub.poll().unwrap()), ["events:later/ingest-a"], "the logged run stays silent while its log entry is absent");
}

/// A read of the table bounded `as_of` a notification's frontier covers the notified run's files.
// spec: read.subscribe.frontier-covers-run@4eb34a80
#[test]
fn a_read_at_the_notified_frontier_covers_the_run() {
    let f = Fixture::new();
    let d = decl("name = \"events\"");
    land(&f, "r1", 1);
    land(&f, "r2", 2);
    let mut sub = Subscription::open(&f.store, &tables(&["events"]), &BTreeMap::new()).unwrap();
    for message in sub.poll().unwrap() {
        let Message::Notification(n) = message else { panic!("no overrun under the bound") };
        let scan = f.scan(&d, Bounds { as_of: Some(Bound { at: n.frontier.as_of, inclusive: true }), ..Bounds::default() }).unwrap();
        let dir = format!("runs/{}/{}/", n.run_id, n.node_id);
        assert!(scan.files.iter().any(|file| file.contains(&dir)), "{dir} absent from {:?}", scan.files);
    }
}

/// `after` maps a table to a `<run-id>/<node-id>` position; the subscription replays every
/// later committed run of that table once, then continues live, a fold between leaving the
/// replay unchanged.
// spec: read.subscribe.resume@1360c755
#[test]
fn resuming_replays_later_runs_once_across_a_fold_then_continues_live() {
    let f = Fixture::new();
    let d = decl("name = \"events\"");
    land(&f, "r1", 1);
    land(&f, "r2", 2);
    land(&f, "r3", 3);
    let unfolded = notified(&Subscription::open(&f.store, &tables(&["events"]), &after("events", "r1/ingest-a")).unwrap().poll().unwrap());
    assert_eq!(unfolded, ["events:r2/ingest-a", "events:r3/ingest-a"]);
    folded(&f, &d, "2030-01-01T01:00:00Z");
    let mut sub = Subscription::open(&f.store, &tables(&["events"]), &after("events", "r1/ingest-a")).unwrap();
    assert_eq!(notified(&sub.poll().unwrap()), unfolded, "a fold leaves the replay unchanged");
    land(&f, "r4", 4);
    folded(&f, &d, "2030-01-01T02:00:00Z");
    land(&f, "r5", 5);
    assert_eq!(notified(&sub.poll().unwrap()), ["events:r4/ingest-a", "events:r5/ingest-a"]);
    assert!(sub.poll().unwrap().is_empty());
    assert_eq!(sub.positions()["events"].as_deref(), Some("r5/ingest-a"));
}

/// A subscription reads run manifests and commit logs on disk, never the
/// {{run.project.emission-never-blocks}} channel, so a run one process commits reaches a
/// subscriber in another.
#[test]
fn a_run_another_store_handle_commits_reaches_the_subscriber() {
    let f = Fixture::new();
    land(&f, "r1", 1);
    let reader = Store::open(f.store.root().parent().unwrap().parent().unwrap().parent().unwrap(), "research").unwrap();
    assert_eq!(reader.root(), f.store.root());
    let mut sub = Subscription::open(&reader, &tables(&["events"]), &after("events", "r1/ingest-a")).unwrap();
    assert!(sub.poll().unwrap().is_empty());
    land(&f, "r2", 2);
    assert_eq!(notified(&sub.poll().unwrap()), ["events:r2/ingest-a"]);
    let unknown = Subscription::open(&reader, &tables(&["events"]), &after("other", "r1/ingest-a"));
    assert!(unknown.is_err(), "an `after` table the subscription does not name refuses");
}

/// Notifications of one table arrive in ascending `commit_seq`, the order
/// {{store.reserve.commit-order}} makes runs readable, and each committed run notifies exactly
/// once per subscription.
#[test]
fn a_run_committed_after_an_unpublished_group_waits_for_the_group() {
    use contextful_context::land::{commit_parts_group, publish_group};
    let f = Fixture::new();
    let d = decl("name = \"events\"");
    land(&f, "r1", 1);
    let ctx = RunContext {
        node: NodeId::parse("ingest-a").unwrap(),
        injection: Injection { run_id: "grouped".into(), site_id: "site-a".into(), batch_seq: None, authored_by: None, authored_agent: None, taint: None },
        committed_at: at("2030-01-01T00:02:00Z"),
    };
    let grouped = commit_parts_group(&f.store, &d, &[], &ctx, &Position::default(), "events", &[], &|| Ok(()), &|_| Ok(())).unwrap();
    land(&f, "plain", 3);
    let mut sub = Subscription::open(&f.store, &tables(&["events"]), &after("events", "r1/ingest-a")).unwrap();
    assert!(sub.poll().unwrap().is_empty(), "a run above an unpublished group's commit_seq waits");
    publish_group(&f.store, "events", &tables(&["events"]), &ctx).unwrap();
    assert_eq!(notified(&sub.poll().unwrap()), ["events:grouped/ingest-a", "events:plain/ingest-a"]);
    assert!(sub.poll().unwrap().is_empty());
    let resumed = Subscription::open(&f.store, &tables(&["events"]), &after("events", "r1/ingest-a")).unwrap().poll().unwrap();
    assert_eq!(notified(&resumed), ["events:grouped/ingest-a", "events:plain/ingest-a"]);
    assert!(grouped.commit_seq.is_some());
}

/// Each committed run notifies exactly once per subscription: a table named twice refuses.
#[test]
fn a_table_named_twice_refuses() {
    let f = Fixture::new();
    land(&f, "r1", 1);
    let twice = Subscription::open(&f.store, &tables(&["events", "events"]), &BTreeMap::new());
    assert!(matches!(twice, Err(contextful_context::ContextError::Invalid(_))), "a table named twice refuses");
}
