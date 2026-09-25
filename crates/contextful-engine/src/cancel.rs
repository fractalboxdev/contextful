//! The run's one cancellation token and the keeper thread feeding it: one catalog read
//! before the run's first await, then one every 500 ms, with the owner lease renewed on
//! its own cadence.

use contextful_core::coordinate::{Catalog, Lease};
use contextful_core::run::cancel::POLL_INTERVAL_MS;
use contextful_core::run::ports::Cancellation;
use contextful_core::run::record::{OWNER_LEASE_RENEWAL_SECS, OWNER_LEASE_TTL_SECS};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

/// The token every await of one run selects on.
#[derive(Debug, Clone, Default)]
pub struct CancelToken(Arc<AtomicBool>);

impl CancelToken {
    pub fn fire(&self) {
        self.0.store(true, Ordering::SeqCst);
    }
}

impl Cancellation for CancelToken {
    fn requested(&self) -> bool {
        self.0.load(Ordering::SeqCst)
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

/// The background thread polling the stop mark and renewing the owner lease; it stops
/// when dropped.
pub struct Keeper {
    stop: Arc<AtomicBool>,
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
        let stop = Arc::new(AtomicBool::new(false));
        let (flag, id) = (stop.clone(), run_id.to_string());
        let handle = std::thread::spawn(move || {
            let tick = Duration::from_millis(10);
            let (mut since_poll, mut since_renew) = (Duration::ZERO, Duration::ZERO);
            while !flag.load(Ordering::SeqCst) {
                std::thread::sleep(tick);
                since_poll += tick;
                since_renew += tick;
                if since_poll >= cadence.poll {
                    since_poll = Duration::ZERO;
                    poll(catalog.as_ref(), &id, &token);
                }
                if since_renew >= cadence.renew {
                    since_renew = Duration::ZERO;
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
        self.stop.store(true, Ordering::SeqCst);
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
    }
}
