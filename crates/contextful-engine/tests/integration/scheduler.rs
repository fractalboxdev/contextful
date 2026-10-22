//! `surface.arm` and `surface.dispatch`: the scheduler over the catalog's run history and cadence lease.

use crate::support::{at, Rig, SetClock, T0};
use contextful_core::coordinate::{Catalog, LeaseKey, CADENCE_LEASE_RENEWAL_SECS, CADENCE_LEASE_TTL_SECS};
use contextful_core::run::record::RunStatus;
use contextful_core::surface::arm::{Schedule, WAKE_ANSWER_SECS};
use contextful_engine::scheduler::{Dispatch, Entry, LeaseState, Scheduler};
use std::collections::{BTreeMap, BTreeSet};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// A dispatch that journals a run row per fire, starting at the catalog's now, and
/// optionally holds at a barrier: it announces each journaled fire on `entered`, then
/// waits for a release before it returns.
struct Recording {
    catalog: Arc<dyn Catalog + Send + Sync>,
    fired: Mutex<Vec<(String, u64)>>,
    /// The landing steps each fire carried.
    steps: Mutex<Vec<(String, Vec<String>)>>,
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
    fn fire(&self, id: &str, steps: &[String], _derived_parents: &BTreeMap<String, String>, version: u64) -> Result<String, String> {
        self.steps.lock().unwrap().push((id.to_string(), steps.to_vec()));
        // The run row lands before the fire is listed, so a test seeing the fire listed reads
        // a start time the clock had not yet moved past.
        {
            let mut fired = self.fired.lock().unwrap();
            let mut row = crate::support_row(&format!("{id}-{}", fired.len() + 1), RunStatus::Success);
            row.pipeline_id = id.to_string();
            row.started_at = self.catalog.now().unwrap();
            self.catalog.put_run(&row).unwrap();
            fired.push((id.to_string(), version));
        }
        if let Some(gate) = &self.gate {
            gate.entered.lock().unwrap().send(id.to_string()).unwrap();
            gate.release.lock().unwrap().recv().unwrap();
        }
        Ok(format!("{id} landed"))
    }
}

