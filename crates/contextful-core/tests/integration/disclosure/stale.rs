//! `disclosure.bound-staleness` at read and at diagnose: the refusal past a table's budget,
//! the budget a sweep cadence cannot meet, and the clock offset.

use contextful_core::disclosure::declare::Binding;
use contextful_core::disclosure::reach::public_resources;
use contextful_core::disclosure::stale::{check_cadence, check_fresh, now_offset, posture, public_watermark, OffsetMalformed, Posture, NOW_OFFSET_VAR};
use contextful_core::disclosure::sweep::{Freshness, GrantRow, ResourceRow, SourceState, SweepBlock};
use std::collections::BTreeMap;
use contextful_core::disclosure::VisibilityError;
use contextful_core::store::declare::TableDecl;
use contextful_core::time::Instant;

fn at(s: &str) -> Instant {
    Instant::parse(s).unwrap()
}

fn binding(fidelity: &str, budget: &str) -> Binding {
    let manifest = format!(
        "[[pipeline.tables]]\nname = \"wiki/pages\"\n\n[pipeline.tables.visibility]\nsource = \"wiki\"\nresource_key = \"page_id\"\nfamily = \"item-exception\"\nfidelity = \"{fidelity}\"\nmax_acl_staleness = \"{budget}\"\n"
    );
    Binding::of(&TableDecl::parse_pipeline(&manifest).unwrap().remove(0)).unwrap().unwrap()
}

fn fresh(watermark: Option<&str>, full: Option<&str>) -> Freshness {
    Freshness {
        source: "wiki".into(),
        epoch: 2,
        sweep_id: "s2".into(),
        sweep_kind: "incremental".into(),
        watermark_at: watermark.map(at),
        last_full_sweep_at: full.map(at),
        last_incremental_at: watermark.map(at),
        budget_seconds: Some(900),
    }
}

fn stale(r: Result<(), VisibilityError>) -> VisibilityError {
    match r {
        Err(e @ VisibilityError::AccessStale { .. }) => e,
        other => panic!("{other:?}"),
    }
}

/// A read raises `VisibilityAccessStale` (HTTP 503, carrying source, lag, budget and last observation) when a touched table's source lag exceeds its budget, the source has no completed full run, or the public-status watermark is past budget.
#[test]
fn a_read_past_the_budget_or_before_a_full_run_is_refused_with_503() {
    let pages = binding("mirrored", "15m");
    let swept = fresh(Some("2030-01-01T00:00:00Z"), Some("2030-01-01T00:00:00Z"));
    // Inside the budget, and at it, the read proceeds.
    check_fresh(&pages, Some(&swept), at("2030-01-01T00:10:00Z")).unwrap();
    check_fresh(&pages, Some(&swept), at("2030-01-01T00:15:00Z")).unwrap();

    // One second past it, the read refuses carrying source, lag, budget and last observation.
    let e = stale(check_fresh(&pages, Some(&swept), at("2030-01-01T00:15:01Z")));
    assert_eq!((e.identifier(), e.status()), ("VisibilityAccessStale", Some(503)));
    let text = e.to_string();
    for part in ["`wiki`", "lag 901s", "budget 900s", "2030-01-01T00:00:00Z"] {
        assert!(text.contains(part), "{part}: {text}");
    }

    // No run at all, or runs with no completed full run, refuse.
    assert!(stale(check_fresh(&pages, None, at("2030-01-01T00:00:00Z"))).to_string().contains("no completed full run"));
    let never_full = fresh(Some("2030-01-01T00:00:00Z"), None);
    assert!(stale(check_fresh(&pages, Some(&never_full), at("2030-01-01T00:01:00Z"))).to_string().contains("no completed full run"));

    // A table served from no store carries no budget to exceed.
    check_fresh(&binding("excluded", "15m"), None, at("2030-01-01T00:00:00Z")).unwrap();
}

fn sweep(full: &str, incremental: &str) -> Vec<SweepBlock> {
    SweepBlock::parse_manifest(&format!("[[acl_sweep]]\nsource = \"wiki\"\nenumerate = \"pages\"\nfull = {{ every = \"{full}\" }}\nincremental = {{ every = \"{incremental}\" }}\n")).unwrap()
}

