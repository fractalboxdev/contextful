//! `disclosure.suppress`: the size floor, the share ceiling and the one sentinel.

use contextful_core::disclosure::suppress::{
    audit_attributes, evaluate_group, suppress, GroupDecision, GroupStats, Sentinel, SuppressPolicy, SuppressReason,
    SuppressTally, DISCLOSURE_SENTINEL, MIN_GROUP_SIZE_FLOOR,
};
use contextful_core::disclosure::DisclosureError;

fn policy(k: Option<u64>, share: Option<f64>) -> SuppressPolicy {
    SuppressPolicy::new(k, share).unwrap()
}

fn group(noised_contributors: u64, masses: Option<Vec<f64>>) -> GroupStats {
    GroupStats { noised_contributors, contributor_masses: masses }
}

/// `min_group_size` counts distinct contributors and is at least 2 subjects. A group whose noised distinct-contributor count falls under it is suppressed, and a smaller declared value raises `DisclosureMinGroupSizeBelowFloor`.
// spec: disclosure.suppress.min-group-size@c1e52b47
#[test]
fn min_group_size() {
    assert_eq!(MIN_GROUP_SIZE_FLOOR, 2);
    for k in [0, 1] {
        match SuppressPolicy::new(Some(k), None) {
            Err(e @ DisclosureError::MinGroupSizeBelowFloor(_)) => assert_eq!(e.identifier(), "DisclosureMinGroupSizeBelowFloor"),
            other => panic!("{k}: {other:?}"),
        }
    }
    let p = policy(Some(50), None);
    assert_eq!(p.min_group_size(), Some(50));
    assert_eq!(evaluate_group(&p, &group(49, None)), GroupDecision::Suppress(SuppressReason::BelowMinGroupSize));
    assert_eq!(evaluate_group(&p, &group(50, None)), GroupDecision::Publish);
    assert_eq!(evaluate_group(&policy(Some(2), None), &group(1, None)), GroupDecision::Suppress(SuppressReason::BelowMinGroupSize));
    let out = suppress(&p, vec![("small", group(3, None)), ("large", group(60, None))]);
    assert_eq!(out.published, vec!["large"]);
    assert_eq!(out.sentinel, Some(Sentinel));
    assert_eq!(out.tally.below_min_group_size, 1);
}

/// `max_contributor_share` lies above 0 percent and at most 100 percent of a group's sign-insensitive metric mass; a value outside raises `DisclosureShareOutOfRange`.
// spec: disclosure.suppress.contributor-share@ecca4dee
#[test]
fn contributor_share() {
    for share in [0.0, -0.1, 1.5, f64::NAN, f64::INFINITY] {
        match SuppressPolicy::new(None, Some(share)) {
            Err(e @ DisclosureError::ShareOutOfRange(_)) => assert_eq!(e.identifier(), "DisclosureShareOutOfRange"),
            other => panic!("{share}: {other:?}"),
        }
    }
    assert!(SuppressPolicy::new(None, Some(1.0)).is_ok());
    assert!(SuppressPolicy::new(None, Some(f64::MIN_POSITIVE)).is_ok());

    // A group large enough for the floor, one contributor holding 80% of the mass.
    let p = policy(Some(6), Some(0.5));
    let whale = group(6, Some(vec![80.0, 4.0, 4.0, 4.0, 4.0, 4.0]));
    assert_eq!(evaluate_group(&p, &whale), GroupDecision::Suppress(SuppressReason::DominantContributor));
    let balanced = group(6, Some(vec![20.0, 15.0, 20.0, 15.0, 15.0, 15.0]));
    assert_eq!(evaluate_group(&p, &balanced), GroupDecision::Publish);

    // A top share equal to the ceiling is not above it.
    let ceiling = policy(None, Some(0.5));
    assert_eq!(evaluate_group(&ceiling, &group(2, Some(vec![50.0, 50.0]))), GroupDecision::Publish);
    // Mass is sign-insensitive: a large negative contribution dominates too.
    assert_eq!(
        evaluate_group(&ceiling, &group(3, Some(vec![-80.0, 10.0, 10.0]))),
        GroupDecision::Suppress(SuppressReason::DominantContributor)
    );
    // Zero total mass discloses nothing.
    assert_eq!(evaluate_group(&ceiling, &group(3, Some(vec![0.0, 0.0, 0.0]))), GroupDecision::Publish);
}

