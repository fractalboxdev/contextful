//! `read.cache`: the result cache — a repeated statement over an unchanged read frontier
//! executes once, every change to its key misses, and the cache stays under its budget.

use super::*;
use contextful_context::land::{land, Batch, RunContext};
use contextful_context::ledger;
use contextful_core::store::lay_out::NodeId;
use contextful_core::store::reserve::Injection;
use contextful_context::read::ResultCounts;
use contextful_core::store::ledger::RequestRecord;
use contextful_policy::issue::MintClaims;
use contextful_core::identify::Subject;
use serde_json::Map;
use std::collections::BTreeSet;

const CACHED: &str = r#"
[[pipeline.tables]]
name = "dash/events"
result_cache = "10m"

[[pipeline.tables]]
name = "dash/brief"
result_cache = "1s"

[[pipeline.tables]]
name = "dash/secret"
result_cache = "10m"
private = true

[[pipeline.tables]]
name = "dash/plain"
"#;

const EVENTS: &str = r#"SELECT event_id FROM "dash/events" ORDER BY event_id"#;

/// The fixture store with the dashboard tables landed, and a face over it whose result
/// cache holds `budget` bytes.
fn dash(budget: Option<u64>) -> (Reads, Face) {
    let r = Reads::new();
    for (t, run) in [("dash/events", "run-0001"), ("dash/brief", "run-0001"), ("dash/secret", "run-0001"), ("dash/plain", "run-0001")] {
        land_dash(&r.store, t, run, &["e1", "e2"]);
    }
    let face = Face::open(r.store.clone(), CACHED, pepper()).unwrap();
    let face = match budget {
        Some(b) => face.with_result_cache(b),
        None => face,
    };
    (r, face)
}

fn land_dash(store: &Store, table: &str, run: &str, ids: &[&str]) {
    let rows: Vec<Value> = ids.iter().map(|id| json!({ "event_id": id, "kind": "open" })).collect();
    let decl = TableDecl::parse_pipeline(CACHED).unwrap().into_iter().find(|d| d.name == table).unwrap();
    let rows = rows.iter().map(|r| r.as_object().unwrap().clone()).collect();
    let ctx = RunContext {
        node: NodeId::parse("ingest-a").unwrap(),
        injection: Injection { run_id: run.into(), site_id: "site-a".into(), batch_seq: Some(0), authored_by: None, taint: None },
        committed_at: at("2030-01-10T00:00:00Z"),
    };
    land(store, &decl, &Batch { rows, types: HashMap::new() }, &ctx).unwrap();
}

fn reader(r: &Reads) -> AdmittedAuthority {
    r.authority(loop_subject("agent://dashboard"), vec![read(&["dash/*"], None)])
}

/// One request as a transport serves it: a session for the authority, then the statement.
fn ask(face: &Face, authority: &AdmittedAuthority, sql: &str, parameters: &Map<String, Value>, opts: ReadOptions) -> Response {
    let s = face.session(authority, &Request::default(), opts.bounds).unwrap();
    face.query_with(&s, sql, parameters, opts).unwrap()
}

fn plain(face: &Face, authority: &AdmittedAuthority, sql: &str) -> Response {
    ask(face, authority, sql, &Map::new(), ReadOptions::default())
}

fn bytes(r: &Response) -> String {
    serde_json::to_string(&r.to_json()).unwrap()
}

fn counts(face: &Face) -> ResultCounts {
    face.results().expect("the face declares a budget").counts()
}

fn param(name: &str, value: &str) -> Map<String, Value> {
    json!({ name: { "type": "string", "value": value } }).as_object().unwrap().clone()
}

