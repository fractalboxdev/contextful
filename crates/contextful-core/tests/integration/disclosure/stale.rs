//! `disclosure.bound-staleness` at read and at diagnose: the refusal past a table's budget,
//! the budget a sweep cadence cannot meet, and the clock offset.

use contextful_core::disclosure::declare::Binding;
use contextful_core::disclosure::stale::{check_cadence, check_fresh, now_offset, OffsetMalformed, NOW_OFFSET_VAR};
use contextful_core::disclosure::sweep::{Freshness, SweepBlock};
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