/// A group under a share constraint whose per-contributor masses are unavailable raises `DisclosureDominanceUnverifiable` and is suppressed.
// spec: disclosure.suppress.dominance-unverifiable@cd1c5f50
#[test]
fn dominance_unverifiable() {
    let p = policy(None, Some(0.5));
    assert_eq!(evaluate_group(&p, &group(100, None)), GroupDecision::Suppress(SuppressReason::DominanceUnverifiable));
    assert_eq!(
        evaluate_group(&p, &group(100, Some(vec![1.0, f64::NAN]))),
        GroupDecision::Suppress(SuppressReason::DominanceUnverifiable)
    );
    // An empty mass list verifies no contributor's share.
    assert_eq!(evaluate_group(&p, &group(100, Some(vec![]))), GroupDecision::Suppress(SuppressReason::DominanceUnverifiable));
    // Without a share constraint, absent masses decide nothing.
    assert_eq!(evaluate_group(&policy(Some(10), None), &group(100, None)), GroupDecision::Publish);

    let out = suppress(&p, vec![("blind", group(100, None)), ("seen", group(4, Some(vec![1.0, 1.0, 1.0, 1.0])))]);
    assert_eq!(out.published, vec!["seen"]);
    assert_eq!(out.sentinel, Some(Sentinel));
    assert_eq!(out.tally.dominance_unverifiable, 1);
    assert_eq!(out.refusals.len(), 1);
    assert_eq!(out.refusals[0].identifier(), "DisclosureDominanceUnverifiable");
    assert!(!out.refusals[0].to_string().contains("blind"), "{}", out.refusals[0]);
}

/// A policy setting neither threshold raises `DisclosurePolicySuppressesNothing`.
// spec: disclosure.suppress.empty-policy@999bfe8c
#[test]
fn empty_policy() {
    match SuppressPolicy::new(None, None) {
        Err(e @ DisclosureError::PolicySuppressesNothing(_)) => assert_eq!(e.identifier(), "DisclosurePolicySuppressesNothing"),
        other => panic!("{other:?}"),
    }
    let parsed: Result<SuppressPolicy, _> = serde_json::from_str("{}");
    let err = parsed.unwrap_err().to_string();
    assert!(err.contains("DisclosurePolicySuppressesNothing"), "{err}");
}

#[test]
fn a_policy_parses_from_its_declaration_and_validates_on_load() {
    let p: SuppressPolicy = toml::from_str("min_group_size = 5\nmax_contributor_share = 0.4\n").unwrap();
    assert_eq!((p.min_group_size(), p.max_contributor_share()), (Some(5), Some(0.4)));
    let err = toml::from_str::<SuppressPolicy>("min_group_size = 1\n").unwrap_err().to_string();
    assert!(err.contains("DisclosureMinGroupSizeBelowFloor"), "{err}");
    let err = toml::from_str::<SuppressPolicy>("max_contributor_share = 40.0\n").unwrap_err().to_string();
    assert!(err.contains("DisclosureShareOutOfRange"), "{err}");
}

#[test]
fn the_size_floor_decides_before_the_share_ceiling() {
    let p = policy(Some(10), Some(0.5));
    assert_eq!(evaluate_group(&p, &group(2, Some(vec![99.0, 1.0]))), GroupDecision::Suppress(SuppressReason::BelowMinGroupSize));
    assert_eq!(evaluate_group(&p, &group(2, None)), GroupDecision::Suppress(SuppressReason::BelowMinGroupSize));
}

#[test]
fn every_withheld_group_collapses_into_one_reason_free_sentinel() {
    let p = policy(Some(3), Some(0.5));
    let out = suppress(
        &p,
        vec![
            ("ok", group(5, Some(vec![20.0; 5]))),
            ("small", group(2, Some(vec![10.0, 10.0]))),
            ("whale", group(4, Some(vec![97.0, 1.0, 1.0, 1.0]))),
        ],
    );
    assert_eq!(out.published, vec!["ok"]);
    assert_eq!(out.sentinel.map(|s| s.key()), Some(DISCLOSURE_SENTINEL));
    assert_eq!(DISCLOSURE_SENTINEL, "__suppressed__");
    assert_eq!(out.tally, SuppressTally { below_min_group_size: 1, dominant_contributor: 1, dominance_unverifiable: 0 });

    // A size-only and a dominance-only withholding leave identical caller-facing output.
    let small = suppress(&p, vec![("g", group(2, Some(vec![5.0, 5.0])))]);
    let whale = suppress(&p, vec![("g", group(4, Some(vec![97.0, 1.0, 1.0, 1.0])))]);
    assert_eq!((small.published, small.sentinel), (whale.published, whale.sentinel));
    assert_ne!(small.tally, whale.tally);

    let clear = suppress(&policy(Some(2), None), vec![("a", group(3, None)), ("b", group(4, None))]);
    assert_eq!(clear.published, vec!["a", "b"]);
    assert_eq!(clear.sentinel, None);
}

#[test]
fn the_audit_record_carries_the_thresholds_and_per_reason_counts() {
    let tally = SuppressTally { below_min_group_size: 2, dominant_contributor: 1, dominance_unverifiable: 0 };
    let attrs = audit_attributes(&policy(Some(50), Some(0.5)), &tally);
    assert_eq!(attrs["contextful.disclosure.suppressed_groups"], 3);
    assert_eq!(attrs["contextful.disclosure.below_min_group_size"], 2);
    assert_eq!(attrs["contextful.disclosure.dominant_contributor"], 1);
    assert_eq!(attrs["contextful.disclosure.min_group_size"], 50);
    assert_eq!(attrs["contextful.disclosure.max_contributor_share"], 0.5);
}
