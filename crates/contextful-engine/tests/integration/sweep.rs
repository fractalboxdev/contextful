//! `run.sweep`: a store-driven job run once per origin under one frozen `as_of`.

use crate::support::{Rig, T0};
use contextful_core::run::drive::{Emitted, InputRow, InputSet, RowBody, RowCalls, RowStop, StoreInput};
use contextful_core::run::ports::{Landed, Row};
use contextful_core::run::record::RunStatus;
use contextful_core::run::retry::Schedule;
use contextful_core::run::sweep::{Sweep, SweepState};
use contextful_core::run::{Failure, FailureTag};
use contextful_core::time::Instant;
use contextful_engine::drive::sweep::{SweepFire, Swept};
use contextful_engine::drive::Drive;
use serde_json::json;
use std::collections::{BTreeMap, BTreeSet};
use std::panic::AssertUnwindSafe;
use std::sync::Mutex;
use std::time::Duration;

/// Twelve daily origins, the last a month before the rig's clock.
fn twelve(state: SweepState) -> Sweep {
    Sweep { start: "2029-11-20T00:00:00Z".into(), end: "2029-12-01T00:00:00Z".into(), step: "1d".into(), state }
}

fn input(as_of: Option<&str>) -> StoreInput {
    StoreInput { body: "echo".into(), statement: "SELECT doc_id FROM documents".into(), as_of: as_of.map(str::to_string), sweep: None, structural_identity: None }
}

fn fire<'a>(body: &'a dyn RowBody, sweep: Sweep, as_of: Option<&str>, run_id: &str) -> SweepFire<'a> {
    let drive = Drive {
        job: "forecast".into(),
        input: input(as_of),
        body,
        max_in_flight: 1,
        run_id: run_id.into(),
        site_id: "site-a".into(),
        pid: 4242,
        boot_id: "boot-a".into(),
        schedule: Schedule::single_attempt(),
        poll: Duration::from_millis(10),
    };
    SweepFire { drive, sweep }
}

/// Emits one `notes` row per input row; fails terminally on the origin it names and dies with
/// the process on another.
#[derive(Default)]
struct Echo {
    fails_at: Option<String>,
    dies_at: Option<String>,
}

impl RowBody for Echo {
    fn run(&self, row: &InputRow, calls: &dyn RowCalls) -> Result<Emitted, RowStop> {
        let origin = row.row["origin"].as_str().unwrap().to_string();
        if self.fails_at.as_deref() == Some(origin.as_str()) {
            return Err(RowStop::Failed(Failure::deterministic(FailureTag::Permanent, format!("origin {origin} is malformed"))));
        }
        let note = calls.call("model", origin.as_bytes(), &mut |_| {
            if self.dies_at.as_deref() == Some(origin.as_str()) {
                panic!("the process dies inside origin {origin}");
            }
            Ok(origin.clone().into_bytes())
        })?;
        Ok(BTreeMap::from([("notes".to_string(), vec![json!({ "origin": String::from_utf8(note).unwrap() }).as_object().cloned().unwrap()])]))
    }
}

/// One read the sweep asked for: the origin, the `as_of`, and the carried commit instant.
type Asked = (String, String, Option<Instant>);

fn run(rig: &Rig, f: &SweepFire<'_>, asked: &Mutex<Vec<Asked>>) -> Swept {
    let mut read = |input: &StoreInput, as_of: &str, carried: Option<Instant>| -> Result<InputSet, Failure> {
        let origin = input.sweep.as_ref().unwrap().origin.clone();
        asked.lock().unwrap().push((origin.clone(), as_of.to_string(), carried));
        let row: Row = json!({ "origin": origin }).as_object().cloned().unwrap();
        Ok(InputSet { as_of: as_of.into(), snapshots: BTreeMap::new(), rows: vec![row] })
    };
    let mut land = |_: &StoreInput, _: &str, e: &Emitted| -> Result<Landed, Failure> { Ok(Landed { rows: e.values().map(|r| r.len() as u64).sum(), bytes: 0 }) };
    rig.engine.sweep(f, &mut read, &mut land).unwrap()
}

