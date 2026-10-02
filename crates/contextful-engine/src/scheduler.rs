//! The scheduler behind `serve`: it arms the applied snapshot's schedules, reads each
//! entry's next fire from the catalog's run history, and dispatches due units into a
//! bounded pool under the deployment's cadence lease.
//!
//! One beat is one evaluation of the armed set (`surface.arm.tick-interval`). A beat takes
//! or renews the cadence lease first and dispatches nothing without it
//! (`surface.dispatch.lease-gated`); due-ness counts from history, so a boot past several
//! intervals fires once (`surface.arm.catch-up`); admission holds a key in flight and caps
//! the pool (`surface.dispatch.exclusion-key`, `surface.dispatch.pool-bound`).

use contextful_core::coordinate::{Catalog, Lease, LeaseKey, CADENCE_LEASE_RENEWAL_SECS, CADENCE_LEASE_TTL_SECS};
use contextful_core::run::Failure;
use contextful_core::surface::arm::{next_fire, Schedule};
use contextful_core::surface::dispatch::{admit, Due};
use contextful_core::time::Instant;
use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

/// Where a due unit runs. `fire` blocks until the unit ends, answering its report line or
/// its failure line.
pub trait Dispatch: Send + Sync {
    fn fire(&self, id: &str, version: u64) -> Result<String, String>;
}

/// One armed entry: a pipeline id and its schedule.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    pub id: String,
    pub schedule: Schedule,
}

/// Who holds the cadence lease after a beat.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LeaseState {
    Held,
    HeldBy(String),
}

/// What one beat did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Beat {
    pub lease: LeaseState,
    pub started: Vec<String>,
    pub pending: Vec<String>,
    pub held: Vec<String>,
}

/// One unit that ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Fired {
    pub id: String,
    pub result: Result<String, String>,
}

pub struct Scheduler {
    catalog: Arc<dyn Catalog + Send + Sync>,
    dispatch: Arc<dyn Dispatch>,
    deployment: String,
    holder: String,
    pool: usize,
    lease: Option<Lease>,
    renewed_at: Option<Instant>,
    version: u64,
    armed: Vec<Entry>,
    armed_at: BTreeMap<String, Instant>,
    last_dispatch: BTreeMap<String, Instant>,
    in_flight: Arc<Mutex<BTreeSet<String>>>,
    ended: Arc<Mutex<Vec<Fired>>>,
    handles: Vec<JoinHandle<()>>,
}

impl Scheduler {
    pub fn new(catalog: Arc<dyn Catalog + Send + Sync>, dispatch: Arc<dyn Dispatch>, deployment: &str, holder: &str, pool: usize) -> Scheduler {
        Scheduler {
            catalog,
            dispatch,
            deployment: deployment.to_string(),
            holder: holder.to_string(),
            pool: pool.max(1),
            lease: None,
            renewed_at: None,
            version: 0,
            armed: Vec::new(),
            armed_at: BTreeMap::new(),
            last_dispatch: BTreeMap::new(),
            in_flight: Arc::default(),
            ended: Arc::default(),
            handles: Vec::new(),
        }
    }

    /// Replace the armed set with `entries` of applied `version`. An entry armed before
    /// keeps its arming instant; a new one takes the catalog's now.
    pub fn arm(&mut self, version: u64, entries: Vec<Entry>) -> Result<(), Failure> {
        let now = self.catalog.now()?;
        let ids: BTreeSet<&str> = entries.iter().map(|e| e.id.as_str()).collect();
        self.armed_at.retain(|id, _| ids.contains(id.as_str()));
        for e in &entries {
            self.armed_at.entry(e.id.clone()).or_insert(now);
        }
        self.version = version;
        self.armed = entries;
        Ok(())
    }

    pub fn armed(&self) -> &[Entry] {
        &self.armed
    }

    pub fn version(&self) -> u64 {
        self.version
    }

    /// Take the cadence lease, or renew it once the renewal interval has passed; `false`
    /// while another holder has it or once a later acquisition took it.
    fn hold_lease(&mut self, now: Instant) -> Result<LeaseState, Failure> {
        let key = LeaseKey::Cadence(self.deployment.clone());
        if let Some(lease) = self.lease.clone() {
            let due = self.renewed_at.is_none_or(|at| at.secs_until(now) >= CADENCE_LEASE_RENEWAL_SECS);
            let kept = if due && !self.armed.is_empty() {
                let renewed = self.catalog.renew(&lease, CADENCE_LEASE_TTL_SECS)?;
                if renewed.is_some() {
                    self.renewed_at = Some(now);
                }
                self.lease = renewed;
                self.lease.is_some()
            } else {
                self.catalog.lease_holds(&lease)?
            };
            if kept {
                return Ok(LeaseState::Held);
            }
            self.lease = None;
        }
        match self.catalog.acquire(&key, &self.holder, CADENCE_LEASE_TTL_SECS)? {
            Some(lease) => {
                self.lease = Some(lease);
                self.renewed_at = Some(now);
                Ok(LeaseState::Held)
            }
            None => {
                let holder = self.catalog.lease_row(&key)?.holder.unwrap_or_else(|| "an unnamed holder".into());
                Ok(LeaseState::HeldBy(holder))
            }
        }
    }

