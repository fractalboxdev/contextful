//! `surface.arm` and `surface.dispatch`: the scheduler over the catalog's run history and cadence lease.

use crate::support::{at, Rig, T0};
use contextful_core::coordinate::{Catalog, LeaseKey, CADENCE_LEASE_RENEWAL_SECS, CADENCE_LEASE_TTL_SECS};
use contextful_core::run::record::RunStatus;
use contextful_core::surface::arm::Schedule;
use contextful_engine::scheduler::{Dispatch, Entry, LeaseState, Scheduler};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// A dispatch that journals a run row per fire, starting at the catalog's now, and
/// optionally holds at a barrier: it announces each journaled fire on `entered`, then
/// waits for a release before it returns.
struct Recording {
    catalog: Arc<dyn Catalog + Send + Sync>,
    fired: Mutex<Vec<(String, u64)>>,
    gate: Option<Barrier>,
}

/// The two halves of a dispatch barrier: the unit reports its id once its run row is
/// journaled, then blocks until the test releases it.
struct Barrier {
    entered: Mutex<Sender<String>>,
    release: Mutex<Receiver<()>>,
}

/// The test's side of a [`Barrier`].
struct Gate {
    entered: Receiver<String>,
    release: Sender<()>,
}

impl Gate {
    /// Wait until a dispatched unit has journaled its run row and is blocked in flight.
    fn entered(&self) -> String {
        self.entered.recv_timeout(Duration::from_secs(30)).expect("a dispatched unit enters the barrier")
    }

    fn release(&self) {
        self.release.send(()).unwrap();
    }
}

impl Dispatch for Recording {
    fn fire(&self, id: &str, version: u64) -> Result<String, String> {
        let n = {
            let mut fired = self.fired.lock().unwrap();
            fired.push((id.to_string(), version));
            fired.len()
        };
        let mut row = crate::support_row(&format!("{id}-{n}"), RunStatus::Success);
        row.pipeline_id = id.to_string();
        row.started_at = self.catalog.now().unwrap();
        self.catalog.put_run(&row).unwrap();
        if let Some(gate) = &self.gate {
            gate.entered.lock().unwrap().send(id.to_string()).unwrap();
            gate.release.lock().unwrap().recv().unwrap();
        }
        Ok(format!("{id} landed"))
    }
}

fn rig_with(gate: Option<Barrier>) -> (Rig, Arc<Recording>) {
    let rig = Rig::new();
    let rec = Arc::new(Recording { catalog: rig.engine.catalog.clone(), fired: Mutex::default(), gate });
    (rig, rec)
}

fn barrier() -> (Barrier, Gate) {
    let (entered_tx, entered_rx) = channel();
    let (release_tx, release_rx) = channel();
    (Barrier { entered: Mutex::new(entered_tx), release: Mutex::new(release_rx) }, Gate { entered: entered_rx, release: release_tx })
}

fn scheduler(rig: &Rig, rec: &Arc<Recording>, pool: usize) -> Scheduler {
    Scheduler::new(rig.engine.catalog.clone(), rec.clone() as Arc<dyn Dispatch>, "prod", "daemon-a", pool)
}

fn hourly(id: &str) -> Entry {
    Entry { id: id.into(), schedule: Schedule::parse("every 1h").unwrap() }
}

fn journal_run(rig: &Rig, id: &str, pipeline: &str, started: &str) {
    let mut row = crate::support_row(id, RunStatus::Success);
    row.pipeline_id = pipeline.into();
    row.started_at = at(started);
    rig.catalog().put_run(&row).unwrap();
}

fn fired(rec: &Recording) -> Vec<String> {
    rec.fired.lock().unwrap().iter().map(|(id, _)| id.clone()).collect()
}

