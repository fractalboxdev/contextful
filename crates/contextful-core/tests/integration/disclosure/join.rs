//! `disclosure.set-mode`: the cross-owner hashed join under a per-pair pepper.

use contextful_core::disclosure::join::{hashed_join, JoinKey, JoinRow, OwnerPair, PairPepper, PepperLedger};
use contextful_core::disclosure::DisclosureError;

fn pair() -> OwnerPair {
    OwnerPair::new("retailer", "publisher")
}

fn rows(pepper: &PairPepper, keys: &[(&str, &str)]) -> Vec<JoinRow> {
    keys.iter().map(|(key, group)| JoinRow { key: pepper.hash(key), group: group.to_string() }).collect()
}

fn keys(pepper: &PairPepper, keys: &[&str]) -> Vec<JoinKey> {
    keys.iter().map(|k| pepper.hash(k)).collect()
}

/// Two owners match on a key each hashes at write time under a per-pair pepper re-randomized per join; matched rows leave as an aggregate under a group-size floor. A static pepper raises `DisclosureStaticPepper`.
// spec: disclosure.set-mode.hashed-join@f7da71b9
#[test]
fn a_join_reusing_a_pepper_refuses() {
    let pepper = PairPepper::new(pair(), 1, [7u8; 32]);
    let left = rows(&pepper, &[("a@x", "north"), ("b@x", "north"), ("c@x", "north"), ("d@x", "south"), ("e@x", "south")]);
    let right = keys(&pepper, &["a@x", "b@x", "c@x", "d@x", "zz@x"]);
    let mut ledger = PepperLedger::default();

    let release = hashed_join(&pepper, &mut ledger, &left, &right, 3).unwrap();
    assert_eq!(release.groups, vec![("north".to_string(), 3)], "south matched one key, under the floor");
    assert!(release.sentinel.is_some(), "a withheld group leaves the sentinel");
    assert_eq!(release.version, 1);

    // The keyed digest is neither the key nor a bare hash of it.
    assert_ne!(pepper.hash("a@x").digest, "a@x");
    assert_ne!(pepper.hash("a@x").digest, PairPepper::new(pair(), 2, [8u8; 32]).hash("a@x").digest);

    // The same pepper a second time refuses, whatever version it is presented under.
    for again in [PairPepper::new(pair(), 1, [7u8; 32]), PairPepper::new(pair(), 2, [7u8; 32])] {
        let left = rows(&again, &[("a@x", "north")]);
        let right = keys(&again, &["a@x"]);
        match hashed_join(&again, &mut ledger, &left, &right, 3) {
            Err(e @ DisclosureError::StaticPepper(_)) => assert_eq!(e.identifier(), "DisclosureStaticPepper"),
            other => panic!("{other:?}"),
        }
    }

    // A pepper one pair joined under refuses for another pair as well.
    let other = PairPepper::new(OwnerPair::new("retailer", "bank"), 1, [7u8; 32]);
    assert!(matches!(hashed_join(&other, &mut ledger, &[], &[], 3), Err(DisclosureError::StaticPepper(_))));

    // A fresh pepper whose version does not advance past the pair's last join refuses.
    let stale = PairPepper::new(pair(), 1, [9u8; 32]);
    assert!(matches!(hashed_join(&stale, &mut ledger, &[], &[], 3), Err(DisclosureError::StaticPepper(_))));

    // A fresh pepper at the next version joins.
    let next = PairPepper::new(pair(), 2, [9u8; 32]);
    let left = rows(&next, &[("a@x", "north"), ("b@x", "north")]);
    let right = keys(&next, &["a@x", "b@x"]);
    let release = hashed_join(&next, &mut ledger, &left, &right, 2).unwrap();
    assert_eq!(release.groups, vec![("north".to_string(), 2)]);
    assert!(release.sentinel.is_none());

    // The ledger round-trips without carrying a pepper.
    let text = serde_json::to_string(&ledger).unwrap();
    assert!(!text.contains(&"07".repeat(32)) && !text.contains(&"09".repeat(32)));
    let mut restored: PepperLedger = serde_json::from_str(&text).unwrap();
    assert!(matches!(hashed_join(&next, &mut restored, &[], &[], 2), Err(DisclosureError::StaticPepper(_))));
}