/// One principal under another parameter value, row ceiling, bounds or statement text misses.
#[test]
fn a_change_to_any_statement_part_of_the_key_misses() {
    let (r, face) = dash(Some(1 << 20));
    let authority = reader(&r);
    let by_id = r#"SELECT event_id FROM "dash/events" WHERE event_id = $id"#;
    ask(&face, &authority, by_id, &param("id", "e1"), ReadOptions::default());
    ask(&face, &authority, by_id, &param("id", "e1"), ReadOptions::default());
    assert_eq!((counts(&face).hits, counts(&face).misses), (1, 1));

    // Another parameter value.
    let other = ask(&face, &authority, by_id, &param("id", "e2"), ReadOptions::default());
    assert_eq!(column(&other, "event_id"), [json!("e2")]);
    assert_eq!((counts(&face).hits, counts(&face).misses), (1, 2));

    // Another row ceiling.
    let capped = ask(&face, &authority, EVENTS, &Map::new(), ReadOptions { limit: Some(1), ..ReadOptions::default() });
    assert!(capped.truncated);
    let whole = plain(&face, &authority, EVENTS);
    assert!(!whole.truncated, "a truncated result under one ceiling is not the answer under another");
    assert_eq!(column(&whole, "event_id").len(), 2);
    assert_eq!((counts(&face).hits, counts(&face).misses), (1, 4));

    // Other bounds.
    let bounded = ReadOptions { bounds: Bounds { as_of: Some(contextful_core::store::bound_time::Bound::parse("2031-01-01T00:00:00Z").unwrap()), valid_as_of: None }, ..ReadOptions::default() };
    let bounded_rows = ask(&face, &authority, EVENTS, &Map::new(), bounded);
    assert!(bounded_rows.blocks.contains_key("contextful.bounds"));
    assert_eq!((counts(&face).hits, counts(&face).misses), (1, 5));

    // Another statement text.
    plain(&face, &authority, r#"SELECT event_id FROM "dash/events" ORDER BY event_id DESC"#);
    assert_eq!((counts(&face).hits, counts(&face).misses), (1, 6));

    // Every one of them, repeated, hits.
    plain(&face, &authority, EVENTS);
    ask(&face, &authority, by_id, &param("id", "e2"), ReadOptions::default());
    assert_eq!((counts(&face).hits, counts(&face).misses), (3, 6));
}

/// Credentials sharing one id, each differing from the first in one principal field.
fn principals(r: &Reads) -> Vec<(&'static str, AdmittedAuthority)> {
    const SEED: [u8; 32] = [7; 32];
    let all = || vec![read(&["dash/*"], None)];
    let mint = |subject: Subject, grants: Vec<Grant>, epoch: u64| {
        r.authority_seeded(subject, grants, MintClaims { epoch, ..MintClaims::default() }, SEED)
    };
    let dashboard = || loop_subject("agent://dashboard");
    vec![
        ("the first principal", mint(dashboard(), all(), 0)),
        ("another revocation epoch", mint(dashboard(), all(), 7)),
        ("another grant set over the same relation", mint(dashboard(), vec![read(&["dash/events", "dash/brief"], None)], 0)),
        ("another subject", mint(loop_subject("agent://other"), all(), 0)),
        ("another signed zone", mint(Subject { zone: Some("on-prem:lab".into()), ..dashboard() }, all(), 0)),
        ("an incognito session in the same zone", mint(Subject { incognito: true, ..dashboard() }, all(), 0)),
        ("an incognito session with no signed zone", mint(Subject { zone: None, incognito: true, ..dashboard() }, all(), 0)),
        ("no signed zone, which withholds the table", mint(Subject { zone: None, ..dashboard() }, all(), 0)),
    ]
}

/// A cached result keys on the token's id, revocation epoch, grants and subject, the session zone and incognito state, each touched relation and the files it reads, the bounds, the statement, its parameter values and the applied row ceiling.
// spec: read.cache.result-key@3ed2080d
#[test]
fn one_credential_id_under_another_principal_misses() {
    let (r, face) = dash(Some(1 << 20));
    let principals = principals(&r);
    let ids: BTreeSet<String> =
        principals.iter().map(|(_, a)| face.session(a, &Request::default(), Bounds::default()).unwrap().credential_id().to_string()).collect();
    assert_eq!(ids.len(), 1, "every principal holds one credential id");

    for (n, (what, authority)) in principals.iter().enumerate() {
        plain(&face, authority, EVENTS);
        assert_eq!((counts(&face).hits, counts(&face).misses), (0, n as u64 + 1), "{what} misses");
    }
    for (what, authority) in &principals {
        let before = counts(&face).hits;
        plain(&face, authority, EVENTS);
        assert_eq!(counts(&face).hits, before + 1, "{what}, repeated, hits");
    }
}

/// A statement's result caches only when every table it touches declares `result_cache`, a time to live, and none declares `private = true`; an entry expires at the least time to live among them.
// spec: read.cache.cache-is-opt-in@617f0496
#[test]
fn only_opted_in_tables_that_are_not_private_cache_and_entries_expire() {
    let (r, face) = dash(Some(1 << 20));
    let authority = reader(&r);
    for sql in [
        r#"SELECT event_id FROM "dash/plain" ORDER BY event_id"#,
        r#"SELECT event_id FROM "dash/secret" ORDER BY event_id"#,
        r#"SELECT e.event_id FROM "dash/events" e JOIN "dash/plain" p USING (event_id) ORDER BY 1"#,
        r#"SELECT e.event_id FROM "dash/events" e JOIN "dash/secret" p USING (event_id) ORDER BY 1"#,
    ] {
        let first = plain(&face, &authority, sql);
        let second = plain(&face, &authority, sql);
        assert_eq!(column(&first, "event_id"), column(&second, "event_id"));
    }
    let c = counts(&face);
    assert_eq!((c.hits, c.misses, c.entries), (0, 0, 0), "no statement touching a plain or private table caches");
    assert_eq!(c.bypassed, 8);

    // A face declaring no budget caches nothing.
    let (r, uncached) = dash(None);
    assert!(uncached.results().is_none());
    let authority = reader(&r);
    assert_eq!(bytes(&plain(&uncached, &authority, EVENTS)), bytes(&plain(&uncached, &authority, EVENTS)));

    // A join takes the least time to live among its tables.
    let (r, face) = dash(Some(1 << 20));
    let authority = reader(&r);
    let joined = r#"SELECT e.event_id FROM "dash/events" e JOIN "dash/brief" b USING (event_id) ORDER BY 1"#;
    plain(&face, &authority, joined);
    plain(&face, &authority, EVENTS);
    plain(&face, &authority, joined);
    assert_eq!((counts(&face).hits, counts(&face).misses), (1, 2));
    std::thread::sleep(std::time::Duration::from_millis(1_100));
    plain(&face, &authority, joined);
    plain(&face, &authority, EVENTS);
    assert_eq!((counts(&face).hits, counts(&face).misses), (2, 3), "the one-second entry expired; the ten-minute one holds");
}

/// A commit, fold or request-ledger append on a touched table moves its read frontier, the snapshot and runs its relation reads, so an entry filled before it stops matching. A statement in flight during a fold reads {{store.fold.non-blocking}}.
// spec: read.cache.frontier-invalidates@c049fa30
#[test]
fn a_landed_run_or_ledger_append_misses_and_reads_the_new_state() {
    let (r, face) = dash(Some(1 << 20));
    let authority = reader(&r);
    assert_eq!(column(&plain(&face, &authority, EVENTS), "event_id").len(), 2);

    // A run landed with no fold: the snapshot ids are unchanged, the frontier is not.
    land_dash(&r.store, "dash/events", "run-0002", &["e3"]);
    let after = plain(&face, &authority, EVENTS);
    assert_eq!(column(&after, "event_id"), [json!("e1"), json!("e2"), json!("e3")]);
    assert_eq!((counts(&face).hits, counts(&face).misses), (0, 2));

    // A run on an untouched table leaves the entry live.
    land_dash(&r.store, "dash/plain", "run-0002", &["p9"]);
    assert_eq!(bytes(&plain(&face, &authority, EVENTS)), bytes(&after));
    assert_eq!((counts(&face).hits, counts(&face).misses), (1, 2));

    // A fold publishes a snapshot: the same rows, read from a new frontier.
    contextful_context::fold::fold(&r.store, &TableDecl::parse_pipeline(CACHED).unwrap()[0], at("2030-01-11T00:00:00Z")).unwrap();
    assert_eq!(column(&plain(&face, &authority, EVENTS), "event_id").len(), 3);
    assert_eq!((counts(&face).hits, counts(&face).misses), (1, 3));

    // A request-ledger append on the touched table.
    let requests = r#"SELECT request_id FROM "dash/events__requests" ORDER BY request_id"#;
    let record = |id: &str| RequestRecord {
        request_id: id.into(),
        vendor_request_id: None,
        connector: "http".into(),
        method: "GET".into(),
        url_host: "api.example.org".into(),
        status_code: Some(200),
        started_at: at("2030-01-10T00:00:00Z"),
        duration_ms: 12,
        batch_seq: None,
    };
    ledger::append(&r.store, "dash/events", "run-0002", &NodeId::parse("ingest-a").unwrap(), &[record("req-1")]).unwrap();
    assert_eq!(column(&plain(&face, &authority, requests), "request_id"), [json!("req-1")]);
    ledger::append(&r.store, "dash/events", "run-0002", &NodeId::parse("ingest-a").unwrap(), &[record("req-2")]).unwrap();
    assert_eq!(column(&plain(&face, &authority, requests), "request_id"), [json!("req-1"), json!("req-2")]);
    assert_eq!((counts(&face).hits, counts(&face).misses), (1, 5));
}

/// The result cache holds at most the byte budget its process declares, the least recently used entry evicted first; a face declaring no budget caches nothing.
// spec: read.cache.budget@a9635453
#[test]
fn filling_past_the_budget_evicts_the_least_recently_used() {
    let by_id = r#"SELECT event_id FROM "dash/events" WHERE event_id = $id"#;
    // One entry's size, measured on a face with room to spare.
    let (r, probe) = dash(Some(1 << 20));
    ask(&probe, &reader(&r), by_id, &param("id", "e1"), ReadOptions::default());
    let one = counts(&probe).resident_bytes;
    assert!(one > 0);

    let budget = one * 2 + one / 2;
    let (r, face) = dash(Some(budget));
    let authority = reader(&r);
    let get = |id: &str| ask(&face, &authority, by_id, &param("id", id), ReadOptions::default());
    get("e1");
    get("e2");
    get("e1");
    assert_eq!((counts(&face).hits, counts(&face).misses, counts(&face).entries), (1, 2, 2));
    // A third entry evicts `e2`, the least recently used.
    get("e3");
    assert_eq!(counts(&face).entries, 2);
    assert!(counts(&face).resident_bytes <= budget);
    get("e1");
    assert_eq!((counts(&face).hits, counts(&face).misses), (2, 3), "`e1` stays resident");
    get("e2");
    assert_eq!((counts(&face).hits, counts(&face).misses), (2, 4), "`e2` was evicted");
    assert!(counts(&face).resident_bytes <= budget);

    // An entry larger than the whole budget is never resident.
    let (r, tiny) = dash(Some(one - 1));
    let authority = reader(&r);
    ask(&tiny, &authority, by_id, &param("id", "e1"), ReadOptions::default());
    ask(&tiny, &authority, by_id, &param("id", "e1"), ReadOptions::default());
    assert_eq!((counts(&tiny).hits, counts(&tiny).entries, counts(&tiny).resident_bytes), (0, 0, 0));
}

/// A hit returns the response byte-identical to the miss that filled it, restriction block included; under `internals: true` the internals object carries `cache` as `hit` or `miss`.
// spec: read.cache.hit-identical@c4287616
#[test]
fn a_hit_is_byte_identical_to_the_miss_and_internals_name_it() {
    let (r, face) = dash(Some(1 << 20));
    let authority = reader(&r);
    let first = plain(&face, &authority, EVENTS);
    let second = plain(&face, &authority, EVENTS);
    assert_eq!(bytes(&first), bytes(&second));
    assert_eq!((counts(&face).hits, counts(&face).misses), (1, 1), "two identical calls execute one statement");

    let with = ReadOptions { internals: true, ..ReadOptions::default() };
    let miss = ask(&face, &authority, r#"SELECT count(*) AS n FROM "dash/events""#, &Map::new(), with);
    let hit = ask(&face, &authority, r#"SELECT count(*) AS n FROM "dash/events""#, &Map::new(), with);
    assert_eq!(miss.blocks["contextful.internals"]["cache"], json!("miss"));
    assert_eq!(hit.blocks["contextful.internals"]["cache"], json!("hit"));
    assert_eq!(hit.blocks["contextful.internals"]["row_count"], json!(1));
    assert_eq!((miss.rows.clone(), miss.truncated), (hit.rows.clone(), hit.truncated));
    // The internals opt-in is no part of the key: the bare call hits the same entry.
    let bare = plain(&face, &authority, r#"SELECT count(*) AS n FROM "dash/events""#);
    assert!(!bare.blocks.contains_key("contextful.internals"));
    assert_eq!(bare.rows, hit.rows);
    assert_eq!(counts(&face).hits, 3);

    // A zone-withheld relation's restriction block rides the hit.
    let zoned = r#"
[[pipeline.tables]]
name = "dash/events"
result_cache = "10m"

[pipeline.tables.policy.zone]
allow = ["public-cloud:*"]
"#;
    let face = Face::open(r.store.clone(), zoned, pepper()).unwrap().with_result_cache(1 << 20);
    let first = plain(&face, &authority, EVENTS);
    assert!(first.blocks.contains_key("contextful.restriction"));
    assert_eq!(bytes(&first), bytes(&plain(&face, &authority, EVENTS)));
    assert_eq!((counts(&face).hits, counts(&face).misses), (1, 1));
}

/// A statement calling a function the engine marks other than consistent across queries, such as `now()`, `current_timestamp` or `random()`, or a macro reaching one, executes uncached.
// spec: read.cache.volatile-bypasses@44a762e3
#[test]
fn a_statement_calling_a_volatile_function_executes_each_time() {
    let (r, face) = dash(Some(1 << 20));
    let authority = reader(&r);
    let clock = r#"SELECT CAST(now() AS VARCHAR) AS t, count(*) AS n FROM "dash/events""#;
    let first = plain(&face, &authority, clock);
    std::thread::sleep(std::time::Duration::from_millis(20));
    let second = plain(&face, &authority, clock);
    assert_ne!(column(&first, "t"), column(&second, "t"), "the clock moves between two calls");
    assert_eq!((counts(&face).hits, counts(&face).misses, counts(&face).bypassed), (0, 0, 2));

    for sql in [
        r#"SELECT count(*) AS n FROM "dash/events" WHERE current_timestamp > TIMESTAMPTZ '2000-01-01 00:00:00+00'"#,
        r#"SELECT count(*) AS n FROM "dash/events" WHERE pg_postmaster_start_time() > TIMESTAMPTZ '2000-01-01 00:00:00+00'"#,
        r#"SELECT count(*) AS n FROM "dash/events" WHERE length(current_query()) > 0"#,
        r#"SELECT random() AS x FROM "dash/events""#,
        r#"WITH e AS (SELECT event_id, uuid() AS u FROM "dash/events") SELECT count(DISTINCT u) AS n FROM e"#,
    ] {
        plain(&face, &authority, sql);
        plain(&face, &authority, sql);
    }
    assert_eq!((counts(&face).hits, counts(&face).misses, counts(&face).entries), (0, 0, 0));
    assert_eq!(counts(&face).bypassed, 12);

    // A function consistent across queries caches.
    let lowered = r#"SELECT lower(event_id) AS e FROM "dash/events" ORDER BY 1"#;
    plain(&face, &authority, lowered);
    plain(&face, &authority, lowered);
    assert_eq!((counts(&face).hits, counts(&face).misses), (1, 1));
}