/// A budget tighter than the sweep cadence its source sustains raises `VisibilityBudgetUnreachable` at diagnose, naming both figures.
// spec: disclosure.bound-staleness.budget-below-cadence@005a5a13
#[test]
fn a_budget_tighter_than_the_sustained_cadence_is_refused_naming_both() {
    // The source sustains its tighter cadence: a 15m budget over a 5m incremental sweep holds.
    check_cadence(&[binding("mirrored", "15m")], &sweep("1h", "5m")).unwrap();
    check_cadence(&[binding("mirrored", "5m")], &sweep("1h", "5m")).unwrap();
    match check_cadence(&[binding("mirrored", "2m")], &sweep("1h", "5m")) {
        Err(e @ VisibilityError::BudgetUnreachable { .. }) => {
            assert_eq!(e.identifier(), "VisibilityBudgetUnreachable");
            let text = e.to_string();
            assert!(text.contains("120s") && text.contains("300s"), "{text}");
        }
        other => panic!("{other:?}"),
    }
    // A table served from no store carries no budget the cadence must meet.
    check_cadence(&[binding("excluded", "2m")], &sweep("1h", "5m")).unwrap();
}

/// `CONTEXTFUL_NOW_OFFSET_SECS`, a non-negative integer, moves the instant a staleness check reads as now that many seconds later, so it can refuse a read and never extends a budget.
// spec: disclosure.bound-staleness.clock-offset@3ebb32b2
#[test]
fn the_offset_moves_now_later_and_can_only_refuse() {
    assert_eq!(NOW_OFFSET_VAR, "CONTEXTFUL_NOW_OFFSET_SECS");
    assert_eq!(now_offset(None), Ok(0));
    assert_eq!(now_offset(Some("3600")), Ok(3600));
    let pages = binding("mirrored", "15m");
    let swept = fresh(Some("2030-01-01T00:00:00Z"), Some("2030-01-01T00:00:00Z"));
    let now = at("2030-01-01T00:01:00Z");
    check_fresh(&pages, Some(&swept), now.plus_secs(now_offset(None).unwrap())).unwrap();
    stale(check_fresh(&pages, Some(&swept), now.plus_secs(now_offset(Some("3600")).unwrap())));
    // A negative offset, which would extend the budget, does not parse.
    assert!(now_offset(Some("-3600")).is_err());
}

/// A `CONTEXTFUL_NOW_OFFSET_SECS` value other than a non-negative integer stops the process before it serves, naming the variable.
#[test]
fn a_malformed_offset_names_the_variable() {
    for bad in ["", "-1", "1h", "1.5", " 60", "99999999999999999999999"] {
        let e: OffsetMalformed = now_offset(Some(bad)).unwrap_err();
        assert!(e.to_string().contains("CONTEXTFUL_NOW_OFFSET_SECS"), "{bad}: {e}");
    }
}

fn resource(id: &str, observed: &str) -> ResourceRow {
    ResourceRow { resource_id: id.into(), resource_kind: "page".into(), visibility_class: "mirrored".into(), observed_at: at(observed), sweep_kind: "incremental".into(), sweep_id: "s2".into() }
}

fn grant(resource: &str, principal: &str, kind: &str, level: &str) -> GrantRow {
    GrantRow {
        resource_id: resource.into(),
        principal: principal.into(),
        principal_kind: kind.into(),
        level: level.into(),
        expires_at: None,
        observed_at: at("2030-01-01T00:00:00Z"),
        sweep_kind: "full".into(),
        sweep_id: "s1".into(),
    }
}

fn levels() -> BTreeMap<String, String> {
    BTreeMap::from([("anyone".to_string(), "read".to_string()), ("listed".to_string(), "list".to_string())])
}

/// p1 and p2 public, observed at the two instants given; p3 granted to a user alone; p4
/// public at a level conferring no read, observed at midnight.
fn public_state(p1: &str, p2: &str, freshness: Option<Freshness>) -> SourceState {
    SourceState {
        resources: vec![resource("p1", p1), resource("p2", p2), resource("p3", "2030-01-01T00:00:00Z"), resource("p4", "2030-01-01T00:00:00Z")],
        grants: vec![grant("p1", "anyone", "public", "anyone"), grant("p2", "anyone", "public", "anyone"), grant("p3", "dana", "user", "anyone"), grant("p4", "anyone", "public", "listed")],
        freshness,
        ..SourceState::default()
    }
}

