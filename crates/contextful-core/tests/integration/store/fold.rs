//! `store.fold`: when a scheduled pass fires.

use super::{at, run, snapshot};
use contextful_core::store::fold::{due, scheduled, COMPACTION_INTERVAL_SECS, COMPACTION_RUN_COUNT};
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
    assert!(!due(&state(49, later, Some(early)), t0.plus_secs(60)));
    assert!(due(&state(50, later, Some(early)), t0.plus_secs(60)));
    assert!(!due(&state(1, later, Some(early)), t0.plus_secs(6 * 3600 - 1)));
    assert!(due(&state(1, later, Some(early)), t0.plus_secs(6 * 3600)));
    // Nothing to fold never fires.
    assert!(!due(&state(0, later, Some(early)), t0.plus_secs(7 * 3600)));
}

/// A table never folded measures the interval from its oldest unfolded run, so a
/// scheduled pass reaches it 6 h after that run committed.
#[test]
fn a_never_folded_table_is_due_six_hours_after_its_oldest_run() {
    let t0 = at("2030-01-01T00:00:00Z");
    assert!(!due(&state(1, "2030-01-01T00:00:00Z", None), t0.plus_secs(6 * 3600 - 1)));
    assert!(due(&state(1, "2030-01-01T00:00:00Z", None), t0.plus_secs(6 * 3600)));
}

/// A scheduled pass visits a table whose trigger fired and one with nothing to fold, which
/// reports nothing-landed and still collects what retention allows; it skips the rest.
#[test]
fn a_scheduled_pass_visits_due_and_idle_tables() {
    let t0 = at("2030-01-01T00:00:00Z");
    assert!(scheduled(&state(0, "2030-01-01T00:00:00Z", Some("2030-01-01T00:00:00Z")), t0.plus_secs(60)));
    assert!(!scheduled(&state(1, "2030-01-01T00:00:00Z", None), t0.plus_secs(60)));
    assert!(scheduled(&state(1, "2030-01-01T00:00:00Z", None), t0.plus_secs(7 * 3600)));
}