/// The group-size floor holds at the suppression floor, and a refused floor spends no pepper.
#[test]
fn a_floor_below_the_suppression_floor_refuses_before_the_pepper_spends() {
    let pepper = PairPepper::new(pair(), 1, [3u8; 32]);
    let mut ledger = PepperLedger::default();
    assert!(matches!(hashed_join(&pepper, &mut ledger, &[], &[], 1), Err(DisclosureError::MinGroupSizeBelowFloor(_))));
    assert!(hashed_join(&pepper, &mut ledger, &[], &[], 2).is_ok());
}

/// Each group counts distinct matched keys, so one contributor landed twice counts once.
#[test]
fn a_group_counts_distinct_matched_keys() {
    let pepper = PairPepper::new(pair(), 1, [4u8; 32]);
    let left = rows(&pepper, &[("a@x", "north"), ("a@x", "north"), ("b@x", "north")]);
    let right = keys(&pepper, &["a@x", "a@x"]);
    let mut ledger = PepperLedger::default();
    let release = hashed_join(&pepper, &mut ledger, &left, &right, 2).unwrap();
    assert!(release.groups.is_empty());
    assert!(release.sentinel.is_some());
}

/// A cross-owner join accepts only rows carrying its pair's current pepper version; rotation re-lands both owners' join keys before the next release.
// spec: disclosure.set-mode.pepper-version@7d6ba926
#[test]
fn a_join_matches_only_rows_under_the_current_pepper_version() {
    let old = PairPepper::new(pair(), 1, [5u8; 32]);
    let current = PairPepper::new(pair(), 2, [6u8; 32]);
    let mut ledger = PepperLedger::default();

    // One owner re-landed under the rotated pepper, the other did not: nothing matches.
    let left = rows(&current, &[("a@x", "north"), ("b@x", "north")]);
    let right = keys(&old, &["a@x", "b@x"]);
    let release = hashed_join(&current, &mut ledger, &left, &right, 2).unwrap();
    assert!(release.groups.is_empty());

    // A row presenting the current digest under an old version tag is refused as well.
    let next = PairPepper::new(pair(), 3, [7u8; 32]);
    let left = rows(&next, &[("a@x", "north"), ("b@x", "north")]);
    let right: Vec<JoinKey> = keys(&next, &["a@x", "b@x"]).into_iter().map(|k| JoinKey { version: 2, ..k }).collect();
    assert!(hashed_join(&next, &mut ledger, &left, &right, 2).unwrap().groups.is_empty());

    // Both owners re-landed under the current pepper: the release matches.
    let last = PairPepper::new(pair(), 4, [8u8; 32]);
    let left = rows(&last, &[("a@x", "north"), ("b@x", "north")]);
    let right = keys(&last, &["a@x", "b@x"]);
    assert_eq!(hashed_join(&last, &mut ledger, &left, &right, 2).unwrap().groups, vec![("north".to_string(), 2)]);
}

/// Two pairs whose names concatenate alike keep separate version histories.
#[test]
fn pairs_whose_names_concatenate_alike_keep_separate_versions() {
    let mut ledger = PepperLedger::default();
    ledger.admit(&PairPepper::new(OwnerPair::new("a+b", "c"), 5, [1u8; 32])).unwrap();
    ledger.admit(&PairPepper::new(OwnerPair::new("a", "b+c"), 3, [2u8; 32])).unwrap();
    assert!(matches!(ledger.admit(&PairPepper::new(OwnerPair::new("a", "b+c"), 3, [3u8; 32])), Err(DisclosureError::StaticPepper(_))));
}
