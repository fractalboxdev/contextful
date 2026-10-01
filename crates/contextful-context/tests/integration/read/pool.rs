//! `read.cache`: the session pool — reuse while the whole key holds, a miss on any change
//! to it, and its bounds.

use super::*;
use contextful_context::land::{land, Batch, RunContext};
use contextful_context::read::PoolCounts;
use contextful_core::read::cache::{SESSION_POOL_CONNECTIONS, SESSION_POOL_ENTRIES};
use contextful_core::read::pin::Pins;
use contextful_core::store::bound_time::Bound;
use contextful_core::store::ledger::RequestRecord;
use contextful_policy::issue::MintClaims;
use contextful_core::store::lay_out::NodeId;
use contextful_core::store::reserve::Injection;

/// The table every pooled read touches, landed one run at a time and never folded.
const EVENTS: &str = "bench/events";
const COUNT: &str = r#"SELECT count(*) AS n FROM "bench/events""#;

fn land_run(store: &Store, i: usize) {
    let rows = (0..4)
        .map(|k| json!({ "event_id": format!("e{i}-{k}"), "kind": "open", "body": format!("event {k} of run {i}") }))
        .map(|r| r.as_object().unwrap().clone())
        .collect();
    let ctx = RunContext {
        node: NodeId::parse("ingest-a").unwrap(),
        injection: Injection { run_id: format!("run-{i:04}"), site_id: "site-a".into(), batch_seq: Some(0), authored_by: None, taint: None },
        committed_at: at("2030-01-10T00:00:00Z"),
    };
    land(store, &TableDecl::named(EVENTS), &Batch { rows, types: HashMap::new() }, &ctx).unwrap();
}

