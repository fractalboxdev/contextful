//! `store.fold`: when a scheduled pass fires.

use super::at;
use contextful_core::store::fold::{due, COMPACTION_INTERVAL_SECS, COMPACTION_RUN_COUNT};

/// A pass fires at 50 runs committed on a table, 6 h after the table's previous pass, or on `contextful context compact <table>`.
// spec: store.fold.triggers@15661a71
#[test]
fn a_pass_fires_at_fifty_runs_or_six_hours() {
    let t0 = at("2030-01-01T00:00:00Z");
    assert_eq!(COMPACTION_RUN_COUNT, 50);
    assert_eq!(COMPACTION_INTERVAL_SECS, 6 * 3600);
    assert!(!due(49, Some(t0), t0.plus_secs(60)));
    assert!(due(50, Some(t0), t0.plus_secs(60)));
    assert!(!due(1, Some(t0), t0.plus_secs(6 * 3600 - 1)));
    assert!(due(1, Some(t0), t0.plus_secs(6 * 3600)));
    // Nothing to fold never fires.
    assert!(!due(0, Some(t0), t0.plus_secs(7 * 3600)));
}
