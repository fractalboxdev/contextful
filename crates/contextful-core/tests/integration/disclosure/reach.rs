//! `disclosure.reach`: a subject's seed principals, the bounded group closure, the
//! reachable set over a source's current rows, and the reachable-set cache.

use contextful_core::disclosure::reach::{
    closure, confers_read, expired_public, resolve, resolve_mapped, ClosureBound, Held, ReachCache, ReachError, ReachKey, ReachSet, MAX_CLOSURE_NODES, MAX_GROUP_DEPTH, REACH_CACHE_ENTRIES,
};
use contextful_core::disclosure::sweep::{commit, parse_directory, Governed, GrantRow, MemberRow, Observation, ResourceRow, SourceState, SweepKind};
use contextful_core::disclosure::VisibilityError;
use contextful_core::identify::{NormalizedSubject, Subject};
use contextful_core::time::Instant;
use std::collections::{BTreeMap, BTreeSet};

fn at(s: &str) -> Instant {
    Instant::parse(s).unwrap()
}

fn subject(on_behalf_of: &str) -> NormalizedSubject {
    Subject { on_behalf_of: Some(on_behalf_of.into()), agent: Some("agent://wiki-loop".into()), ..Subject::default() }.normalize()
}

fn member(group: &str, member: &str, kind: &str) -> MemberRow {
    MemberRow { group: group.into(), member: member.into(), member_kind: kind.into(), observed_at: at("2030-01-01T00:00:00Z") }
}

fn resource(id: &str, class: &str) -> ResourceRow {
    ResourceRow {
        resource_id: id.into(),
        resource_kind: "page".into(),
        visibility_class: class.into(),
        observed_at: at("2030-01-01T00:00:00Z"),
        sweep_kind: "full".into(),
        sweep_id: "s1".into(),
    }
}

fn grant(resource: &str, principal: &str, kind: &str) -> GrantRow {
    GrantRow {
        resource_id: resource.into(),
        principal: principal.into(),
        principal_kind: kind.into(),
        level: "viewer".into(),
        expires_at: None,
        observed_at: at("2030-01-01T00:00:00Z"),
        sweep_kind: "full".into(),
        sweep_id: "s1".into(),
    }
}

fn ids(set: &ReachSet) -> Vec<&str> {
    set.resources().into_iter().collect()
}

/// A chain `u -> g1 -> g2 -> ... -> g<n>` of `n` group edges.
fn chain(n: usize) -> Vec<MemberRow> {
    (1..=n).map(|i| if i == 1 { member("g1", "dana@acme.example", "user") } else { member(&format!("g{i}"), &format!("g{}", i - 1), "group") }).collect()
}