fn events(r: &Reads, authority: &AdmittedAuthority) -> Vec<Value> {
    let s = r.face.session(authority, &Request::default(), Bounds::default()).unwrap();
    column(&r.face.query(&s, r#"SELECT event_id FROM "bench/events" ORDER BY event_id"#, ReadOptions::default()).unwrap(), "event_id")
}

fn counts(r: &Reads) -> (u64, u64, u64) {
    let PoolCounts { session_hits, session_misses, engine_opens } = r.face.pool().counts();
    (session_hits, session_misses, engine_opens)
}

/// `Face` reuses a resolved session and its connections while the whole key holds: admitted authority, token id, revocation epoch, request zone, bounds, pin map, table set, and per granted table its schema digest, pointer, run set and ledger files.
// spec: read.cache.session-pool@945a082b
#[test]
fn statements_under_one_key_share_one_resolved_session_and_one_engine() {
    let r = Reads::new();
    land_run(&r.store, 0);
    let authority = r.authority(loop_subject("agent://research-loop"), vec![read(&[EVENTS], None)]);
    let s = r.face.session(&authority, &Request::default(), Bounds::default()).unwrap();
    r.face.query(&s, COUNT, ReadOptions::default()).unwrap();
    r.face.query(&s, COUNT, ReadOptions::default()).unwrap();
    assert_eq!(counts(&r), (0, 1, 1), "two statements on one session open one engine");
    contextful_eval::record::emit("read-session-one-engine", r.face.pool().counts().engine_opens as f64, 2, 0);
    // The same authority, zone and bounds over an unchanged store reuse both.
    assert_eq!(events(&r, &authority).len(), 4);
    assert_eq!(counts(&r), (1, 1, 1));
    // Another zone or other bounds is another key.
    let zoned = r.face.session(&authority, &Request { zone: Some("on-prem:hq") }, Bounds::default()).unwrap();
    r.face.query(&zoned, COUNT, ReadOptions::default()).unwrap();
    let bounded = Bounds { as_of: Some(Bound::parse("2031-01-01T00:00:00Z").unwrap()), valid_as_of: None };
    let s = r.face.session(&authority, &Request::default(), bounded).unwrap();
    r.face.query(&s, COUNT, ReadOptions::default()).unwrap();
    assert_eq!(counts(&r), (1, 3, 3));
    // Another pin map is another key; a map of null pins is the unpinned key.
    let nulled = Pins::parse(Some(&serde_json::json!({ EVENTS: null }))).unwrap();
    r.face.session_pinned(&authority, &Request::default(), Bounds::default(), &nulled).unwrap();
    assert_eq!(counts(&r), (2, 3, 3));
    let pinned = Pins::default().with("bench/other", "snapshot-01773100800000000000");
    let s = r.face.session_pinned(&authority, &Request::default(), Bounds::default(), &pinned).unwrap();
    r.face.query(&s, COUNT, ReadOptions::default()).unwrap();
    assert_eq!(counts(&r), (2, 4, 4));
}

/// A committed run, snapshot, schema edit, new table, request-ledger file or token under another id or epoch misses the pool, and the next statement reads the new state on a new connection.
// spec: read.cache.change-misses@f498fb90
#[test]
fn every_change_to_the_whole_key_misses_and_reads_the_new_state() {
    let r = Reads::new();
    land_run(&r.store, 0);
    let authority = r.authority(loop_subject("agent://research-loop"), vec![read(&["bench/*"], None)]);
    assert_eq!(events(&r, &authority).len(), 4);
    assert_eq!(counts(&r), (0, 1, 1));

    // A committed run: the next session reads its rows on a new connection.
    land_run(&r.store, 1);
    assert_eq!(events(&r, &authority).len(), 8);
    assert_eq!(counts(&r), (0, 2, 2), "a new run misses and opens a fresh engine");
    assert_eq!(r.face.pool().len(), 1, "the principal's older entry leaves the pool");

    // A schema change: a run carrying a new column.
    let decl = TableDecl::named(EVENTS);
    let ctx = |run: &str| RunContext {
        node: NodeId::parse("ingest-a").unwrap(),
        injection: Injection { run_id: run.into(), site_id: "site-a".into(), batch_seq: Some(0), authored_by: None, taint: None },
        committed_at: at("2030-01-10T00:00:00Z"),
    };
    let wide = json!({ "event_id": "w1", "kind": "open", "body": "wide", "extra": "x" }).as_object().unwrap().clone();
    land(&r.store, &decl, &Batch { rows: vec![wide], types: HashMap::new() }, &ctx("run-wide")).unwrap();
    let s = r.face.session(&authority, &Request::default(), Bounds::default()).unwrap();
    let extra = r.face.query(&s, r#"SELECT extra FROM "bench/events" WHERE event_id = 'w1'"#, ReadOptions::default()).unwrap();
    assert_eq!(column(&extra, "extra"), [json!("x")]);
    assert_eq!(counts(&r).1, 3);

    // A new table under the granted prefix.
    let other = json!({ "id": "o1" }).as_object().unwrap().clone();
    land(&r.store, &TableDecl::named("bench/other"), &Batch { rows: vec![other], types: HashMap::new() }, &ctx("run-other")).unwrap();
    let s = r.face.session(&authority, &Request::default(), Bounds::default()).unwrap();
    assert_eq!(column(&r.face.query(&s, r#"SELECT id FROM "bench/other""#, ReadOptions::default()).unwrap(), "id"), [json!("o1")]);
    assert_eq!(counts(&r).1, 4);

    // A request-ledger file of a granted table.
    let record = RequestRecord {
        request_id: "req-1".into(),
        vendor_request_id: None,
        connector: "http".into(),
        method: "GET".into(),
        url_host: "api.example.org".into(),
        status_code: Some(200),
        started_at: at("2030-01-10T00:00:00Z"),
        duration_ms: 12,
        batch_seq: None,
    };
    contextful_context::ledger::append(&r.store, EVENTS, "run-0001", &NodeId::parse("ingest-a").unwrap(), &[record]).unwrap();
    let s = r.face.session(&authority, &Request::default(), Bounds::default()).unwrap();
    let requests = r.face.query(&s, r#"SELECT request_id FROM "bench/events__requests""#, ReadOptions::default()).unwrap();
    assert_eq!(column(&requests, "request_id"), [json!("req-1")]);
    assert_eq!(counts(&r).1, 5);

    // A token minted under a new revocation epoch.
    let renewed = r.authority_under(loop_subject("agent://research-loop"), vec![read(&["bench/*"], None)], MintClaims { epoch: 7, ..MintClaims::default() });
    assert_eq!(renewed.epoch(), 7);
    assert_eq!(events(&r, &renewed).len(), 9);
    assert_eq!(counts(&r), (0, 6, 6), "no entry is reused across a change");
}

/// The pool holds at most 16 entries, oldest evicted first; a miss evicts the same principal's entries under an older store state.
// spec: read.cache.pool-entries@4654ccda
#[test]
fn the_pool_holds_sixteen_entries_and_evicts_the_oldest() {
    let r = Reads::new();
    land_run(&r.store, 0);
    let grant = || vec![read(&[EVENTS], None)];
    let first = r.authority(loop_subject("agent://research-loop"), grant());
    events(&r, &first);
    for _ in 0..SESSION_POOL_ENTRIES {
        events(&r, &r.authority(loop_subject("agent://research-loop"), grant()));
    }
    assert_eq!(r.face.pool().len(), SESSION_POOL_ENTRIES);
    assert_eq!(counts(&r).0, 0);
    events(&r, &first);
    assert_eq!(counts(&r), (0, SESSION_POOL_ENTRIES as u64 + 2, SESSION_POOL_ENTRIES as u64 + 2), "the oldest entry left the pool");
}

/// One pool entry keeps at most 4 connections idle; a concurrent statement past them opens its own and closes it afterwards.
// spec: read.cache.pool-connections@81d18d1d
#[test]
fn an_entry_keeps_at_most_four_idle_connections() {
    let r = Reads::new();
    land_run(&r.store, 0);
    let authority = r.authority(loop_subject("agent://research-loop"), vec![read(&[EVENTS], None)]);
    let s = r.face.session(&authority, &Request::default(), Bounds::default()).unwrap();
    let barrier = std::sync::Barrier::new(8);
    std::thread::scope(|scope| {
        for _ in 0..8 {
            scope.spawn(|| {
                barrier.wait();
                for _ in 0..4 {
                    r.face.query(&s, COUNT, ReadOptions::default()).unwrap();
                }
            });
        }
    });
    assert!(r.face.pool().idle() <= SESSION_POOL_CONNECTIONS, "{} idle connections", r.face.pool().idle());
    let opened = counts(&r).2;
    r.face.query(&s, COUNT, ReadOptions::default()).unwrap();
    assert_eq!(counts(&r).2, opened, "a statement after the burst takes an idle connection");
}