/// A store-driven `[[job]]` declaring `sweep = { start, end, step, state }` opens one run per origin from `start` through `end` at each `step`, a `<n><unit>` interval in {{surface.arm.grammar}} units, each reading with `available_as_of` equal to its origin.
// spec: run.sweep.origins@ee93e0d3
#[test]
fn a_sweep_opens_one_run_per_origin_each_bounded_by_its_origin() {
    let rig = Rig::new();
    let body = Echo::default();
    let asked = Mutex::new(Vec::new());
    let swept = run(&rig, &fire(&body, twelve(SweepState::Fresh), None, "sweep-1"), &asked);
    assert_eq!(swept.sweep.status, RunStatus::Success, "{:?}", swept.sweep);
    assert_eq!(swept.origins.len(), 12);
    let origins: Vec<String> = asked.lock().unwrap().iter().map(|a| a.0.clone()).collect();
    assert_eq!(origins.first().unwrap(), "2029-11-20T00:00:00.000000000Z");
    assert_eq!(origins.last().unwrap(), "2029-12-01T00:00:00.000000000Z");
    for row in &swept.origins {
        assert_eq!(row.status, RunStatus::Success, "{row:?}");
        let bounds = row.input.clone().unwrap();
        assert_eq!(bounds.available_as_of, bounds.origin, "each origin reads at `available_as_of` equal to itself");
    }
}

/// A sweep's id is the execution id of the fire that opens it; every origin's run records it, so run history groups one sweep's origins.
// spec: run.sweep.sweep-id@8398749b
#[test]
fn every_origin_records_the_sweeps_execution_id() {
    let rig = Rig::new();
    let body = Echo::default();
    let swept = run(&rig, &fire(&body, twelve(SweepState::Fresh), None, "sweep-1"), &Mutex::default());
    assert!(!swept.origins.is_empty(), "the check below ranges over no origin");
    assert!(swept.origins.iter().all(|r| r.input.as_ref().unwrap().sweep_id.as_deref() == Some(swept.sweep.execution_id.as_str())));
    assert_eq!(swept.sweep.host_scope.as_deref(), Some("job:forecast"));
}

/// Every origin of one sweep reads at one `as_of`: the job's declared `as_of`, else the instant the sweep opens, recorded once as {{run.journal.open-as-of}} records it for resumes.
// spec: run.sweep.frozen-as-of@41957ca8
#[test]
fn every_origin_reads_one_frozen_as_of_across_a_resume() {
    let rig = Rig::new();
    let dying = Echo { dies_at: Some("2029-11-24T00:00:00.000000000Z".into()), ..Echo::default() };
    let asked = Mutex::new(Vec::new());
    let died = std::panic::catch_unwind(AssertUnwindSafe(|| run(&rig, &fire(&dying, twelve(SweepState::Fresh), None, "sweep-1"), &asked)));
    assert!(died.is_err());
    assert_eq!(asked.lock().unwrap().len(), 5, "origins 1 through 5 read before the fifth died");

    rig.clock.advance(86_400);
    let body = Echo::default();
    let resumed = Mutex::new(Vec::new());
    let swept = run(&rig, &fire(&body, twelve(SweepState::Fresh), None, "sweep-2"), &resumed);
    assert_eq!(swept.sweep.status, RunStatus::Success, "{:?}", swept.sweep);
    assert_eq!(swept.origins.len(), 12, "recorded origins answer from the journal");
    let resumed = resumed.lock().unwrap();
    assert_eq!(resumed.len(), 7, "four recorded origins ran no read; the fifth re-entered and six more ran");
    assert!(resumed.iter().all(|a| a.1 == T0), "the resume reads at the instant the sweep opened: {resumed:?}");
    assert!(swept.origins.iter().all(|r| r.input.as_ref().unwrap().as_of == T0));

    // A declared `as_of` is the frozen instant.
    let rig = Rig::new();
    let asked = Mutex::new(Vec::new());
    run(&rig, &fire(&body, twelve(SweepState::Fresh), Some("2029-12-15T00:00:00Z"), "sweep-1"), &asked);
    assert!(asked.lock().unwrap().iter().all(|a| a.1 == "2029-12-15T00:00:00Z"));
}

