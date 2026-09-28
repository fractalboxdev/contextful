//! The run's one cancellation token and the keeper thread feeding it: one catalog read
//! before the run's first await, then one every 500 ms, with the owner lease renewed on
//! its own cadence.

use contextful_core::coordinate::{Catalog, Lease};
use contextful_core::run::cancel::POLL_INTERVAL_MS;
use contextful_core::run::ports::Cancellation;
use contextful_core::run::record::{OWNER_LEASE_RENEWAL_SECS, OWNER_LEASE_TTL_SECS};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

/// A flag threads block on: `raise` wakes every waiter at once, so a wait costs no
/// wakeup until its deadline or the raise.
#[derive(Debug, Default)]
struct Signal {
    raised: Mutex<bool>,
    changed: Condvar,
}

impl Signal {
    fn lock(&self) -> MutexGuard<'_, bool> {
        self.raised.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn raise(&self) {
        *self.lock() = true;
        self.changed.notify_all();
    }

    fn raised(&self) -> bool {
        *self.lock()
    }

    /// Block until `deadline` or the raise; `true` when the deadline passed unraised. No
    /// deadline blocks until the raise.
    fn wait_until(&self, deadline: Option<Instant>) -> bool {
        let mut raised = self.lock();
        loop {
            if *raised {
                return false;
            }
            raised = match deadline {
                None => self.changed.wait(raised).unwrap_or_else(|e| e.into_inner()),
                Some(deadline) => {
                    let now = Instant::now();
                    if now >= deadline {
                        return true;
                    }
                    self.changed.wait_timeout(raised, deadline - now).unwrap_or_else(|e| e.into_inner()).0
                }
            };
        }
    }
}

/// The token every await of one run selects on.
#[derive(Debug, Clone, Default)]
pub struct CancelToken(Arc<Signal>);

impl CancelToken {
    /// Fire the token, waking every [`CancelToken::wait_timeout`] blocked on it.
    pub fn fire(&self) {
        self.0.raise();
    }

    /// Block for `total` unless the token fires first: `true` when `total` passed
    /// unfired, `false` at once on a fire, including one before the call.
    pub fn wait_timeout(&self, total: Duration) -> bool {
        self.0.wait_until(Instant::now().checked_add(total))
    }
}

impl Cancellation for CancelToken {
    fn requested(&self) -> bool {
        self.0.raised()
    }
}

/// One poll: read the run row and fire the token when it carries a stop. A failed read
/// warns and leaves the token as it was (`run.cancel.storage-blip`).
pub fn poll(catalog: &dyn Catalog, run_id: &str, token: &CancelToken) {
    match catalog.run(run_id) {
        Ok(Some(row)) if row.stop.is_some() => token.fire(),
        Ok(_) => {}
        Err(f) => eprintln!("warning: reading the stop mark of run `{run_id}`: {f}; polling continues"),
    }
}

/// The cadences the keeper runs on.
#[derive(Debug, Clone, Copy)]
pub struct Cadence {
    pub poll: Duration,
    pub renew: Duration,
}

impl Default for Cadence {
    fn default() -> Cadence {
        Cadence { poll: Duration::from_millis(POLL_INTERVAL_MS), renew: Duration::from_secs(OWNER_LEASE_RENEWAL_SECS) }
    }
}

/// The instants the keeper wakes at: the earlier of the next poll and the next renewal.
/// Each cadence runs from the instant it last ran, so a late wakeup runs a job once and
/// never in a catch-up burst.
#[derive(Debug, Clone, Copy)]
pub struct Schedule {
    cadence: Cadence,
    next_poll: Instant,
    next_renew: Instant,
}

/// The jobs one wakeup runs.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Due {
    pub poll: bool,
    pub renew: bool,
}

impl Schedule {
    /// The schedule of a keeper whose first poll ran at `start`.
    pub fn new(cadence: Cadence, start: Instant) -> Schedule {
        Schedule { cadence, next_poll: start + cadence.poll, next_renew: start + cadence.renew }
    }

    /// The instant the keeper next wakes at.
    pub fn next(&self) -> Instant {
        self.next_poll.min(self.next_renew)
    }

    /// The jobs due at `now`, each rescheduled one cadence after `now`.
    pub fn take(&mut self, now: Instant) -> Due {
        let due = Due { poll: now >= self.next_poll, renew: now >= self.next_renew };
        if due.poll {
            self.next_poll = now + self.cadence.poll;
        }
        if due.renew {
            self.next_renew = now + self.cadence.renew;
        }
        due
    }
}

/// The background thread polling the stop mark and renewing the owner lease; it sleeps
/// until the next poll or renewal and stops at once when dropped.
pub struct Keeper {
    stop: Arc<Signal>,
    handle: Option<JoinHandle<()>>,
}

impl Keeper {
    /// Poll once before returning, so the run's first await already sees a stop written
    /// before it opened, then keep polling and renewing on `cadence`.
    pub fn start(catalog: Arc<dyn Catalog + Send + Sync>, run_id: &str, token: CancelToken, cadence: Cadence) -> Keeper {
        Keeper::start_holding(catalog, run_id, token, cadence, Arc::default())
    }

    /// [`Keeper::start`], renewing the single-writer lease in `held` on the owner lease's
    /// cadence as well, so a run holding it past one time-to-live keeps it.
    pub fn start_holding(catalog: Arc<dyn Catalog + Send + Sync>, run_id: &str, token: CancelToken, cadence: Cadence, held: Arc<Mutex<Option<Lease>>>) -> Keeper {
        poll(catalog.as_ref(), run_id, &token);
        let mut schedule = Schedule::new(cadence, Instant::now());
        let stop = Arc::new(Signal::default());
        let (flag, id) = (stop.clone(), run_id.to_string());
        let handle = std::thread::spawn(move || {
            while flag.wait_until(Some(schedule.next())) {
                let due = schedule.take(Instant::now());
                if due.poll {
                    poll(catalog.as_ref(), &id, &token);
                }
                if due.renew {
                    renew(catalog.as_ref(), &id);
                    renew_lease(catalog.as_ref(), &held);
                }
            }
        });
        Keeper { stop, handle: Some(handle) }
    }
}

/// Renew the single-writer lease in `held`. A lease a later acquisition took stays in the
/// slot, where the fenced commit refuses it.
pub fn renew_lease(catalog: &dyn Catalog, held: &Mutex<Option<Lease>>) {
    let mut slot = held.lock().unwrap_or_else(|e| e.into_inner());
    let Some(lease) = slot.clone() else { return };
    match catalog.renew(&lease, OWNER_LEASE_TTL_SECS) {
        Ok(Some(renewed)) => *slot = Some(renewed),
        Ok(None) => eprintln!("warning: lease `{}` passed to a later holder; this run's commit is fenced", lease.key),
        Err(f) => eprintln!("warning: renewing lease `{}`: {f}", lease.key),
    }
}

/// Renew the run row's owner lease against the catalog's clock.
pub fn renew(catalog: &dyn Catalog, run_id: &str) {
    let now = match catalog.now() {
        Ok(n) => n,
        Err(f) => return eprintln!("warning: renewing the lease of run `{run_id}`: {f}"),
    };
    let renewed = catalog.update_run(run_id, &mut |row| {
        if row.status.is_in_flight() {
            row.owner = row.owner.as_ref().map(|o| o.renewed(now));
        }
        Ok(())
    });
    if let Err(f) = renewed {
        eprintln!("warning: renewing the lease of run `{run_id}`: {f}");
    }
}

impl Drop for Keeper {
    fn drop(&mut self) {
        self.stop.raise();
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
    }
}