/// A table's public-status watermark is the oldest `acl_observed_at` among its source's resources holding a read-conferring, unexpired `public` grant; with no such resource it lags zero.
// spec: disclosure.bound-staleness.public-watermark@f4cd0a7a
#[test]
fn the_public_status_watermark_is_the_oldest_observation_of_a_public_resource() {
    let now = at("2030-01-01T01:00:00Z");
    let levels = levels();
    let state = public_state("2030-01-01T00:50:00Z", "2030-01-01T00:40:00Z", None);
    // p3 holds no public grant and p4's public level confers no read, so neither ages it.
    assert_eq!(public_watermark(&state, Some(&levels), None, now), at("2030-01-01T00:40:00Z"));
    // An expired public grant leaves its resource out.
    let mut expired = state.clone();
    expired.grants[1].expires_at = Some(at("2030-01-01T00:59:00Z"));
    assert_eq!(public_watermark(&expired, Some(&levels), None, now), at("2030-01-01T00:50:00Z"));
    // With no public resource, the watermark is now.
    let private = SourceState { grants: vec![grant("p3", "dana", "user", "anyone")], ..state };
    assert_eq!(public_watermark(&private, Some(&levels), None, now), now);
}

fn public_only(budget: &str) -> Binding {
    let manifest = format!(
        "[[pipeline.tables]]\nname = \"wiki/pages\"\n\n[pipeline.tables.visibility]\nsource = \"wiki\"\nresource_key = \"page_id\"\nfamily = \"item-exception\"\nfidelity = \"mirrored\"\nmax_acl_staleness = \"{budget}\"\non_stale = \"public_only\"\n"
    );
    Binding::of(&TableDecl::parse_pipeline(&manifest).unwrap().remove(0)).unwrap().unwrap()
}

/// Past its budget, a `public_only` table whose source has a completed full run and a public-status watermark within budget serves only rows whose resource holds a read-conferring, unexpired `public` grant.
// spec: disclosure.bound-staleness.public-only@941ffc39
#[test]
fn a_public_only_table_past_its_budget_narrows_while_public_status_is_within_it() {
    let levels = levels();
    let pages = public_only("15m");
    let swept = Some(fresh(Some("2030-01-01T00:00:00Z"), Some("2030-01-01T00:00:00Z")));
    let state = public_state("2030-01-01T00:50:00Z", "2030-01-01T00:46:00Z", swept.clone());

    // Inside the budget the table reads whole.
    assert_eq!(posture(&pages, &state, Some(&levels), None, at("2030-01-01T00:10:00Z")), Ok(Posture::Fresh));
    // Past it, the public resources were observed 14 minutes ago: the read narrows.
    let now = at("2030-01-01T01:00:00Z");
    assert!(check_fresh(&pages, swept.as_ref(), now).is_err());
    assert_eq!(posture(&pages, &state, Some(&levels), None, now), Ok(Posture::Narrowed));
    // Narrowed, it serves p1 and p2: p3 holds a user grant alone and p4's public level
    // confers no read.
    let served = |s: &SourceState| public_resources(s, Some(&levels), None, now).into_keys().collect::<Vec<_>>();
    assert_eq!(served(&state), ["p1", "p2"]);
    // An expired public grant leaves its resource out.
    let mut lapsed = state.clone();
    lapsed.grants[1].expires_at = Some(at("2030-01-01T00:59:00Z"));
    assert_eq!(served(&lapsed), ["p1"]);

    // A public-status watermark past the budget refuses, naming it.
    let e = stale(posture(&pages, &state, Some(&levels), None, at("2030-01-01T01:01:01Z")).map(|_| ()));
    assert!(e.to_string().contains("public-status watermark") && e.to_string().contains("lag 901s"), "{e}");
    // A source with no completed full run refuses.
    let never_full = public_state("2030-01-01T00:50:00Z", "2030-01-01T00:46:00Z", Some(fresh(Some("2030-01-01T00:00:00Z"), None)));
    assert!(stale(posture(&pages, &never_full, Some(&levels), None, now).map(|_| ())).to_string().contains("no completed full run"));
    // A table declaring `refuse` refuses past its budget.
    assert!(posture(&binding("mirrored", "15m"), &state, Some(&levels), None, now).is_err());
}