/// A sweep whose `end` is later than its `as_of` raises `RunSweepOriginAfterOpen`, at validation for a declared `as_of` and at sweep open otherwise, and opens no origin.
// spec: run.sweep.origin-after-open@7157ef96
#[test]
fn a_sweep_ending_after_its_open_instant_opens_no_origin() {
    let rig = Rig::new();
    let body = Echo::default();
    let late = Sweep { end: "2030-02-01T00:00:00Z".into(), ..twelve(SweepState::Fresh) };
    let asked = Mutex::new(Vec::new());
    let swept = run(&rig, &fire(&body, late.clone(), None, "sweep-1"), &asked);
    assert_eq!(swept.sweep.status, RunStatus::Failed);
    assert!(swept.sweep.error_message.as_deref().unwrap().contains("RunSweepOriginAfterOpen"), "{:?}", swept.sweep);
    assert!(swept.origins.is_empty() && asked.lock().unwrap().is_empty());

    let declared = "[[job]]\nname = \"forecast\"\nkind = \"store-driven\"\nbody = \"echo\"\nstatement = \"SELECT 1\"\nas_of = \"2030-01-01T00:00:00Z\"\ntables = [\"notes\"]\nmax_in_flight = 1\nsweep = { start = \"2029-12-01T00:00:00Z\", end = \"2030-02-01T00:00:00Z\", step = \"1d\" }\n";
    let refused = contextful_core::job::parse_jobs(declared, &|_| true).unwrap_err();
    assert!(refused.to_string().starts_with("RunSweepOriginAfterOpen"), "{refused}");
    assert!(contextful_core::job::parse_jobs(&declared.replace("2030-02-01", "2029-12-31"), &|_| true).is_ok());
}

/// Under `state = "carried"`, origins run in ascending order one at a time, each reading the job's output `tables` at the commit of the preceding origin and every other table at the sweep's `as_of`.
// spec: run.sweep.carried-state@3f9438c2
#[test]
fn a_carried_origin_reads_outputs_at_the_preceding_commit() {
    let rig = Rig::new();
    let body = Echo::default();
    let asked = Mutex::new(Vec::new());
    let swept = run(&rig, &fire(&body, twelve(SweepState::Carried), None, "sweep-1"), &asked);
    let asked = asked.lock().unwrap();
    let origins: Vec<&String> = asked.iter().map(|a| &a.0).collect();
    let mut sorted = origins.clone();
    sorted.sort();
    assert_eq!(origins, sorted, "origins run in ascending order");
    assert_eq!(asked[0].2, None, "the first origin has no predecessor");
    for (k, a) in asked.iter().enumerate().skip(1) {
        assert_eq!(a.2, swept.origins[k - 1].ended_at, "origin {k} reads outputs at its predecessor's commit");
        assert_eq!(a.1, T0, "every other table reads at the sweep's `as_of`");
    }
}

/// Under `state = "fresh"`, the default, every table an origin reads, its output tables included, resolves at the sweep's `as_of`, so no origin reads a row another origin wrote.
// spec: run.sweep.fresh-state@8b358cee
#[test]
fn a_fresh_origin_reads_every_table_at_the_sweeps_as_of() {
    assert_eq!(Sweep { start: T0.into(), end: T0.into(), step: "1d".into(), state: Default::default() }.state, SweepState::Fresh);
    let declared = "[[job]]\nname = \"forecast\"\nkind = \"store-driven\"\nbody = \"echo\"\nstatement = \"SELECT 1\"\ntables = [\"notes\"]\nmax_in_flight = 1\nsweep = { start = \"2029-11-20T00:00:00Z\", end = \"2029-12-01T00:00:00Z\", step = \"1d\" }\n";
    let job = contextful_core::job::parse_jobs(declared, &|_| true).unwrap().remove(0);
    let contextful_core::job::JobKind::StoreDriven(driven) = job.kind else { panic!("a store-driven job") };
    let parsed = *driven.sweep.unwrap();
    assert_eq!(parsed.state, SweepState::Fresh, "`state` defaults to fresh");
    let rig = Rig::new();
    let body = Echo::default();
    let asked = Mutex::new(Vec::new());
    run(&rig, &fire(&body, parsed, None, "sweep-1"), &asked);
    let asked = asked.lock().unwrap();
    assert_eq!(asked.len(), 12);
    assert!(asked.iter().all(|a| a.1 == T0 && a.2.is_none()), "{asked:?}");
}

