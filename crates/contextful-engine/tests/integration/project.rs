//! `run.project`: the in-process hub — emission, the terminal slot, the broadcast ring,
//! connect and restart.

use contextful_core::run::project::{Change, StepPatch, StepStatus};
use contextful_core::run::record::{Phase, RunRow, RunStatus};
use contextful_core::time::Instant;
use contextful_engine::project::{Hub, Update, BROADCAST_RING_ENTRIES};

fn at(s: &str) -> Instant {
    Instant::parse(s).unwrap()
}

/// `n` coalescing windows after the epoch, so every pump broadcasts.
fn tick(n: i128) -> Instant {
    Instant::from_unix_nanos(at("2030-01-01T00:00:00Z").unix_nanos() + n * 200_000_000).unwrap()
}

fn step(label: &str, st: StepStatus) -> Change {
    Change::Step(StepPatch::new(label).status(st))
}

fn status(st: RunStatus) -> Change {
    Change::Status { status: st, at: None, error: None }
}

fn row(run_id: &str, st: RunStatus) -> RunRow {
    RunRow {
        run_id: run_id.into(),
        pipeline_id: "filings-feed".into(),
        table: "filings".into(),
        site_id: "site-a".into(),
        status: st,
        owner: None,
        started_at: at("2030-01-01T00:00:00Z"),
        ended_at: Some(at("2030-01-01T00:00:09Z")),
        rows: 4,
        bytes: 900,
        batches: 3,
        skipped: 0,
        declined: Default::default(),
        error_kind: None,
        error_message: None,
        connector_id: "vendor".into(),
        connector_version: "1.0.0".into(),
        connector_hash: "ab".into(),
        trace_id: None,
        phase: Phase::Commit,
        execution_id: "exec-1".into(),
        stop: None,
        host_scope: None,
        input: None,
    }
}

/// Progress emission is no journal step: events reach an in-process observer over a bounded channel that drops when full and never blocks the runner.
// spec: run.project.emission-never-blocks@4c8fae25
#[test]
fn emission_into_a_full_channel_drops_and_returns() {
    let hub = Hub::with_capacity(tick(0), 4);
    let emitter = hub.emitter();
    // Nobody drains the channel; every emission past its capacity still returns at once.
    let started = std::time::Instant::now();
    for i in 0..1000 {
        emitter.emit("run-1", "filings-feed", step(&format!("pull-{i}"), StepStatus::Running));
    }
    assert!(started.elapsed() < std::time::Duration::from_secs(1));
    assert_eq!(hub.dropped(), 996);
    hub.pump(tick(1));
    let (snapshot, _) = hub.connect("run-1").unwrap();
    assert_eq!(snapshot.steps.len(), 4, "the observer received exactly what the channel held");
}

/// A terminal transition bypasses the lossy channel through a per-run slot that is never dropped; a projection reading non-terminal behind a terminal run record reconciles from the record.
// spec: run.project.terminal-slot@2f541514
#[test]
fn a_terminal_transition_survives_a_full_channel_and_a_lagging_projection_reconciles() {
    let hub = Hub::with_capacity(tick(0), 2);
    let emitter = hub.emitter();
    emitter.emit("run-1", "filings-feed", status(RunStatus::Running));
    emitter.emit("run-1", "filings-feed", step("pull-0", StepStatus::Running));
    emitter.emit("run-1", "filings-feed", step("pull-1", StepStatus::Running));
    assert_eq!(hub.dropped(), 1, "the channel is full");
    emitter.emit("run-1", "filings-feed", status(RunStatus::Success));
    assert_eq!(hub.dropped(), 1, "the terminal transition is not dropped");
    hub.pump(tick(1));
    assert_eq!(hub.connect("run-1").unwrap().0.status, RunStatus::Success);

    // A projection whose terminal event never arrived reads running until the record reconciles it.
    emitter.emit("run-2", "filings-feed", status(RunStatus::Running));
    hub.pump(tick(2));
    assert_eq!(hub.connect("run-2").unwrap().0.status, RunStatus::Running);
    hub.reconcile(&row("run-2", RunStatus::Failed), tick(3));
    let (snapshot, _) = hub.connect("run-2").unwrap();
    assert_eq!(snapshot.status, RunStatus::Failed);
    assert_eq!(snapshot.ended_at, Some(at("2030-01-01T00:00:09Z")));
}

