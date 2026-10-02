//! `surface.dispatch`: the bounded fire pool and its exclusion keys.

use contextful_core::surface::dispatch::{admit, Due, DEFAULT_POOL};
use contextful_core::time::Instant;
use std::collections::BTreeSet;

fn due(key: &str, at: &str) -> Due {
    Due { key: key.into(), at: Instant::parse(at).unwrap() }
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
    // One beat's order: due instant, then id.
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