/// A sweep runs every origin it declares, however many have passed, and {{surface.arm.catch-up}} collapses none; a failed origin of a `carried` sweep leaves every later origin unopened.
// spec: run.sweep.no-collapse@bfbf849d
#[test]
fn every_origin_runs_and_a_failed_carried_origin_stops_the_rest() {
    let failing = Echo { fails_at: Some("2029-11-23T00:00:00.000000000Z".into()), ..Echo::default() };

    let rig = Rig::new();
    let asked = Mutex::new(Vec::new());
    let carried = run(&rig, &fire(&failing, twelve(SweepState::Carried), None, "sweep-1"), &asked);
    assert_eq!(carried.sweep.status, RunStatus::Failed);
    assert_eq!(carried.origins.len(), 4, "the fourth origin failed and no later origin opened");
    assert_eq!(carried.origins[3].status, RunStatus::Failed);

    let rig = Rig::new();
    let fresh = run(&rig, &fire(&failing, twelve(SweepState::Fresh), None, "sweep-1"), &Mutex::default());
    assert_eq!(fresh.sweep.status, RunStatus::Failed, "the sweep reports its failed origin");
    assert_eq!(fresh.origins.len(), 12, "a fresh sweep still runs every origin");
    assert_eq!(fresh.origins.iter().filter(|r| r.status == RunStatus::Success).count(), 11);
}

/// No recorded or cached value crosses origins: each origin runs under its own execution id, which keys {{run.journal.entry-key}}, and carries its origin as `available_as_of` among the bounds {{read.cache.result-key}} keys on.
// spec: run.sweep.cache-key@427ef791
#[test]
fn each_origin_runs_under_its_own_execution_id_and_bound() {
    let rig = Rig::new();
    let body = Echo::default();
    let swept = run(&rig, &fire(&body, twelve(SweepState::Fresh), None, "sweep-1"), &Mutex::default());
    let executions: BTreeSet<&str> = swept.origins.iter().map(|r| r.execution_id.as_str()).collect();
    assert_eq!(executions.len(), 12);
    assert!(!executions.contains(swept.sweep.execution_id.as_str()));
    let bounds: BTreeSet<Option<String>> = swept.origins.iter().map(|r| r.input.as_ref().unwrap().available_as_of.clone()).collect();
    assert_eq!(bounds.len(), 12, "each origin carries its own availability bound");
}

/// A sweep origin's run row carries, beside {{run.record.input-bounds}}, its `available_as_of`, any `valid_as_of` it read under, the sweep id and the origin.
// spec: run.record.sweep-bounds@d3edfb11
#[test]
fn an_origin_row_names_its_bounds_sweep_and_origin() {
    let rig = Rig::new();
    let body = Echo::default();
    let swept = run(&rig, &fire(&body, twelve(SweepState::Fresh), None, "sweep-1"), &Mutex::default());
    let first = rig.row(&swept.origins[0].run_id).input.unwrap();
    assert_eq!(first.as_of, T0);
    assert_eq!(first.available_as_of.as_deref(), Some("2029-11-20T00:00:00.000000000Z"));
    assert_eq!(first.origin.as_deref(), Some("2029-11-20T00:00:00.000000000Z"));
    assert_eq!(first.sweep_id.as_deref(), Some(swept.sweep.execution_id.as_str()));
    assert_eq!(first.valid_as_of, None);
    assert_eq!(first.rows, 1);
    let encoded = serde_json::to_value(&first).unwrap();
    assert_eq!(encoded["sweep_id"], json!(swept.sweep.execution_id));
    // A run outside a sweep carries none of the four.
    let plain = serde_json::to_value(contextful_core::run::record::InputBounds { as_of: T0.into(), ..Default::default() }).unwrap();
    assert!(plain.get("available_as_of").is_none() && plain.get("sweep_id").is_none() && plain.get("origin").is_none());
}

