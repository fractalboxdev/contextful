//! `surface.arm` and `surface.dispatch`: the scheduler over the catalog's run history and cadence lease.

use crate::support::{at, Rig, T0};
use contextful_core::coordinate::{Catalog, LeaseKey, CADENCE_LEASE_RENEWAL_SECS, CADENCE_LEASE_TTL_SECS};
use contextful_core::run::record::RunStatus;
use contextful_core::surface::arm::Schedule;
use contextful_engine::scheduler::{Dispatch, Entry, LeaseState, Scheduler};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::{Arc, Mutex};

/// A dispatch that journals a run row per fire, starting at the catalog's now, and
/// optionally waits for a release before it returns.
struct Recording {
    catalog: Arc<dyn Catalog + Send + Sync>,
    fired: Mutex<Vec<(String, u64)>>,
    gate: Option<Mutex<Receiver<()>>>,
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
            gate.lock().unwrap().recv().unwrap();
        }
        Ok(format!("{id} landed"))
    }
}

fn rig_with(gate: Option<Receiver<()>>) -> (Rig, Arc<Recording>) {
    let rig = Rig::new();
    let rec = Arc::new(Recording { catalog: rig.engine.catalog.clone(), fired: Mutex::default(), gate: gate.map(Mutex::new) });
    (rig, rec)
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

/// A serve process dispatches only while it holds its deployment's cadence lease; a process finding the lease
/// held arms nothing and, under `--cycle`, exits naming the holder.
// spec: surface.dispatch.lease-gated@f1237082
#[test]
fn a_held_cadence_lease_dispatches_nothing() {
    let (rig, rec) = rig_with(None);
    let other = rig.catalog().acquire(&LeaseKey::Cadence("prod".into()), "daemon-b", CADENCE_LEASE_TTL_SECS).unwrap().unwrap();
    assert_eq!(rig.catalog().lease_row(&LeaseKey::Cadence("prod".into())).unwrap().holder.as_deref(), Some("daemon-b"));
    let mut s = scheduler(&rig, &rec, 4);
    s.arm(1, vec![hourly("feed")]).unwrap();
    let beat = s.beat().unwrap();
    assert_eq!(beat.lease, LeaseState::HeldBy("daemon-b".into()));
    assert!(beat.started.is_empty());
    s.drain();
    assert!(fired(&rec).is_empty());
    // Once the other holder's lease lapses, this process takes it and fires.
    rig.clock.advance(CADENCE_LEASE_TTL_SECS as i64);
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
#[test]
fn in_flight_units_hold_their_key_and_fill_the_pool() {
    let (tx, rx): (Sender<()>, Receiver<()>) = channel();
    let (rig, rec) = rig_with(Some(rx));
    let mut s = scheduler(&rig, &rec, 1);
    s.arm(1, vec![hourly("alpha"), hourly("beta")]).unwrap();
    let beat = s.beat().unwrap();
    assert_eq!((beat.started.clone(), beat.pending.clone()), (vec!["alpha".to_string()], vec!["beta".to_string()]));
    // Past alpha's next fire while its first fire still runs: no second instance.
    rig.clock.advance(3600);
    let beat = s.beat().unwrap();
    assert!(beat.started.is_empty());
    assert_eq!(beat.held, ["alpha"]);
    assert_eq!(beat.pending, ["beta"]);
    tx.send(()).unwrap();
    s.drain();
    let beat = s.beat().unwrap();
    assert_eq!(beat.started, ["beta"]);
    tx.send(()).unwrap();
    s.drain();
    assert_eq!(fired(&rec), ["alpha", "beta"]);
}
