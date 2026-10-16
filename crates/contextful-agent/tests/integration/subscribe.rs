//! `context.subscribe` on the tool protocol: a paged poll over the committed runs of the
//! tables the credential reads, resumed by the position each page returns.

use crate::mcp::{available_under, call, current, fixture, put};
use contextful_agent::mcp::Server;
use contextful_core::ports::FixedClock;
use contextful_core::store::declare::TableDecl;
use contextful_core::time::Instant;
use serde_json::{json, Value};

fn at(s: &str) -> Instant {
    Instant::parse(s).unwrap()
}

fn runs(page: &Value) -> Vec<&str> {
    page["result"]["structuredContent"]["messages"]
        .as_array()
        .unwrap_or_else(|| panic!("a page lists messages: {page}"))
        .iter()
        .map(|m| m["run_id"].as_str().unwrap())
        .collect()
}

/// `context.subscribe` answers one page: the notifications the replay from `after` sends,
/// and `after`, each table's last delivered position, which the next call passes back.
// spec: read.subscribe.tool-poll@a3ad75d5
#[test]
fn a_subscription_pages_committed_runs_from_the_returned_position() {
    let f = fixture();
    let notes = TableDecl::named("research/notes");
    put(f.face.store(), &notes, "run-0002", "2030-01-01T00:01:00Z", json!([{ "note_id": "n3", "title": "Grid tariffs" }]), &[]);
    let clock = FixedClock(at("2030-01-01T00:06:00Z"));
    let server = Server::new(&f.face, f.authority.clone(), &current, &clock, &f.audit).unwrap();

    let first = call(&server, "context.subscribe", json!({ "tables": ["research/notes"] }));
    assert_eq!(first["result"].get("isError"), None, "{first}");
    assert_eq!(runs(&first), ["run-0001", "run-0002"]);
    let message = &first["result"]["structuredContent"]["messages"][0];
    assert_eq!(message["type"], json!("notification"));
    assert_eq!(message["table"], json!("research/notes"));
    assert!(message.get("rows").is_none() && message.get("row_count").is_none(), "{message}");
    let after = first["result"]["structuredContent"]["after"].clone();
    assert_eq!(after, json!({ "research/notes": "run-0002/ingest-a" }));

    let quiet = call(&server, "context.subscribe", json!({ "tables": ["research/notes"], "after": after }));
    assert!(runs(&quiet).is_empty(), "{quiet}");
    assert_eq!(quiet["result"]["structuredContent"]["after"], after);

    put(f.face.store(), &notes, "run-0003", "2030-01-01T00:02:00Z", json!([{ "note_id": "n4", "title": "Heat pumps" }]), &[]);
    let next = call(&server, "context.subscribe", json!({ "tables": ["research/notes"], "after": after }));
    assert_eq!(runs(&next), ["run-0003"]);
    assert_eq!(next["result"]["structuredContent"]["after"], json!({ "research/notes": "run-0003/ingest-a" }));

    let gone = call(&server, "context.subscribe", json!({ "tables": ["research/notes"], "after": { "research/notes": "run-9999/ingest-a" } }));
    assert_eq!(gone["result"]["structuredContent"]["error"]["identifier"], json!("SubscriptionPositionUnavailable"), "{gone}");

    // A table listed twice, or an `after` key naming a table the call does not list, is a
    // malformed argument.
    let twice = call(&server, "context.subscribe", json!({ "tables": ["research/notes", "research/notes"] }));
    assert_eq!(twice["error"]["code"], json!(-32602), "{twice}");
    let stray = call(&server, "context.subscribe", json!({ "tables": ["research/notes"], "after": { "research/other": "run-0001/ingest-a" } }));
    assert_eq!(stray["error"]["code"], json!(-32602), "{stray}");
}

/// Under a credential carrying a horizon, `context.subscribe` refuses before any table or
/// position resolves, so no run committed after the horizon reaches the caller.
// spec: read.subscribe.horizon-refusal@a5f94f8e
#[test]
fn a_subscription_under_a_horizon_is_refused() {
    let f = available_under(Some("2030-06-01T00:00:00Z"));
    let clock = FixedClock(at("2030-09-02T00:00:00Z"));
    let server = Server::new(&f.face, f.authority.clone(), &current, &clock, &f.audit).unwrap();
    for tables in [json!(["research/earnings"]), json!(["hr/salaries"])] {
        let refused = call(&server, "context.subscribe", json!({ "tables": tables }));
        assert_eq!(refused["result"]["isError"], json!(true), "{refused}");
        assert_eq!(refused["result"]["structuredContent"]["error"]["identifier"], json!("SubscriptionUnderHorizon"), "{refused}");
        assert!(refused["result"]["structuredContent"].get("messages").is_none(), "{refused}");
    }
    let entries = contextful_policy::audit::entries(&f.dir.path().join("audit")).unwrap();
    assert_eq!(entries.last().unwrap().attributes["contextful.tool"], json!("context.subscribe"));
}

/// A subscription naming a table outside the credential's grant refuses as an unknown
/// relation, whether or not the table exists, and sends no notification of a granted one.
// spec: read.subscribe.ungranted-table@406cac71
#[test]
fn a_subscription_to_an_ungranted_table_is_refused() {
    let f = fixture();
    let clock = FixedClock(at("2030-01-01T00:06:00Z"));
    let server = Server::new(&f.face, f.authority.clone(), &current, &clock, &f.audit).unwrap();
    for tables in [json!(["hr/salaries"]), json!(["research/notes", "hr/salaries"]), json!(["research/absent"])] {
        let refused = call(&server, "context.subscribe", json!({ "tables": tables }));
        assert_eq!(refused["result"]["isError"], json!(true), "{refused}");
        assert_eq!(refused["result"]["structuredContent"]["error"]["identifier"], json!("EnforceUnknownRelation"), "{refused}");
        assert!(refused["result"]["structuredContent"].get("messages").is_none(), "{refused}");
    }
    let positioned = call(&server, "context.subscribe", json!({ "tables": ["hr/salaries"], "after": { "hr/salaries": "run-9999/ingest-a" } }));
    assert_eq!(positioned["result"]["structuredContent"]["error"]["identifier"], json!("EnforceUnknownRelation"), "{positioned}");
    let entries = contextful_policy::audit::entries(&f.dir.path().join("audit")).unwrap();
    assert_eq!(entries.last().unwrap().attributes["contextful.tool"], json!("context.subscribe"));
}