/// An origin's plan reference hashes the sweep declaration and its origin beside the fields {{run.journal.input-pin}} hashes, so a resume under a changed sweep meets {{run.own.pinned-plan-changed}}.
// spec: run.sweep.origin-pin@e0358fe2
#[test]
fn a_resume_under_a_changed_sweep_meets_a_pin_mismatch() {
    let rig = Rig::new();
    let dying = Echo { dies_at: Some("2029-11-22T00:00:00.000000000Z".into()), ..Echo::default() };
    let died = std::panic::catch_unwind(AssertUnwindSafe(|| run(&rig, &fire(&dying, twelve(SweepState::Fresh), None, "sweep-1"), &Mutex::default())));
    assert!(died.is_err());
    rig.clock.advance(86_400);
    let body = Echo::default();
    let changed = Sweep { step: "2d".into(), ..twelve(SweepState::Fresh) };
    let mut read = |_: &StoreInput, _: &str, _: Option<Instant>| -> Result<InputSet, Failure> { panic!("a refused resume reads nothing") };
    let mut land = |_: &StoreInput, _: &str, _: &Emitted| -> Result<Landed, Failure> { unreachable!() };
    let refused = rig.engine.sweep(&fire(&body, changed, None, "sweep-2"), &mut read, &mut land).unwrap_err();
    assert!(refused.to_string().contains("ExecutionPinMismatch"), "{refused}");

    // The origin's own reference moves with the declaration and the origin.
    let pin = |declaration: &str, origin: &str| StoreInput {
        sweep: Some(contextful_core::run::sweep::OriginPin { declaration: declaration.into(), sweep_id: "s".into(), origin: origin.into() }),
        ..input(None)
    };
    let base = pin("a", "2029-11-20");
    assert_ne!(base.plan_ref(), input(None).plan_ref());
    assert_ne!(base.plan_ref(), pin("b", "2029-11-20").plan_ref());
    assert_ne!(base.plan_ref(), pin("a", "2029-11-21").plan_ref());
    assert_eq!(base.plan_ref(), StoreInput { sweep: Some(contextful_core::run::sweep::OriginPin { sweep_id: "other".into(), ..base.sweep.clone().unwrap() }), ..base.clone() }.plan_ref(), "the sweep id is no pin");
}

#[test]
fn a_resume_under_a_changed_structural_identity_refuses_before_any_origin_reads() {
    let rig = Rig::new();
    let pinned = |body: &'static Echo, identity: Option<&str>, run_id: &str| {
        let mut f = fire(body, twelve(SweepState::Fresh), None, run_id);
        f.drive.input.structural_identity = identity.map(str::to_string);
        f
    };
    let dying: &'static Echo = Box::leak(Box::new(Echo { dies_at: Some("2029-11-22T00:00:00.000000000Z".into()), ..Echo::default() }));
    let carried = Mutex::new(Vec::new());
    let died = std::panic::catch_unwind(AssertUnwindSafe(|| {
        let mut read = |input: &StoreInput, as_of: &str, _: Option<Instant>| -> Result<InputSet, Failure> {
            carried.lock().unwrap().push(input.structural_identity.clone());
            let row: Row = json!({ "origin": input.sweep.as_ref().unwrap().origin }).as_object().cloned().unwrap();
            Ok(InputSet { as_of: as_of.into(), snapshots: BTreeMap::new(), rows: vec![row] })
        };
        let mut land = |_: &StoreInput, _: &str, e: &Emitted| -> Result<Landed, Failure> { Ok(Landed { rows: e.values().map(|r| r.len() as u64).sum(), bytes: 0 }) };
        rig.engine.sweep(&pinned(dying, Some("structure-a"), "sweep-1"), &mut read, &mut land)
    }));
    assert!(died.is_err());
    assert!(!carried.lock().unwrap().is_empty() && carried.lock().unwrap().iter().all(|i| i.as_deref() == Some("structure-a")), "every origin carries the pinned structure");
    rig.clock.advance(86_400);
    let body: &'static Echo = Box::leak(Box::default());
    for (i, identity) in [Some("structure-b"), None].into_iter().enumerate() {
        let mut read = |_: &StoreInput, _: &str, _: Option<Instant>| -> Result<InputSet, Failure> { panic!("a refused resume reads nothing") };
        let mut land = |_: &StoreInput, _: &str, _: &Emitted| -> Result<Landed, Failure> { unreachable!() };
        let refused = rig.engine.sweep(&pinned(body, identity, &format!("sweep-refused-{i}")), &mut read, &mut land).unwrap_err();
        assert!(refused.to_string().contains("ExecutionPinMismatch"), "{refused}");
    }
    let resumed = run(&rig, &pinned(body, Some("structure-a"), "sweep-resumed"), &Mutex::default());
    assert_eq!(resumed.sweep.status, RunStatus::Success);
}