    /// Take or renew the cadence lease outside a beat. A process calls it before arming, so
    /// one finding the lease held arms nothing (`surface.dispatch.lease-gated`).
    pub fn hold(&mut self) -> Result<LeaseState, Failure> {
        let now = self.catalog.now()?;
        self.hold_lease(now)
    }

    /// The start of `id`'s newest journaled run.
    fn last_run(&self, id: &str) -> Result<Option<Instant>, Failure> {
        self.catalog.last_run_start(id)
    }

    fn next_of(&self, e: &Entry, now: Instant) -> Result<Instant, Failure> {
        let armed_at = self.armed_at.get(&e.id).copied().unwrap_or(now);
        Ok(next_fire(&e.schedule, self.last_run(&e.id)?, self.last_dispatch.get(&e.id).copied(), armed_at))
    }

    /// One evaluation of the armed set.
    pub fn beat(&mut self) -> Result<Beat, Failure> {
        let now = self.catalog.now()?;
        self.reap();
        let lease = self.hold_lease(now)?;
        let mut beat = Beat { lease, started: Vec::new(), pending: Vec::new(), held: Vec::new() };
        if beat.lease != LeaseState::Held {
            return Ok(beat);
        }
        let mut due = Vec::new();
        for e in &self.armed {
            let at = self.next_of(e, now)?;
            if at <= now {
                due.push(Due { key: e.id.clone(), at });
            }
        }
        let in_flight = self.in_flight.lock().map(|s| s.clone()).unwrap_or_default();
        let admitted = admit(&due, &in_flight, self.pool);
        beat.pending = admitted.pending.into_iter().map(|d| d.key).collect();
        beat.held = admitted.held.into_iter().map(|d| d.key).collect();
        for unit in admitted.start {
            self.start(unit.key.clone(), now);
            beat.started.push(unit.key);
        }
        Ok(beat)
    }

    fn start(&mut self, id: String, now: Instant) {
        if let Ok(mut s) = self.in_flight.lock() {
            s.insert(id.clone());
        }
        self.last_dispatch.insert(id.clone(), now);
        let (dispatch, in_flight, ended, version) = (self.dispatch.clone(), self.in_flight.clone(), self.ended.clone(), self.version);
        self.handles.push(std::thread::spawn(move || {
            let result = dispatch.fire(&id, version);
            if let Ok(mut e) = ended.lock() {
                e.push(Fired { id: id.clone(), result });
            }
            if let Ok(mut s) = in_flight.lock() {
                s.remove(&id);
            }
        }));
    }

    /// Join finished dispatch threads.
    fn reap(&mut self) {
        let (done, running): (Vec<_>, Vec<_>) = self.handles.drain(..).partition(|h| h.is_finished());
        self.handles = running;
        for h in done {
            let _ = h.join();
        }
    }

    /// Units that ended since the last call, without waiting.
    pub fn ended(&mut self) -> Vec<Fired> {
        self.reap();
        self.ended.lock().map(|mut e| std::mem::take(&mut *e)).unwrap_or_default()
    }

    /// Wait for every dispatched unit, answering each that ended.
    pub fn drain(&mut self) -> Vec<Fired> {
        for h in self.handles.drain(..) {
            let _ = h.join();
        }
        self.ended()
    }

    /// The earliest next fire across the armed set, from history as it now stands.
    pub fn next_due(&self) -> Result<Option<Instant>, Failure> {
        let now = self.catalog.now()?;
        let mut next: Option<Instant> = None;
        for e in &self.armed {
            let at = self.next_of(e, now)?;
            next = Some(next.map_or(at, |n| n.min(at)));
        }
        Ok(next)
    }

    /// Release the cadence lease, so a successor need not wait out its life.
    pub fn release(&mut self) -> Result<(), Failure> {
        if let Some(lease) = self.lease.take() {
            self.catalog.release(&lease)?;
        }
        Ok(())
    }
}