/// A daemon arming an entry whose next fire has passed fires it once, whatever count of intervals elapsed, and
/// arms the following fire from that run.
// spec: surface.arm.catch-up@2d12457c
#[test]
fn a_daemon_booting_past_missed_intervals_fires_once() {
    let (rig, rec) = rig_with(None);
    journal_run(&rig, "feed-0", "feed", T0);
    // The daemon boots five and a half intervals later.
    rig.clock.advance(5 * 3600 + 1800);
    let mut s = scheduler(&rig, &rec, 4);
    s.arm(7, vec![hourly("feed")]).unwrap();
    let beat = s.beat().unwrap();
    assert_eq!(beat.lease, LeaseState::Held);
    assert_eq!(beat.started, ["feed"]);
    let done = s.drain();
    assert_eq!(done.len(), 1);
    assert_eq!(done[0].result, Ok("feed landed".to_string()));
    // Later beats in the same instant find nothing due: the missed intervals coalesced.
    for _ in 0..3 {
        assert!(s.beat().unwrap().started.is_empty());
    }
    s.drain();
    assert_eq!(*rec.fired.lock().unwrap(), [("feed".to_string(), 7)]);
    // The following fire counts from that run, not from the missed grid.
    assert_eq!(s.next_due().unwrap(), Some(at(T0).plus_secs(6 * 3600 + 1800)));
    rig.clock.advance(3599);
    assert!(s.beat().unwrap().started.is_empty());
    rig.clock.advance(1);
    assert_eq!(s.beat().unwrap().started, ["feed"]);
    s.drain();
}

/// A pipeline's last journaled run start is the latest start across this node's catalog and every run state
/// another node's push carries that records the pipeline.
#[test]
fn a_run_another_node_started_defers_the_next_fire() {
    let (rig, rec) = rig_with(None);
    rig.clock.advance(600);
    let mut s = scheduler(&rig, &rec, 4);
    s.arm(1, vec![hourly("feed"), hourly("other")]).unwrap();
    // Another node fired `feed` ten minutes ago; this catalog journals nothing.
    s.observe_runs([("feed".to_string(), at(T0))].into());
    assert_eq!(s.beat().unwrap().started, ["other"]);
    s.drain();
    assert_eq!(fired(&rec), ["other"]);
    // A local run older than the pulled start leaves the pulled start in force.
    journal_run(&rig, "feed-old", "feed", "2029-12-31T00:00:00Z");
    assert_eq!(s.next_due().unwrap(), Some(at(T0).plus_secs(3600)));
    rig.clock.advance(2999);
    assert!(s.beat().unwrap().started.is_empty());
    rig.clock.advance(1);
    assert_eq!(s.beat().unwrap().started, ["feed"]);
    s.drain();
}

/// A pulled run start later than the scheduler's current instant counts as no start.
#[test]
fn a_pulled_run_start_in_the_future_defers_nothing() {
    let (rig, rec) = rig_with(None);
    rig.clock.advance(600);
    let mut s = scheduler(&rig, &rec, 4);
    s.arm(1, vec![hourly("feed")]).unwrap();
    s.observe_runs([("feed".to_string(), at("2031-06-01T00:00:00Z"))].into());
    assert_eq!(s.next_due().unwrap(), Some(at(T0).plus_secs(600)));
    assert_eq!(s.beat().unwrap().started, ["feed"]);
    s.drain();
    assert_eq!(s.next_due().unwrap(), Some(at(T0).plus_secs(600 + 3600)));
}

