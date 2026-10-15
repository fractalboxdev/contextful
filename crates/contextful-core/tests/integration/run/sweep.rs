//! `run.sweep`'s declaration: origins over the schedule grammar's units, and the
//! availability instant an origin's write carries.

use contextful_core::run::sweep::{stamp_origin, Sweep, SweepState};
use contextful_core::store::declare::TableDecl;
use serde_json::json;

fn sweep(start: &str, end: &str, step: &str) -> Sweep {
    Sweep { start: start.into(), end: end.into(), step: step.into(), state: SweepState::Fresh }
}

#[test]
fn origins_step_from_start_through_end_in_schedule_units() {
    let hourly = sweep("2030-01-01T00:00:00Z", "2030-01-01T03:30:00Z", "1h").origins().unwrap();
    let spelled: Vec<String> = hourly.iter().map(|o| o.to_rfc3339_nanos()).collect();
    assert_eq!(spelled, ["2030-01-01T00:00:00.000000000Z", "2030-01-01T01:00:00.000000000Z", "2030-01-01T02:00:00.000000000Z", "2030-01-01T03:00:00.000000000Z"]);
    assert_eq!(sweep("2030-01-01T00:00:00Z", "2030-01-01T00:00:00Z", "30s").origins().unwrap().len(), 1);
    assert!(sweep("2030-01-01T00:00:00Z", "2030-01-02T00:00:00Z", "1w").origins().is_err(), "a week is no grammar unit");
    assert!(sweep("2030-01-01T00:00:00Z", "2030-01-02T00:00:00Z", "0d").origins().is_err());
    assert!(sweep("2030-01-02T00:00:00Z", "2030-01-01T00:00:00Z", "1d").origins().is_err(), "`end` precedes `start`");
}

/// A row an origin lands into a table declaring `available_time` carries the origin as its availability instant, so a later origin reads it and an earlier origin never does.
// spec: run.sweep.simulated-write@f040d074
#[test]
fn an_origins_write_carries_the_origin_as_its_availability_instant() {
    let notes = TableDecl::parse_pipeline("[[pipeline.tables]]\nname = \"notes\"\n[pipeline.tables.available_time]\ncolumn = \"written_at\"\n").unwrap().remove(0);
    let mut rows = vec![json!({ "note": "buy", "written_at": "2031-06-01T00:00:00Z" }).as_object().cloned().unwrap(), json!({ "note": "hold" }).as_object().cloned().unwrap()];
    stamp_origin(&notes, &mut rows, "2014-03-01T00:00:00.000000000Z");
    assert!(!rows.is_empty(), "the check below ranges over no row");
    assert!(rows.iter().all(|r| r["written_at"] == json!("2014-03-01T00:00:00.000000000Z")), "{rows:?}");

    // A table declaring no availability time lands its rows untouched.
    let mut plain = vec![json!({ "note": "sell" }).as_object().cloned().unwrap()];
    stamp_origin(&TableDecl::named("scratch"), &mut plain, "2014-03-01T00:00:00.000000000Z");
    assert_eq!(plain[0], json!({ "note": "sell" }).as_object().cloned().unwrap());
}
