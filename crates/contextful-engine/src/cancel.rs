//! The run's one cancellation token and the engine's keeper feeding it: one catalog read
//! before the run's first await, then one every 500 ms, with the owner lease renewed on
//! its own cadence, for every open execution from one thread.

use contextful_core::coordinate::{Catalog, Lease};
use contextful_core::run::cancel::POLL_INTERVAL_MS;
use contextful_core::run::ports::Cancellation;
use contextful_core::run::record::{OWNER_LEASE_RENEWAL_SECS, OWNER_LEASE_TTL_SECS};
use std::cmp::Reverse;
use std::collections::{BinaryHeap, HashMap};
use std::panic::AssertUnwindSafe;
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
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

/// One registered execution: where its row lives, the token its polls feed, the
/// single-writer lease it holds, and its own poll and renewal schedule.
struct Entry {
    catalog: Arc<dyn Catalog + Send + Sync>,
    run_id: String,
    token: CancelToken,
    held: Arc<Mutex<Option<Lease>>>,
    schedule: Schedule,
}

/// The keeper's registrations and its deadline heap. Each live entry has exactly one heap
/// item, at its schedule's next instant; an item whose entry is gone or has moved is stale
/// and is dropped when it reaches the top.
#[derive(Default)]
struct State {
    next_id: u64,
    entries: HashMap<u64, Entry>,
    deadlines: BinaryHeap<Reverse<(Instant, u64)>>,
    /// Keeper threads alive: 0 while nothing is registered, otherwise 1.
    threads: usize,
    /// The entry whose jobs run outside the lock; deregistering it waits them out.
    busy: Option<u64>,
}

struct Shared {
    cadence: Cadence,
    state: Mutex<State>,
    changed: Condvar,
}

impl Shared {
    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }
}

/// One engine's keeper (`run.cancel.engine-keeper`): a single thread holding a deadline
/// heap of registered executions, polling each one's stop mark and renewing its leases on
/// the [`Cadence`], asleep until the earliest deadline or a registration change. The
/// thread starts on the first registration and exits when the last one drops. Clones
/// share one keeper.
#[derive(Clone)]
pub struct Keeper(Arc<Shared>);

impl Default for Keeper {
    fn default() -> Keeper {
        Keeper::new(Cadence::default())
    }
}

impl std::fmt::Debug for Keeper {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Keeper").field("cadence", &self.0.cadence).field("registered", &self.registered()).finish()
    }
}

impl Keeper {
    pub fn new(cadence: Cadence) -> Keeper {
        Keeper(Arc::new(Shared { cadence, state: Mutex::default(), changed: Condvar::new() }))
    }

    pub fn cadence(&self) -> Cadence {
        self.0.cadence
    }

    /// Executions registered now.
    pub fn registered(&self) -> usize {
        self.0.lock().entries.len()
    }

    /// Keeper threads alive now: 1 while anything is registered, 0 once the thread exits.
    pub fn threads(&self) -> usize {
        self.0.lock().threads
    }

    /// Register run `run_id`: poll once before returning, so the run's first await already
    /// sees a stop written before it opened, then poll and renew the run row's owner lease
    /// and the single-writer lease in `held` on the cadence until the registration drops.
    pub fn register(&self, catalog: Arc<dyn Catalog + Send + Sync>, run_id: &str, token: CancelToken, held: Arc<Mutex<Option<Lease>>>) -> Registration {
        poll(catalog.as_ref(), run_id, &token);
        let schedule = Schedule::new(self.0.cadence, Instant::now());
        let mut state = self.0.lock();
        let id = state.next_id;
        state.next_id += 1;
        state.deadlines.push(Reverse((schedule.next(), id)));
        state.entries.insert(id, Entry { catalog, run_id: run_id.to_string(), token, held, schedule });
        if state.threads == 0 {
            state.threads = 1;
            let shared = self.0.clone();
            std::thread::Builder::new()
                .name("contextful-keeper".into())
                .spawn(move || keep(&shared))
                .unwrap_or_else(|e| panic!("spawning the engine keeper thread: {e}"));
        }
        self.0.changed.notify_all();
        Registration { keeper: self.0.clone(), id }
    }
}

/// An execution's place in its engine's keeper; dropping it deregisters the execution,
/// waiting out a poll or renewal already running for it, so none runs after the drop.
pub struct Registration {
    keeper: Arc<Shared>,
    id: u64,
}

impl Drop for Registration {
    fn drop(&mut self) {
        let mut state = self.keeper.lock();
        state.entries.remove(&self.id);
        while state.busy == Some(self.id) {
            state = self.keeper.changed.wait(state).unwrap_or_else(|e| e.into_inner());
        }
        self.keeper.changed.notify_all();
    }
}

/// The keeper thread: run the earliest due entry's jobs outside the lock, sleep until the
/// next deadline, and exit once no entry is registered.
fn keep(shared: &Shared) {
    let mut state = shared.lock();
    loop {
        let top = loop {
            match state.deadlines.peek() {
                None => break None,
                Some(&Reverse((at, id))) if state.entries.get(&id).is_some_and(|e| e.schedule.next() == at) => break Some((at, id)),
                Some(_) => {
                    state.deadlines.pop();
                }
            }
        };
        let Some((at, id)) = top else {
            state.threads = 0;
            shared.changed.notify_all();
            return;
        };
        let now = Instant::now();
        if now < at {
            state = shared.changed.wait_timeout(state, at - now).unwrap_or_else(|e| e.into_inner()).0;
            continue;
        }
        state.deadlines.pop();
        let Some(entry) = state.entries.get_mut(&id) else { continue };
        let due = entry.schedule.take(now);
        let next = entry.schedule.next();
        let (catalog, run_id, token, held) = (entry.catalog.clone(), entry.run_id.clone(), entry.token.clone(), entry.held.clone());
        state.deadlines.push(Reverse((next, id)));
        state.busy = Some(id);
        drop(state);
        // A panicking job unwinds to here (`run.cancel.keeper-panic`), so `busy` clears and
        // the thread keeps serving every other registration.
        let jobs = std::panic::catch_unwind(AssertUnwindSafe(|| {
            if due.poll {
                poll(catalog.as_ref(), &run_id, &token);
            }
            if due.renew {
                renew(catalog.as_ref(), &run_id);
                renew_lease(catalog.as_ref(), &held);
            }
        }));
        if jobs.is_err() {
            eprintln!("warning: a keeper job of run `{run_id}` panicked; it retries at its next deadline");
        }
        state = shared.lock();
        state.busy = None;
        shared.changed.notify_all();
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