/// A beat takes the cadence lease before it dispatches, and a held lease dispatches nothing until it lapses.
#[test]
fn a_held_cadence_lease_dispatches_nothing() {
    let (rig, rec) = rig_with(None);
    let other = rig.catalog().acquire(&LeaseKey::Cadence("prod".into()), "daemon-b", CADENCE_LEASE_TTL_SECS).unwrap().unwrap();
    assert_eq!(rig.catalog().lease_row(&LeaseKey::Cadence("prod".into())).unwrap().holder.as_deref(), Some("daemon-b"));
    let mut s = scheduler(&rig, &rec, 4);
    // The process asks for the lease before it arms, and finding it held arms nothing.
    assert_eq!(s.hold().unwrap(), LeaseState::HeldBy("daemon-b".into()));
    assert!(s.armed().is_empty());
    // A beat under a held lease dispatches nothing, whatever the armed set holds.
    s.arm(1, vec![hourly("feed")]).unwrap();
    let beat = s.beat().unwrap();
    assert_eq!(beat.lease, LeaseState::HeldBy("daemon-b".into()));
    assert!(beat.started.is_empty());
    s.drain();
    assert!(fired(&rec).is_empty());
    // Once the other holder's lease lapses, this process takes it and fires.
    rig.clock.advance(CADENCE_LEASE_TTL_SECS as i64);
    assert_eq!(s.hold().unwrap(), LeaseState::Held);
    let beat = s.beat().unwrap();
    assert_eq!(beat.lease, LeaseState::Held);
    assert_eq!(beat.started, ["feed"]);
    s.drain();
    assert!(!rig.catalog().lease_holds(&other).unwrap());
    // Release clears the holder so a successor need not wait out the lease.
    s.release().unwrap();
    assert_eq!(rig.catalog().lease_row(&LeaseKey::Cadence("prod".into())).unwrap().holder, None);
}

/// The reconciler renews a cadence lease every 30 s while its applied document schedules a dispatchable unit.
// spec: topology.coordinate.cadence-lease-renewal@e8a96b3e
#[test]
fn the_cadence_lease_renews_every_30_s() {
    assert_eq!(CADENCE_LEASE_RENEWAL_SECS, 30);
    let (rig, rec) = rig_with(None);
    let mut s = scheduler(&rig, &rec, 4);
    s.arm(1, vec![Entry { id: "feed".into(), schedule: Schedule::parse("0 3 * * *").unwrap() }]).unwrap();
    journal_run(&rig, "feed-0", "feed", T0);
    s.beat().unwrap();
    let key = LeaseKey::Cadence("prod".into());
    assert_eq!(rig.catalog().lease_row(&key).unwrap().expires_at, Some(at(T0).plus_secs(90)));
    rig.clock.advance(29);
    s.beat().unwrap();
    assert_eq!(rig.catalog().lease_row(&key).unwrap().expires_at, Some(at(T0).plus_secs(90)), "not yet due");
    rig.clock.advance(1);
    s.beat().unwrap();
    assert_eq!(rig.catalog().lease_row(&key).unwrap().expires_at, Some(at(T0).plus_secs(120)));
    assert_eq!(rig.catalog().lease_row(&key).unwrap().fence, 1, "renewal keeps the fence");
}

/// A unit in flight holds its key across beats, and a pool of one reports the rest pending.
///
/// The clock moves only by hand and each fire blocks at a barrier, so every beat observes
/// the in-flight set at a known point: alpha's run row is journaled at T0 before the
/// clock advances.
#[test]
fn in_flight_units_hold_their_key_and_fill_the_pool() {
    let (barrier, gate) = barrier();
    let (rig, rec) = rig_with(Some(barrier));
    let mut s = scheduler(&rig, &rec, 1);
    s.arm(1, vec![hourly("alpha"), hourly("beta")]).unwrap();
    let beat = s.beat().unwrap();
    assert_eq!((beat.started.clone(), beat.pending.clone()), (vec!["alpha".to_string()], vec!["beta".to_string()]));
    assert_eq!(gate.entered(), "alpha");
    // Past alpha's next fire while its first fire still runs: no second instance.
    rig.clock.advance(3600);
    let beat = s.beat().unwrap();
    assert!(beat.started.is_empty());
    assert_eq!(beat.held, ["alpha"]);
    assert_eq!(beat.pending, ["beta"]);
    gate.release();
    s.drain();
    let beat = s.beat().unwrap();
    assert_eq!(beat.started, ["beta"]);
    assert_eq!(gate.entered(), "beta");
    gate.release();
    s.drain();
    assert_eq!(fired(&rec), ["alpha", "beta"]);
}