/// A subject `user://<p>` reaches the `user` principal spelled `<p>` and each principal an `identity_links` row maps to it, then the groups those principals belong to.
// spec: disclosure.reach.subject-principals@e8f0eba6
#[test]
fn a_subject_reaches_its_user_principal_its_linked_principals_and_their_groups() {
    let links = parse_directory("wiki", r#"{"source_principal": "u-1001", "subject": "user://dana@acme.example", "method": "scim_email", "confidence": 1.0}"#).unwrap().links;
    let state = SourceState {
        resources: ["p1", "p2", "p3", "p4"].iter().map(|r| resource(r, "mirrored")).collect(),
        grants: vec![grant("p1", "dana@acme.example", "user"), grant("p2", "u-1001", "user"), grant("p3", "g-eng", "group"), grant("p4", "u-2002", "user")],
        members: vec![member("g-eng", "u-1001", "user")],
        links,
        freshness: None,
    };
    assert_eq!(ids(&resolve("wiki", &subject("user://dana@acme.example"), &state, ClosureBound::default()).unwrap()), ["p1", "p2", "p3"]);
    // Another subject holds none of dana's principals.
    assert!(resolve("wiki", &subject("user://erin@acme.example"), &state, ClosureBound::default()).unwrap().resources().is_empty());
}

/// The closure walks the group graph with a visited set, ending any cycle, to a depth of `max_group_depth`, default 8 hops, and a breadth of 10000 nodes.
// spec: disclosure.reach.closure-walk@66923490
#[test]
fn the_closure_walks_eight_hops_and_ten_thousand_nodes_ending_cycles() {
    assert_eq!((MAX_GROUP_DEPTH, MAX_CLOSURE_NODES), (8, 10_000));
    let dana = subject("user://dana@acme.example");
    let seeds = BTreeSet::from([("user".to_string(), "dana@acme.example".to_string())]);

    // Eight hops resolve: the seed and eight groups.
    let walked = closure("wiki", &dana, seeds.clone(), &chain(8), ClosureBound::default()).unwrap();
    assert_eq!(walked.len(), 9);
    assert!(walked.contains(&("group".to_string(), "g8".to_string())));

    // A cycle ends at the visited set.
    let mut cyclic = chain(3);
    cyclic.push(member("g1", "g3", "group"));
    assert_eq!(closure("wiki", &dana, seeds.clone(), &cyclic, ClosureBound::default()).unwrap().len(), 4);

    // 9999 groups one hop away, with the seed, fill the closure to its 10000-node breadth.
    let wide: Vec<MemberRow> = (0..MAX_CLOSURE_NODES - 1).map(|i| member(&format!("w{i}"), "dana@acme.example", "user")).collect();
    assert_eq!(closure("wiki", &dana, seeds, &wide, ClosureBound::default()).unwrap().len(), MAX_CLOSURE_NODES);

    // `max_group_depth` narrows the walk.
    let shallow = ClosureBound { max_group_depth: 2, ..ClosureBound::default() };
    assert!(closure("wiki", &dana, BTreeSet::from([("user".to_string(), "dana@acme.example".to_string())]), &chain(2), shallow).is_ok());
}

/// A closure reaching its depth or node bound raises `VisibilityClosureDepth` with the non-retryable HTTP status 422 and returns no set.
#[test]
fn a_closure_past_its_depth_or_node_bound_is_refused_with_422() {
    let dana = subject("user://dana@acme.example");
    let state = |members: Vec<MemberRow>| SourceState {
        resources: vec![resource("p1", "mirrored")],
        grants: vec![grant("p1", "dana@acme.example", "user")],
        members,
        ..SourceState::default()
    };
    // A ninth hop refuses, and the user grant it already reached is not returned.
    match resolve("wiki", &dana, &state(chain(9)), ClosureBound::default()) {
        Err(ReachError::Visibility(e @ VisibilityError::ClosureDepth { .. })) => {
            assert_eq!(e.identifier(), "VisibilityClosureDepth");
            assert_eq!(e.status(), Some(422));
            assert!(e.to_string().contains("8 hops"), "{e}");
        }
        other => panic!("{other:?}"),
    }
    // A 10001st node refuses.
    let wide: Vec<MemberRow> = (0..MAX_CLOSURE_NODES).map(|i| member(&format!("w{i}"), "dana@acme.example", "user")).collect();
    match resolve("wiki", &dana, &state(wide), ClosureBound::default()) {
        Err(ReachError::Visibility(e @ VisibilityError::ClosureDepth { .. })) => assert!(e.to_string().contains("10000 nodes"), "{e}"),
        other => panic!("{other:?}"),
    }
    // A declared `max_group_depth` of 2 refuses the third hop.
    let shallow = ClosureBound { max_group_depth: 2, ..ClosureBound::default() };
    assert!(matches!(resolve("wiki", &dana, &state(chain(3)), shallow), Err(ReachError::Visibility(VisibilityError::ClosureDepth { .. }))));
}

fn key(subject: &str, sources: &[(&str, u64)]) -> ReachKey {
    ReachKey { subject: subject.into(), directory_epoch: 1, sources: sources.iter().map(|(s, e)| (s.to_string(), *e)).collect::<BTreeMap<_, _>>() }
}

fn set(ids: &[&str]) -> ReachSet {
    let held = BTreeSet::from([Held { level: "read".into(), expires_at: None }]);
    ReachSet { held: ids.iter().map(|s| (s.to_string(), held.clone())).collect(), degraded: false }
}

/// The reachable-set cache holds at most 100000 entries and evicts the least recently used.
// spec: disclosure.reach.cache-capacity@7081dc2f
#[test]
fn the_reachable_set_cache_holds_one_hundred_thousand_entries_evicting_the_least_recently_used() {
    assert_eq!(REACH_CACHE_ENTRIES, 100_000);
    let mut cache = ReachCache::default();
    for i in 0..REACH_CACHE_ENTRIES {
        cache.insert(key(&format!("user://u{i}"), &[("wiki", 1)]), set(&["p1"])).unwrap();
    }
    assert_eq!(cache.len(), REACH_CACHE_ENTRIES);
    // Reading u0 makes u1 the least recently used; one more entry evicts u1 and keeps u0.
    assert!(cache.get(&key("user://u0", &[("wiki", 1)])).is_some());
    cache.insert(key("user://extra", &[("wiki", 1)]), set(&["p2"])).unwrap();
    assert_eq!(cache.len(), REACH_CACHE_ENTRIES);
    assert!(cache.get(&key("user://u1", &[("wiki", 1)])).is_none());
    assert!(cache.get(&key("user://u0", &[("wiki", 1)])).is_some());
    assert_eq!(cache.get(&key("user://extra", &[("wiki", 1)])).unwrap().resources().into_iter().collect::<Vec<_>>(), ["p2"]);
}

/// Retaining a reachable set or result produced past a budget, or under the narrowing posture, raises `VisibilityDegradedCached`.
// spec: disclosure.reach.degraded-uncached@c083cd7c
#[test]
fn a_degraded_reachable_set_is_refused_and_not_retained() {
    let mut cache = ReachCache::default();
    let degraded = ReachSet { degraded: true, ..set(&["p1"]) };
    match cache.insert(key("user://dana@acme.example", &[("wiki", 1)]), degraded) {
        Err(e @ VisibilityError::DegradedCached { .. }) => assert_eq!(e.identifier(), "VisibilityDegradedCached"),
        other => panic!("{other:?}"),
    }
    assert!(cache.is_empty());
    assert!(cache.get(&key("user://dana@acme.example", &[("wiki", 1)])).is_none());
}

/// The reachable-set cache keys on the subject, the directory epoch and the epoch of each source the read touches, so a sweep invalidates only sets reading its source.
// spec: disclosure.reach.cache-epochs@5ae0f41e
#[test]
fn a_sweep_invalidates_only_the_sets_reading_its_source() {
    let mut cache = ReachCache::default();
    cache.insert(key("user://dana@acme.example", &[("wiki", 3)]), set(&["p1"])).unwrap();
    cache.insert(key("user://dana@acme.example", &[("crm", 7), ("wiki", 3)]), set(&["p1", "a1"])).unwrap();

    // A crm sweep moves crm to epoch 8: the wiki-only set still answers; the set reading crm does not.
    assert!(cache.get(&key("user://dana@acme.example", &[("wiki", 3)])).is_some());
    assert!(cache.get(&key("user://dana@acme.example", &[("crm", 8), ("wiki", 3)])).is_none());
    // A wiki sweep invalidates both.
    assert!(cache.get(&key("user://dana@acme.example", &[("wiki", 4)])).is_none());
    // Another subject, or another directory epoch, is another key.
    assert!(cache.get(&key("user://erin@acme.example", &[("wiki", 3)])).is_none());
    let moved = ReachKey { directory_epoch: 2, ..key("user://dana@acme.example", &[("wiki", 3)]) };
    assert!(cache.get(&moved).is_none());
}

/// A grant on a resource of the unknown class, or on no resource row, joins no reachable set.
#[test]
fn a_grant_on_an_unknown_class_or_absent_resource_reaches_nothing() {
    let state = SourceState {
        resources: vec![resource("p1", "mirrored"), resource("p2", "unknown")],
        grants: vec![grant("p1", "dana@acme.example", "user"), grant("p2", "dana@acme.example", "user"), grant("p3", "dana@acme.example", "user")],
        ..SourceState::default()
    };
    assert_eq!(ids(&resolve("wiki", &subject("user://dana@acme.example"), &state, ClosureBound::default()).unwrap()), ["p1"]);
}

fn levels() -> BTreeMap<String, String> {
    [("viewer", "read"), ("editor", "write"), ("owner", "admin"), ("lister", "list")].iter().map(|(k, v)| (k.to_string(), v.to_string())).collect()
}

fn graded(resource: &str, principal: &str, kind: &str, level: &str, expires_at: Option<&str>) -> GrantRow {
    GrantRow { level: level.into(), expires_at: expires_at.map(at), ..grant(resource, principal, kind) }
}

fn admitted(set: &ReachSet, levels: Option<&BTreeMap<String, String>>, now: &str) -> Vec<String> {
    set.admitted(levels, at(now)).into_iter().collect()
}

// spec: disclosure.mirror.unmirrorable-opening@9e9da0b0
/// The semi-join opening {{authority.compose.relation-order}} admits a row of a bound table when its `resource_key` value names a resource with a read-conferring, unexpired, untombstoned grant in the reader's reachable set.
// spec: disclosure.reach.semi-join@6f8b5b64
#[test]
fn the_semi_join_admits_resources_holding_a_read_conferring_unexpired_current_grant() {
    let dana = subject("user://dana@acme.example");
    let prior = SourceState {
        resources: ["p1", "p2", "p3", "p4", "p5", "p6"].iter().map(|r| resource(r, "mirrored")).collect(),
        grants: vec![
            graded("p1", "dana@acme.example", "user", "viewer", None),
            graded("p2", "dana@acme.example", "user", "editor", None),
            graded("p3", "dana@acme.example", "user", "owner", None),
            graded("p4", "dana@acme.example", "user", "lister", None),
            graded("p5", "dana@acme.example", "user", "viewer", Some("2030-01-01T11:00:00Z")),
            graded("p6", "dana@acme.example", "user", "viewer", Some("2030-01-01T13:00:00Z")),
        ],
        ..SourceState::default()
    };
    let levels = levels();
    let set = resolve("wiki", &dana, &prior, ClosureBound::default()).unwrap();
    // Every held grant reaches; the level and the expiry decide what reads.
    assert_eq!(ids(&set), ["p1", "p2", "p3", "p4", "p5", "p6"]);
    assert_eq!(admitted(&set, Some(&levels), "2030-01-01T12:00:00Z"), ["p1", "p2", "p3", "p6"]);
    assert_eq!(set.expired(at("2030-01-01T12:00:00Z")), 1);
    // From the instant it expires, a grant confers nothing.
    assert_eq!(admitted(&set, Some(&levels), "2030-01-01T13:00:00Z"), ["p1", "p2", "p3"]);
    assert_eq!(set.expired(at("2030-01-01T13:00:00Z")), 2);

    // An incremental run withdrawing p2's grant tombstones it; the opening on p6 stands.
    let governed = vec![Governed { resource_id: "p2".into(), resource_kind: "page".into(), visibility_class: "mirrored".into() }];
    let obs = Observation::from_grants_file("wiki", SweepKind::Incremental, "s2", at("2030-01-01T12:00:00Z"), governed, Vec::new());
    let c = commit(&prior, &obs).unwrap();
    assert!(c.tombstones.iter().any(|t| t.scope == "grant" && t.resource_id == "p2"));
    let after = SourceState { resources: c.resources, grants: c.grants, ..SourceState::default() };
    let set = resolve("wiki", &dana, &after, ClosureBound::default()).unwrap();
    assert_eq!(admitted(&set, Some(&levels), "2030-01-01T12:30:00Z"), ["p1", "p3", "p6"]);
}

/// A grant confers read when its `level`, mapped through the `level` table of the pack mapping its table, or taken verbatim where no pack maps that table, is `read`, `write` or `admin`.
// spec: disclosure.reach.read-level@7cd0479b
#[test]
fn a_level_confers_read_through_the_pack_mapping_or_verbatim() {
    let levels = levels();
    for level in ["viewer", "editor", "owner"] {
        assert!(confers_read(level, Some(&levels)), "{level}");
    }
    // A level the mapping projects elsewhere, or does not name, confers nothing; the mapping
    // governs a verbatim spelling too.
    for level in ["lister", "commenter", "read"] {
        assert!(!confers_read(level, Some(&levels)), "{level}");
    }
    // With no pack mapping the table, the level reads verbatim.
    for level in ["read", "write", "admin"] {
        assert!(confers_read(level, None), "{level}");
    }
    for level in ["viewer", "READ", ""] {
        assert!(!confers_read(level, None), "{level}");
    }
}

/// A grant whose `principal_kind`, mapped through the `principal_kind` table of the pack mapping its table, or taken verbatim where that table names no such kind, is `public` joins every subject's reachable set.
// spec: disclosure.reach.public-grant@85175e28
#[test]
fn a_public_grant_joins_every_subject_reachable_set() {
    let state = SourceState {
        resources: ["p1", "p2"].iter().map(|r| resource(r, "mirrored")).collect(),
        grants: vec![graded("p1", "anyone", "public", "read", None), graded("p2", "dana@acme.example", "user", "read", None)],
        ..SourceState::default()
    };
    let now = "2030-01-01T00:00:00Z";
    let reach = |s: &str| admitted(&resolve("wiki", &subject(s), &state, ClosureBound::default()).unwrap(), None, now);
    assert_eq!(reach("user://dana@acme.example"), ["p1", "p2"]);
    assert_eq!(reach("user://erin@acme.example"), ["p1"]);

    // A pack mapping the source kind `anyone` onto `public` opens its grant to everyone; with
    // no mapping, `anyone` names no principal any subject holds.
    let anyone = SourceState { grants: vec![graded("p1", "everyone", "anyone", "read", None), graded("p2", "dana@acme.example", "user", "read", None)], ..state };
    let kinds = BTreeMap::from([("anyone".to_string(), "public".to_string()), ("user".to_string(), "user".to_string())]);
    let mapped = |s: &str| admitted(&resolve_mapped("wiki", &subject(s), &anyone, ClosureBound::default(), Some(&kinds)).unwrap(), None, now);
    assert_eq!(mapped("user://erin@acme.example"), ["p1"]);
    assert_eq!(mapped("user://dana@acme.example"), ["p1", "p2"]);
    assert!(admitted(&resolve("wiki", &subject("user://erin@acme.example"), &anyone, ClosureBound::default()).unwrap(), None, now).is_empty());
}

/// The count of lapsed `public` openings rises exactly when one lapses, under the source's
/// kind mapping, so a narrowed session keyed on it retires with the opening.
#[test]
fn the_lapsed_public_opening_count_rises_when_an_opening_expires() {
    let state = SourceState {
        resources: ["p1", "p2", "p3"].iter().map(|r| resource(r, "mirrored")).collect(),
        grants: vec![
            graded("p1", "anyone", "public", "read", None),
            graded("p2", "anyone", "public", "read", Some("2030-01-01T00:22:00Z")),
            graded("p3", "dana@acme.example", "user", "read", Some("2030-01-01T00:21:00Z")),
            graded("p3", "everyone", "anyone", "read", Some("2030-01-01T00:21:00Z")),
        ],
        ..SourceState::default()
    };
    assert_eq!(expired_public(&state, None, at("2030-01-01T00:20:00Z")), 0);
    // The user opening on p3 lapses at 00:21 and counts for nothing; `anyone` is no public
    // kind without a mapping.
    assert_eq!(expired_public(&state, None, at("2030-01-01T00:21:30Z")), 0);
    assert_eq!(expired_public(&state, None, at("2030-01-01T00:22:00Z")), 1);
    let kinds = BTreeMap::from([("anyone".to_string(), "public".to_string())]);
    assert_eq!(expired_public(&state, Some(&kinds), at("2030-01-01T00:21:30Z")), 1);
    assert_eq!(expired_public(&state, Some(&kinds), at("2030-01-01T00:22:00Z")), 2);
}