/// The per-run broadcast holds 256 entries; a subscriber that overruns it resynchronizes to the latest snapshot.
// spec: run.project.broadcast-ring@9a980e7d
#[test]
fn a_subscriber_overrunning_the_ring_resynchronizes_to_the_latest() {
    assert_eq!(BROADCAST_RING_ENTRIES, 256);
    let hub = Hub::new(tick(0));
    let emitter = hub.emitter();
    emitter.emit("run-1", "filings-feed", status(RunStatus::Running));
    hub.pump(tick(1));
    let (_, mut slow) = hub.connect("run-1").unwrap();
    let (_, mut keeping_up) = hub.connect("run-1").unwrap();

    for i in 0..256 {
        emitter.emit("run-1", "filings-feed", step(&format!("pull-{i}"), StepStatus::Running));
        hub.pump(tick(2 + i));
        assert!(matches!(keeping_up.recv(), Some(Update::Snapshot(_))));
    }
    // 256 broadcasts fill the ring exactly: the slow subscriber still reads each in order.
    let Some(Update::Snapshot(first)) = slow.recv() else { panic!("expected the oldest entry") };
    assert_eq!(first.steps.len(), 1);

    let (_, mut overrun) = hub.connect("run-1").unwrap();
    for i in 256..(256 + 300) {
        emitter.emit("run-1", "filings-feed", step(&format!("pull-{i}"), StepStatus::Running));
        hub.pump(tick(2 + i));
    }
    match overrun.recv() {
        Some(Update::Resync(latest)) => assert_eq!(latest.steps.len(), 256 + 300),
        other => panic!("expected a resync, got {other:?}"),
    }
    assert!(overrun.recv().is_none(), "after the resync the subscriber is current");
}

/// A subscriber receives the folded snapshot and its update receiver under one lock, missing and duplicating no update.
// spec: run.project.connect@e8917fcd
#[test]
fn connect_hands_over_the_folded_snapshot_then_every_later_update_once() {
    let hub = Hub::new(tick(0));
    let emitter = hub.emitter();
    emitter.emit("run-1", "filings-feed", status(RunStatus::Running));
    emitter.emit("run-1", "filings-feed", step("pull-0", StepStatus::Running));
    hub.pump(tick(1));
    assert!(hub.connect("run-unseen").is_none());
    let (snapshot, mut sub) = hub.connect("run-1").unwrap();
    assert_eq!(snapshot.steps.len(), 1);
    assert!(sub.recv().is_none(), "the folded snapshot already holds every earlier update");

    emitter.emit("run-1", "filings-feed", step("pull-1", StepStatus::Running));
    hub.pump(tick(2));
    emitter.emit("run-1", "filings-feed", status(RunStatus::Success));
    hub.pump(tick(3));
    let mut seen = Vec::new();
    while let Some(Update::Snapshot(s)) = sub.recv() {
        assert!(s.version > snapshot.version);
        seen.push(s.version);
    }
    assert_eq!(seen.len(), 2);
    assert!(seen[0] < seen[1]);
}

/// A restart discards in-memory snapshots; a subscriber recovers history from the durable record.
// spec: run.project.restart-discards@23bc3c4b
#[test]
fn a_restarted_hub_holds_nothing_and_recovers_from_the_record() {
    let before = Hub::new(tick(0));
    before.emitter().emit("run-1", "filings-feed", status(RunStatus::Running));
    before.pump(tick(1));
    assert!(before.connect("run-1").is_some());
    let held = before.connect("run-1").unwrap().0.version;
    drop(before);

    let after = Hub::new(tick(10));
    assert!(after.connect("run-1").is_none(), "the restarted hub holds no snapshot");
    let (recovered, _) = after.connect_or_recover(&row("run-1", RunStatus::Success));
    assert_eq!(recovered.status, RunStatus::Success);
    assert_eq!(recovered.started_at, Some(at("2030-01-01T00:00:00Z")));
    assert!(recovered.version > held, "the new epoch sorts above every version the old hub issued");
}