fn rig_with(gate: Option<Barrier>) -> (Rig, Arc<Recording>) {
    let rig = Rig::new();
    let rec = Arc::new(Recording { catalog: rig.engine.catalog.clone(), fired: Mutex::default(), steps: Mutex::default(), gate });
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
    Entry { id: id.into(), schedule: Schedule::parse("every 1h").unwrap(), priority: 0 }
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
    s.arm(1, vec![Entry { id: "feed".into(), schedule: Schedule::parse("0 3 * * *").unwrap(), priority: 0 }]).unwrap();
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

/// A `[[pipeline]]` block naming `after = "<id>"` is a landing step of that entry's dependent run; the run is
/// the unit and cadence rides its head entry. A due id that is a step raises `DispatchUnitNotAHead` and
/// starts nothing.
// spec: surface.dispatch.not-a-head@3ff3a914
#[test]
fn a_head_fires_its_steps_as_one_unit_and_a_due_step_starts_nothing() {
    let (rig, rec) = rig_with(None);
    let mut s = scheduler(&rig, &rec, 4);
    let steps = [("orders".to_string(), vec!["orders-enrich".to_string(), "orders-score".to_string()])].into();
    s.arm_runs(3, vec![hourly("orders"), hourly("orders-enrich")], steps).unwrap();
    let beat = s.beat().unwrap();
    assert_eq!(beat.started, ["orders"]);
    assert_eq!(beat.refused.len(), 1);
    let (id, refusal) = &beat.refused[0];
    assert_eq!(id, "orders-enrich");
    assert!(refusal.starts_with("DispatchUnitNotAHead:") && refusal.contains("`orders`"), "{refusal}");
    s.drain();
    assert_eq!(fired(&rec), ["orders"], "the step started nothing of its own");
    assert_eq!(*rec.steps.lock().unwrap(), [("orders".to_string(), vec!["orders-enrich".to_string(), "orders-score".to_string()])]);
    // The refusal is once per due fire, not once per beat.
    assert!(s.beat().unwrap().refused.is_empty());
    s.drain();
}

/// A dispatch for wake-paced fixtures: it journals a run row at the catalog's now, then
/// advances the clock by `run_secs`; an id in `fails` returns `Err` before any run row,
/// the shape of a pull that fails ahead of the run. With a barrier, every fire holds until
/// the test releases it.
struct Paced {
    catalog: Arc<dyn Catalog + Send + Sync>,
    clock: SetClock,
    run_secs: i64,
    fails: Vec<String>,
    started: Mutex<Vec<String>>,
    gate: Option<Barrier>,
}

impl Dispatch for Paced {
    fn fire(&self, id: &str, _steps: &[String], _derived_parents: &BTreeMap<String, String>, _version: u64) -> Result<String, String> {
        let journaled = !self.fails.iter().any(|f| f == id);
        {
            let mut started = self.started.lock().unwrap();
            if journaled {
                let mut row = crate::support_row(&format!("{id}-{}", started.len() + 1), RunStatus::Success);
                row.pipeline_id = id.to_string();
                row.started_at = self.catalog.now().unwrap();
                self.catalog.put_run(&row).unwrap();
            }
            started.push(id.to_string());
        }
        self.clock.advance(self.run_secs);
        if let Some(gate) = &self.gate {
            gate.entered.lock().unwrap().send(id.to_string()).unwrap();
            gate.release.lock().unwrap().recv().unwrap();
        }
        if journaled { Ok(format!("{id} landed")) } else { Err(format!("{id}: pull failed before the run")) }
    }
}

/// The external trigger's wake spacing in the fixtures: five wakes per hourly tick.
const WAKE_GAP_SECS: i64 = 720;

/// `count` hourly cron entries `u00`.. on the `0 * * * *` grid, so units whose last runs
/// share an hour tie at the next slot (unlike `every 1h`, which never snaps).
fn cron_units(count: usize) -> Vec<Entry> {
    (0..count).map(|i| Entry { id: format!("u{i:02}"), schedule: Schedule::parse("0 * * * *").unwrap(), priority: 0 }).collect()
}

fn paced(rig: &Rig, run_secs: i64, fails: &[&str], gate: Option<Barrier>) -> Arc<Paced> {
    Arc::new(Paced {
        catalog: rig.engine.catalog.clone(),
        clock: rig.clock.clone(),
        run_secs,
        fails: fails.iter().map(|f| f.to_string()).collect(),
        started: Mutex::default(),
        gate,
    })
}

fn paced_scheduler(rig: &Rig, dispatch: &Arc<Paced>) -> Scheduler {
    Scheduler::new(rig.engine.catalog.clone(), dispatch.clone() as Arc<dyn Dispatch>, "prod", "daemon-a", 1)
}

/// Each unit with one run journaled at T0, so all of them tie on the next hourly slot.
fn journal_cohort(rig: &Rig, units: &[Entry]) {
    for e in units {
        journal_run(rig, &format!("{}-0", e.id), &e.id, T0);
    }
}

/// Move the rig clock to the absolute instant `secs` past T0.
fn clock_to(rig: &Rig, secs: i64) {
    let now = rig.catalog().now().unwrap().unix_secs();
    let target = at(T0).unix_secs() + secs;
    assert!(target >= now, "wake times run forward");
    rig.clock.advance(target - now);
}

/// One wake as the external trigger runs it, with every fire held at the barrier until the
/// wake's settle returns, so a wake admits exactly what its beat started. The held units are
/// then released and joined before the next wake.
fn held_wake(s: &mut Scheduler, gate: &Gate) -> Vec<String> {
    assert_eq!(s.hold().unwrap(), LeaseState::Held);
    let beat = s.beat().unwrap();
    assert_eq!(beat.lease, LeaseState::Held);
    for _ in &beat.started {
        gate.entered();
    }
    s.settle(std::time::Instant::now());
    for _ in &beat.started {
        gate.release();
    }
    s.drain();
    s.release().unwrap();
    beat.started
}

/// A [`Paced`] dispatch whose wake answers `WAKE_ANSWER_SECS` after it opens on the catalog's
/// clock: a fire ending by then returns at once, and one ending later is still running when
/// the wake answers, so it holds at the barrier until the test releases it.
struct Answered {
    inner: Arc<Paced>,
    catalog: Arc<dyn Catalog + Send + Sync>,
    /// The catalog instant the current wake answers at, in unix seconds.
    answers_at: Mutex<i64>,
    gate: Barrier,
}

impl Dispatch for Answered {
    fn fire(&self, id: &str, steps: &[String], derived_parents: &BTreeMap<String, String>, version: u64) -> Result<String, String> {
        let out = self.inner.fire(id, steps, derived_parents, version);
        if self.catalog.now().unwrap().unix_secs() > *self.answers_at.lock().unwrap() {
            self.gate.entered.lock().unwrap().send(id.to_string()).unwrap();
            self.gate.release.lock().unwrap().recv().unwrap();
        }
        out
    }
}

/// Twenty hourly units tied by a 14 h outage, pool 1, five wakes per hour, 20 s per fire
/// against the 25 s wake answer: the beat starts one unit, and the slot it frees at 20 s
/// admits the next, still running at the answer. Two units start per wake, so the last ten
/// first fire one tick after recovery.
#[test]
fn a_backlog_of_twenty_units_clears_one_tick_after_recovery() {
    const UNITS: usize = 20;
    const WAKES: i64 = 10;
    let rig = Rig::new();
    let paced = paced(&rig, 20, &[], None);
    let (barrier, gate) = barrier();
    let dispatch = Arc::new(Answered { inner: paced.clone(), catalog: rig.engine.catalog.clone(), answers_at: Mutex::default(), gate: barrier });
    journal_cohort(&rig, &cron_units(UNITS));
    let recovery = 14 * 3600;
    clock_to(&rig, recovery);
    let mut s = Scheduler::new(rig.engine.catalog.clone(), dispatch.clone() as Arc<dyn Dispatch>, "prod", "daemon-a", 1);
    s.arm(1, cron_units(UNITS)).unwrap();
    let mut first_fire: BTreeMap<String, i64> = BTreeMap::new();
    let mut per_wake = Vec::new();
    for k in 0..WAKES {
        clock_to(&rig, recovery + k * WAKE_GAP_SECS);
        *dispatch.answers_at.lock().unwrap() = rig.catalog().now().unwrap().unix_secs() + WAKE_ANSWER_SECS as i64;
        let before = paced.started.lock().unwrap().len();
        assert_eq!(s.hold().unwrap(), LeaseState::Held);
        assert_eq!(s.beat().unwrap().lease, LeaseState::Held);
        // The answer falls on the catalog's clock; the wall deadline only bounds the fixture,
        // as every fire ending before the answer returns at once and the one past it holds.
        s.settle(std::time::Instant::now() + Duration::from_secs(1));
        for _ in s.in_flight() {
            gate.entered();
            gate.release();
        }
        s.drain();
        let started = paced.started.lock().unwrap()[before..].to_vec();
        per_wake.push(started.len());
        for id in started {
            first_fire.entry(id).or_insert(k * WAKE_GAP_SECS / 3600);
        }
    }
    assert_eq!(per_wake, [2; WAKES as usize], "the beat's start and one refill per wake");
    // A unit that never fires waits the whole window.
    let window = WAKES * WAKE_GAP_SECS / 3600;
    let waited: Vec<i64> = cron_units(UNITS).iter().map(|e| first_fire.get(&e.id).copied().unwrap_or(window)).collect();
    assert_eq!(waited, [[0; 10], [1; 10]].concat());
    let most = waited.iter().copied().max().unwrap();
    contextful_eval::record::emit("dispatch-backlog-ticks", most as f64, UNITS as u64, 0);
}

/// A refill admits only what the wake's evaluation left pending: a unit falling due while the
/// first fire runs waits for the next wake, though a freed slot starts a pending one.
#[test]
fn a_unit_falling_due_after_the_evaluation_waits_for_the_next_wake() {
    let rig = Rig::new();
    let dispatch = paced(&rig, 20, &[], None);
    journal_run(&rig, "late-0", "late", T0);
    clock_to(&rig, 3590);
    let mut s = paced_scheduler(&rig, &dispatch);
    s.arm(1, vec![hourly("a"), hourly("b"), hourly("late")]).unwrap();
    assert_eq!(s.hold().unwrap(), LeaseState::Held);
    let beat = s.beat().unwrap();
    assert_eq!((beat.started, beat.pending), (vec!["a".to_string()], vec!["b".to_string()]));
    s.settle(std::time::Instant::now() + Duration::from_secs(25));
    s.drain();
    // `late` fell due at 3600 s, inside a's 20 s fire; the refill admitted b alone.
    assert_eq!(*dispatch.started.lock().unwrap(), ["a", "b"]);
    assert_eq!(s.refill().started, ["b"]);
    assert_eq!(s.beat().unwrap().started, ["late"]);
    s.drain();
}

/// A dispatch record the catalog cannot write, here a row another node's clock set past the
/// catalog's now, fails the beat before any admitted unit starts.
#[test]
fn a_dispatch_record_that_will_not_write_starts_nothing() {
    let rig = Rig::new();
    let dispatch = paced(&rig, 20, &[], None);
    let mut s = Scheduler::new(rig.engine.catalog.clone(), dispatch.clone() as Arc<dyn Dispatch>, "prod", "daemon-a", 2);
    s.arm(1, vec![hourly("a"), hourly("b")]).unwrap();
    assert_eq!(s.hold().unwrap(), LeaseState::Held);
    rig.engine.catalog.acquire(&LeaseKey::Dispatch("b".into()), "daemon-b", 3600).unwrap().expect("a free row");
    let failure = s.beat().unwrap_err().to_string();
    assert!(failure.contains("recording the dispatch of `b`"), "{failure}");
    s.drain();
    assert!(dispatch.started.lock().unwrap().is_empty(), "a's record landed, yet a did not start");
    assert!(s.in_flight().is_empty());
}

/// A dispatch whose first fire outlives the cadence lease, after which another holder takes it.
struct Usurped {
    inner: Arc<Paced>,
    catalog: Arc<dyn Catalog + Send + Sync>,
}

impl Dispatch for Usurped {
    fn fire(&self, id: &str, steps: &[String], derived_parents: &BTreeMap<String, String>, version: u64) -> Result<String, String> {
        let out = self.inner.fire(id, steps, derived_parents, version);
        self.catalog.acquire(&LeaseKey::Cadence("prod".into()), "daemon-b", CADENCE_LEASE_TTL_SECS).unwrap().expect("the lapsed lease is free");
        out
    }
}

/// A refill admits through the cadence lease: once another holder takes it mid-wake, the
/// slot the first fire frees starts nothing, and the rest stay waiting, so the wake reports
/// them pending.
#[test]
fn a_refill_starts_nothing_once_another_holder_takes_the_cadence_lease() {
    let rig = Rig::new();
    let units = cron_units(3);
    journal_cohort(&rig, &units);
    clock_to(&rig, 14 * 3600);
    let inner = paced(&rig, CADENCE_LEASE_TTL_SECS as i64 + 10, &[], None);
    let dispatch = Arc::new(Usurped { inner: inner.clone(), catalog: rig.engine.catalog.clone() });
    let mut s = Scheduler::new(rig.engine.catalog.clone(), dispatch as Arc<dyn Dispatch>, "prod", "daemon-a", 1);
    s.arm(1, units).unwrap();
    assert_eq!(s.hold().unwrap(), LeaseState::Held);
    let beat = s.beat().unwrap();
    assert_eq!((beat.started, beat.pending), (vec!["u00".to_string()], vec!["u01".to_string(), "u02".to_string()]));
    let ended = s.settle(std::time::Instant::now() + Duration::from_secs(25));
    assert_eq!(ended.len(), 1);
    assert_eq!(*inner.started.lock().unwrap(), ["u00"], "the freed slot admits nothing under another holder's lease");
    assert_eq!(s.waiting(), ["u01", "u02"]);
    assert!(s.refill().started.is_empty());
    assert_eq!(s.refill().stopped.as_deref(), Some("the cadence lease is held by daemon-b"));
    assert_eq!(rig.catalog().lease_row(&LeaseKey::Cadence("prod".into())).unwrap().holder.as_deref(), Some("daemon-b"));
}

/// Six hourly units tied on one cron slot, pool 1, five wakes per tick for 12 ticks, every
/// fire held until its wake answers: the least recent dispatch starts first inside the slot,
/// so the misses rotate and each unit misses two ticks.
#[test]
fn overload_misses_spread_evenly_across_tied_units() {
    const UNITS: usize = 6;
    const TICKS: i64 = 12;
    let (barrier, gate) = barrier();
    let rig = Rig::new();
    let dispatch = paced(&rig, 0, &[], Some(barrier));
    journal_cohort(&rig, &cron_units(UNITS));
    let mut s = paced_scheduler(&rig, &dispatch);
    s.arm(1, cron_units(UNITS)).unwrap();
    let mut ticks_fired: BTreeMap<String, BTreeSet<i64>> = BTreeMap::new();
    for tick in 1..=TICKS {
        for w in 0..5 {
            clock_to(&rig, tick * 3600 + w * WAKE_GAP_SECS);
            for id in held_wake(&mut s, &gate) {
                ticks_fired.entry(id).or_default().insert(tick);
            }
        }
    }
    let missed: Vec<i64> = cron_units(UNITS).iter().map(|e| TICKS - ticks_fired.get(&e.id).map_or(0, |t| t.len() as i64)).collect();
    assert_eq!(missed, [2; UNITS]);
    let most = missed.iter().copied().max().unwrap();
    contextful_eval::record::emit("dispatch-overload-misses", most as f64, TICKS as u64, 0);
}

/// An armed entry's next fire is the first instant its schedule admits after the later of its
/// pipeline's last journaled run start and its last dispatch, which the catalog holding the
/// cadence lease records before the unit starts. A unit failing before its run row then
/// moves past its dispatch even in a fresh scheduler per wake (a cycle, or a replaced
/// container), and the tied cohort takes the pool one wake each.
// spec: surface.arm.next-fire-from-history@d574a85b
#[test]
fn a_unit_failing_before_its_run_row_yields_the_pool_to_a_fresh_scheduler() {
    const UNITS: usize = 20;
    const WAKES: i64 = 10;
    assert!(CADENCE_LEASE_TTL_SECS < WAKE_GAP_SECS as u64, "each fresh scheduler reacquires a lapsed lease");
    let (barrier, gate) = barrier();
    let rig = Rig::new();
    let dispatch = paced(&rig, 0, &["u00"], Some(barrier));
    journal_cohort(&rig, &cron_units(UNITS));
    let recovery = 14 * 3600;
    let mut started = Vec::new();
    for k in 0..WAKES {
        clock_to(&rig, recovery + k * WAKE_GAP_SECS);
        let mut s = paced_scheduler(&rig, &dispatch);
        s.arm(1, cron_units(UNITS)).unwrap();
        started.extend(held_wake(&mut s, &gate));
    }
    let expected: Vec<String> = (0..WAKES).map(|i| format!("u{i:02}")).collect();
    assert_eq!(started, expected);
    assert_eq!(*dispatch.started.lock().unwrap(), started);
    // u00 journaled no run; its dispatch record alone moved it behind the cohort.
    assert_eq!(rig.catalog().last_run_start("u00").unwrap(), Some(at(T0)));
    assert_eq!(rig.catalog().lease_row(&LeaseKey::Dispatch("u00".into())).unwrap().expires_at, Some(at(T0).plus_secs(recovery as u64)));
    let others = started.iter().filter(|id| *id != "u00").count();
    assert!(others >= 9, "{others}");
    contextful_eval::record::emit("dispatch-failing-head", others as f64, WAKES as u64, 0);
}

/// A dispatch declining to claim units while `closed` is set, and counting the claims it
/// granted that are still held.
struct Quiescing {
    inner: Arc<Recording>,
    closed: std::sync::atomic::AtomicBool,
    held: Arc<std::sync::atomic::AtomicUsize>,
}

/// One granted claim; it counts itself held until dropped.
struct Held(Arc<std::sync::atomic::AtomicUsize>);

impl Drop for Held {
    fn drop(&mut self) {
        self.0.fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
    }
}

impl Dispatch for Quiescing {
    fn fire(&self, id: &str, steps: &[String], derived_parents: &BTreeMap<String, String>, version: u64) -> Result<String, String> {
        assert_eq!(self.held.load(std::sync::atomic::Ordering::SeqCst), 1, "a fire runs under its claim");
        self.inner.fire(id, steps, derived_parents, version)
    }

    fn claim(&self, _id: &str, _steps: &[String], _version: u64) -> Option<Box<dyn Send>> {
        if self.closed.load(std::sync::atomic::Ordering::SeqCst) {
            return None;
        }
        self.held.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        Some(Box::new(Held(self.held.clone())))
    }
}

/// A due unit whose dispatch declines its claim starts nothing, reports deferred and stays due: the
/// next beat after the claim opens starts it in the same instant, and the claim releases when it ends.
// spec: surface.dispatch.export-quiesced@3aaeb45f
#[test]
fn a_declined_claim_defers_the_unit_and_keeps_it_due() {
    let (rig, rec) = rig_with(None);
    let held = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let dispatch = Arc::new(Quiescing { inner: rec.clone(), closed: true.into(), held: held.clone() });
    let mut s = Scheduler::new(rig.engine.catalog.clone(), dispatch.clone() as Arc<dyn Dispatch>, "prod", "daemon-a", 4);
    s.arm(1, vec![hourly("alpha")]).unwrap();
    for _ in 0..2 {
        let beat = s.beat().unwrap();
        assert!(beat.started.is_empty());
        assert_eq!(beat.deferred, ["alpha"]);
    }
    s.drain();
    assert!(fired(&rec).is_empty());
    assert_eq!(s.next_due().unwrap(), Some(at(T0)), "a deferred unit stays due");
    dispatch.closed.store(false, std::sync::atomic::Ordering::SeqCst);
    let beat = s.beat().unwrap();
    assert_eq!(beat.started, ["alpha"]);
    assert!(beat.deferred.is_empty());
    s.drain();
    assert_eq!(fired(&rec), ["alpha"]);
    assert_eq!(held.load(std::sync::atomic::Ordering::SeqCst), 0, "the claim releases when the unit ends");
}

/// A unit still in flight names its landing steps beside its head, so a dependent job due behind a head that
/// already landed reads pending under its own id until the unit ends.
#[test]
fn an_in_flight_unit_names_its_dependent_steps_pending() {
    let (barrier, gate) = barrier();
    let (rig, rec) = rig_with(Some(barrier));
    let mut s = scheduler(&rig, &rec, 4);
    let steps = [("orders".to_string(), vec!["job:orders-build".to_string()])].into();
    s.arm_runs(1, vec![hourly("orders"), hourly("audit")], steps).unwrap();
    assert_eq!(s.beat().unwrap().started, ["audit", "orders"]);
    gate.entered();
    gate.entered();
    let in_flight = s.in_flight();
    assert_eq!(s.with_steps(in_flight.iter().map(String::as_str)), ["audit", "job:orders-build", "orders"]);
    gate.release();
    gate.release();
    s.drain();
    assert!(s.with_steps(s.in_flight().iter().map(String::as_str)).is_empty());
}

/// A dispatch declining the claim of each unit in `declined`.
struct Declining {
    inner: Arc<Recording>,
    declined: BTreeSet<String>,
}

impl Dispatch for Declining {
    fn fire(&self, id: &str, steps: &[String], derived_parents: &BTreeMap<String, String>, version: u64) -> Result<String, String> {
        self.inner.fire(id, steps, derived_parents, version)
    }

    fn claim(&self, id: &str, _steps: &[String], _version: u64) -> Option<Box<dyn Send>> {
        (!self.declined.contains(id)).then(|| Box::new(()) as Box<dyn Send>)
    }
}

/// Under a pool of one, a deferred unit leaves its slot to the next unit in admission order, writes no dispatch
/// record, and stays waiting, so a successor process still finds it due.
// spec: surface.dispatch.export-deferred-slot@36629f03
#[test]
fn a_deferred_unit_leaves_its_slot_and_writes_no_dispatch_record() {
    let (rig, rec) = rig_with(None);
    let dispatch = Arc::new(Declining { inner: rec.clone(), declined: ["alpha".to_string()].into() });
    let mut s = Scheduler::new(rig.engine.catalog.clone(), dispatch.clone() as Arc<dyn Dispatch>, "prod", "daemon-a", 1);
    s.arm(1, vec![hourly("alpha"), hourly("beta")]).unwrap();
    let beat = s.beat().unwrap();
    assert_eq!(beat.deferred, ["alpha"]);
    assert_eq!(beat.started, ["beta"], "the deferred unit's slot went to the next unit");
    assert!(beat.pending.is_empty(), "{beat:?}");
    assert_eq!(s.waiting(), ["alpha"], "a deferred unit waits for the wake's refill");
    s.drain();
    assert_eq!(fired(&rec), ["beta"]);
    assert_eq!(rig.catalog().lease_row(&LeaseKey::Dispatch("alpha".into())).unwrap().expires_at, None);
    s.release().unwrap();
    let mut successor = Scheduler::new(rig.engine.catalog.clone(), dispatch as Arc<dyn Dispatch>, "prod", "daemon-b", 1);
    successor.arm(1, vec![hourly("alpha")]).unwrap();
    assert_eq!(successor.next_due().unwrap(), Some(at(T0)), "the deferred fire survives a restart");
}
