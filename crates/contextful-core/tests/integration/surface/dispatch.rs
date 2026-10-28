//! `surface.dispatch`: the bounded fire pool and its exclusion keys.

use contextful_core::surface::dispatch::{admit, Due, DEFAULT_POOL};
use contextful_core::time::Instant;
use std::collections::BTreeSet;

fn due(key: &str, at: &str) -> Due {
    Due { key: key.into(), at: Instant::parse(at).unwrap(), priority: 0, base: None }
}

fn ranked(key: &str, priority: i64, base: Option<&str>) -> Due {
    Due { priority, base: base.map(|b| Instant::parse(b).unwrap()), ..due(key, "2030-01-01T01:00:00Z") }
}

fn keys(units: &[Due]) -> Vec<&str> {
    units.iter().map(|u| u.key.as_str()).collect()
}

/// A deployment's fire pool runs at most 4 units at once, `[control] pool` replacing the bound; a due unit past
/// the bound stays armed and reports pending.
// spec: surface.dispatch.pool-bound@7fdcf605
#[test]
fn the_pool_starts_at_most_its_bound() {
    assert_eq!(DEFAULT_POOL, 4);
    let units: Vec<Due> = ["e", "d", "c", "b", "a", "f"].iter().map(|k| due(k, "2030-01-01T00:00:00Z")).collect();
    let none = BTreeSet::new();
    let a = admit(&units, &none, DEFAULT_POOL);
    // Units tied on instant, priority and base start by id.
    assert_eq!(keys(&a.start), ["a", "b", "c", "d"]);
    assert_eq!(keys(&a.pending), ["e", "f"]);
    // Units in flight count against the bound.
    let busy: BTreeSet<String> = ["x".to_string(), "y".to_string(), "z".to_string()].into();
    let a = admit(&units, &busy, DEFAULT_POOL);
    assert_eq!(keys(&a.start), ["a"]);
    assert_eq!(a.pending.len(), 5);
    // A configured bound replaces the default; an earlier due instant goes first.
    let units = vec![due("late", "2030-01-01T00:05:00Z"), due("early", "2030-01-01T00:01:00Z")];
    let a = admit(&units, &none, 1);
    assert_eq!((keys(&a.start), keys(&a.pending)), (vec!["early"], vec!["late"]));
}

/// A unit's exclusion key is its pipeline id: a unit due while another under its key is in flight starts no
/// second instance, and its next fire recomputes once the first ends.
// spec: surface.dispatch.exclusion-key@466d91d4
#[test]
fn a_key_in_flight_starts_no_second_instance() {
    let units = vec![due("orders", "2030-01-01T00:00:00Z"), due("orders", "2030-01-01T00:00:00Z"), due("filings", "2030-01-01T00:00:00Z")];
    let in_flight: BTreeSet<String> = ["orders".to_string()].into();
    let a = admit(&units, &in_flight, 4);
    assert!(in_flight.contains("orders"), "the key is in flight before admission");
    assert_eq!(keys(&a.start), ["filings"]);
    assert_eq!(keys(&a.held), ["orders"], "held once, neither started nor pending");
    assert!(a.pending.is_empty());
    // With nothing in flight, one key still starts one instance.
    let a = admit(&units, &BTreeSet::new(), 4);
    assert_eq!(keys(&a.start), ["filings", "orders"]);
}

/// Due units start with each unit holding neither a last run start nor a last dispatch first,
/// then by earliest next fire, then highest `priority`, then the least recent of the two, then id.
// spec: surface.dispatch.admission-order@16dc4906
#[test]
fn ties_start_by_priority_then_least_recent_dispatch() {
    let none = BTreeSet::new();
    let units = vec![
        ranked("a", 0, Some("2030-01-01T00:40:00Z")),
        ranked("b", 0, Some("2030-01-01T00:10:00Z")),
        ranked("c", 0, None),
        ranked("d", 5, Some("2030-01-01T00:50:00Z")),
        ranked("e", -1, None),
    ];
    let a = admit(&units, &none, 5);
    assert_eq!(keys(&a.start), ["c", "e", "d", "b", "a"]);
    // Among units with history an earlier due instant outranks any priority, so a unit left
    // pending goes first next.
    let mut late: Vec<Due> = units.iter().filter(|u| u.base.is_some()).cloned().collect();
    assert_eq!(late.len(), 3);
    late.push(Due { base: Some(Instant::parse("2029-12-31T23:00:00Z").unwrap()), ..due("z", "2030-01-01T00:00:00Z") });
    let a = admit(&late, &none, 1);
    assert_eq!((keys(&a.start), keys(&a.pending)), (vec!["z"], vec!["d", "b", "a"]));
}

/// A unit holding neither a run nor a dispatch falls due at the instant its process armed it,
/// so it starts ahead of a unit due since an earlier slot: a serve restarting every wake
/// otherwise arms it behind each backlogged unit at every start.
// spec: surface.dispatch.admission-order@16dc4906
#[test]
fn a_never_run_unit_starts_before_a_unit_due_earlier() {
    let none = BTreeSet::new();
    let backlog = Due { base: Some(Instant::parse("2030-01-01T00:00:00Z").unwrap()), ..due("backlog", "2030-01-01T01:00:00Z") };
    let fresh = due("fresh", "2030-01-01T14:00:00Z");
    let a = admit(&[backlog, fresh], &none, 1);
    assert_eq!((keys(&a.start), keys(&a.pending)), (vec!["fresh"], vec!["backlog"]));
}

/// A unit's dispatch record key spells each id apart, a `job:<name>` id included, and keeps
/// ':' out of the stored spelling.
#[test]
fn each_unit_id_spells_its_own_dispatch_record() {
    use contextful_core::coordinate::LeaseKey;
    let ids = ["a:b", "a%3Ab", "a%253Ab", "job:nightly", "job%3Anightly"];
    let spellings: BTreeSet<String> = ids.iter().map(|id| LeaseKey::Dispatch(id.to_string()).spelling()).collect();
    assert_eq!(spellings.len(), ids.len(), "{spellings:?}");
    assert!(spellings.iter().all(|s| !s.contains(':')), "{spellings:?}");
}
