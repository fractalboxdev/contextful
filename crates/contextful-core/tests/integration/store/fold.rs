//! `store.fold`: when a scheduled pass fires.

use super::{at, run, snapshot};
use contextful_core::store::declare::TableDecl;
use contextful_core::store::fold::{due, scheduled, trigger_fired, COMPACTION_INTERVAL_SECS, COMPACTION_RUN_COUNT};
use contextful_core::store::resolve::TableState;

fn state(runs: usize, committed: &str, previous_pass: Option<&str>) -> TableState {
    let chain: Vec<_> = previous_pass.map(|p| snapshot(p, None, &[])).into_iter().collect();
    TableState {
        table: "filings".into(),
        runs: (0..runs).map(|i| run(&format!("run-{i}"), committed, 1)).collect(),
        chain,
        ..TableState::default()
    }
}

/// A pass fires at 50 runs committed on a table, 6 h after the table's previous pass, or on `contextful context compact <table>`.
// spec: store.fold.triggers@15661a71
#[test]
fn a_pass_fires_at_fifty_runs_or_six_hours() {
    let t0 = at("2030-01-01T00:00:00Z");
    let (early, later) = ("2030-01-01T00:00:00Z", "2030-01-01T00:00:01Z");
    assert_eq!(COMPACTION_RUN_COUNT, 50);
    assert_eq!(COMPACTION_INTERVAL_SECS, 6 * 3600);
    assert!(!due(&state(49, later, Some(early)), &TableDecl::named("filings"), t0.plus_secs(60)));
    assert!(due(&state(50, later, Some(early)), &TableDecl::named("filings"), t0.plus_secs(60)));
    assert!(!due(&state(1, later, Some(early)), &TableDecl::named("filings"), t0.plus_secs(6 * 3600 - 1)));
    assert!(due(&state(1, later, Some(early)), &TableDecl::named("filings"), t0.plus_secs(6 * 3600)));
    // Nothing to fold never fires.
    assert!(!due(&state(0, later, Some(early)), &TableDecl::named("filings"), t0.plus_secs(7 * 3600)));
}

/// A table never folded measures the interval from its oldest unfolded run, so a
/// scheduled pass reaches it 6 h after that run committed.
#[test]
fn a_never_folded_table_is_due_six_hours_after_its_oldest_run() {
    let t0 = at("2030-01-01T00:00:00Z");
    assert!(!due(&state(1, "2030-01-01T00:00:00Z", None), &TableDecl::named("filings"), t0.plus_secs(6 * 3600 - 1)));
    assert!(due(&state(1, "2030-01-01T00:00:00Z", None), &TableDecl::named("filings"), t0.plus_secs(6 * 3600)));
}

/// A scheduled pass visits a table whose trigger fired and one with nothing to fold, which
/// reports nothing-landed and still collects what retention allows; it skips the rest.
#[test]
fn a_scheduled_pass_visits_due_and_idle_tables() {
    let t0 = at("2030-01-01T00:00:00Z");
    assert!(scheduled(&state(0, "2030-01-01T00:00:00Z", Some("2030-01-01T00:00:00Z")), &TableDecl::named("filings"), t0.plus_secs(60)));
    assert!(!scheduled(&state(1, "2030-01-01T00:00:00Z", None), &TableDecl::named("filings"), t0.plus_secs(60)));
    assert!(scheduled(&state(1, "2030-01-01T00:00:00Z", None), &TableDecl::named("filings"), t0.plus_secs(7 * 3600)));
}

/// A declared run count replaces the 50-run count in `due`; parsing refuses zero.
#[test]
fn a_declared_run_count_replaces_the_fifty_run_trigger() {
    let t0 = at("2030-01-01T00:00:00Z");
    let (early, later) = ("2030-01-01T00:00:00Z", "2030-01-01T00:00:01Z");
    let declared = TableDecl::parse_pipeline("[[pipeline.tables]]\nname = \"events\"\nfold_max_unfolded_runs = 200\n").unwrap().remove(0);
    assert_eq!(declared.fold_run_count(), 200);
    assert_eq!(TableDecl::named("events").fold_run_count(), COMPACTION_RUN_COUNT);
    let mut s = state(199, later, Some(early));
    assert!(!due(&s, &declared, t0.plus_secs(60)));
    s.runs.push(super::run("run-199", later, 1));
    assert!(due(&s, &declared, t0.plus_secs(60)));
    assert!(!due(&state(50, later, Some(early)), &declared, t0.plus_secs(60)), "a declared count above 50 holds a pass back at 50 runs");
    for invalid in ["0", "-1"] {
        let text = format!("[[pipeline.tables]]\nname = \"events\"\nfold_max_unfolded_runs = {invalid}\n");
        assert!(TableDecl::parse_pipeline(&text).is_err(), "{invalid}");
    }
}

/// The trigger reads from an unfolded count and the instant the interval runs from, so a
/// writer counting its own commits decides a pass without reading the table's manifests.
#[test]
fn the_trigger_decides_from_a_count_and_an_instant() {
    let t0 = at("2030-01-01T00:00:00Z");
    assert!(!trigger_fired(0, Some(t0), t0.plus_secs(7 * 3600), 5));
    assert!(!trigger_fired(4, Some(t0), t0.plus_secs(60), 5));
    assert!(trigger_fired(5, Some(t0), t0.plus_secs(60), 5));
    assert!(trigger_fired(1, Some(t0), t0.plus_secs(COMPACTION_INTERVAL_SECS), 5));
}

/// A part's size tier is the floor of the base-4 logarithm of its rows, so four parts of one
/// tier merge into a part of the next.
#[test]
fn a_size_tier_spans_a_fourfold_range_of_rows() {
    use contextful_core::store::fold::{size_tier, FOLD_MERGE_FANOUT};
    assert_eq!(FOLD_MERGE_FANOUT, 4);
    let tiers: Vec<u32> = [0, 1, 3, 4, 15, 16, 63, 64, 1_000_000, u64::MAX].into_iter().map(size_tier).collect();
    assert_eq!(tiers, [0, 0, 0, 1, 1, 2, 2, 3, 9, 31]);
}
