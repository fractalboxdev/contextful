//! The in-process run projection hub: the runner emits deltas after durable state
//! changes, an observer folds them into per-run snapshots, and subscribers read a
//! per-run broadcast ring. Execution never reads the hub (`run.project.best-effort`).

use contextful_core::run::project::{reduce, Change, Coalescer, Delta, Snapshot, Version};
use contextful_core::run::record::RunRow;
use contextful_core::time::Instant;
use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{sync_channel, Receiver, SyncSender, TrySendError};
use std::sync::{Arc, Mutex, MutexGuard};

/// Snapshots the per-run broadcast holds: 256 entries (`run.project.broadcast-ring`).
pub const BROADCAST_RING_ENTRIES: usize = 256;
/// Seconds a run's channel outlives its folded terminal event (`run.project.channel-eviction`).
pub const CHANNEL_EVICTION_SECS: u64 = 60;
/// Deltas the emission channel buffers before it drops.
pub const EMISSION_CHANNEL_CAPACITY: usize = 1024;

struct RunChannel {
    snapshot: Snapshot,
    coalescer: Coalescer,
    ring: VecDeque<(u64, Snapshot)>,
    /// Sequence number the next broadcast takes.
    head: u64,
    /// The pump that first saw the snapshot terminal.
    terminal_at: Option<Instant>,
}

impl RunChannel {
    fn broadcast(&mut self, snapshot: Snapshot) {
        if self.ring.len() == BROADCAST_RING_ENTRIES {
            self.ring.pop_front();
        }
        self.ring.push_back((self.head, snapshot));
        self.head += 1;
    }
}

#[derive(Default)]
struct State {
    runs: HashMap<String, RunChannel>,
    /// Terminal deltas, one per run, never dropped.
    terminal: HashMap<String, Delta>,
    dropped: u64,
}

struct Shared {
    epoch: Instant,
    counter: AtomicU64,
    state: Mutex<State>,
}

impl Shared {
    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn next_version(&self) -> Version {
        Version { epoch: self.epoch, counter: self.counter.fetch_add(1, Ordering::Relaxed) + 1 }
    }
}

/// The hub: owns the observer end of the emission channel and every run's snapshot.
/// A restart builds a new hub, which holds no snapshot (`run.project.restart-discards`).
pub struct Hub {
    shared: Arc<Shared>,
    rx: Receiver<Delta>,
    tx: SyncSender<Delta>,
}

/// The runner's handle. Emission is no journal step and never blocks.
#[derive(Clone)]
pub struct Emitter {
    shared: Arc<Shared>,
    tx: SyncSender<Delta>,
}

/// One item a subscriber reads.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Update {
    Snapshot(Snapshot),
    /// The subscriber overran the ring; this is the latest snapshot to resume from.
    Resync(Snapshot),
}

/// A subscriber's position in one run's broadcast ring.
pub struct Subscription {
    shared: Arc<Shared>,
    run_id: String,
    next: u64,
}

impl Hub {
    /// A hub whose versions carry `epoch`, its start instant.
    pub fn new(epoch: Instant) -> Hub {
        Hub::with_capacity(epoch, EMISSION_CHANNEL_CAPACITY)
    }

    pub fn with_capacity(epoch: Instant, capacity: usize) -> Hub {
        let (tx, rx) = sync_channel(capacity);
        let shared = Arc::new(Shared { epoch, counter: AtomicU64::new(0), state: Mutex::new(State::default()) });
        Hub { shared, rx, tx }
    }

    pub fn emitter(&self) -> Emitter {
        Emitter { shared: self.shared.clone(), tx: self.tx.clone() }
    }

    /// Deltas the emission channel dropped while full.
    pub fn dropped(&self) -> u64 {
        self.shared.lock().dropped
    }

    /// Fold every queued delta, then every terminal slot, broadcasting through each
    /// run's coalescer at `now`, and flush any held snapshot whose window closed.
    pub fn pump(&self, now: Instant) {
        let mut deltas: Vec<Delta> = self.rx.try_iter().collect();
        let mut st = self.shared.lock();
        deltas.extend(st.terminal.drain().map(|(_, d)| d));
        // Versions are issued at emission, so folding in version order places a terminal
        // slot exactly where the runner emitted it.
        deltas.sort_by_key(|d| d.version);
        for d in deltas {
            fold(&mut st, &d, now);
        }
        for ch in st.runs.values_mut() {
            if let Some(s) = ch.coalescer.poll(now) {
                ch.broadcast(s);
            }
            if ch.terminal_at.is_none() && ch.snapshot.is_terminal() {
                ch.terminal_at = Some(now);
            }
        }
        // A terminal channel lives 60 s past its terminal event (`run.project.channel-eviction`).
        st.runs.retain(|_, ch| ch.terminal_at.is_none_or(|t| now < t.plus_secs(CHANNEL_EVICTION_SECS)));
    }

    /// The folded snapshot of a run and a subscription starting right after it, taken
    /// under one lock, so the subscriber misses and duplicates no update
    /// (`run.project.connect`). `None` for a run this hub has not seen.
    pub fn connect(&self, run_id: &str) -> Option<(Snapshot, Subscription)> {
        let st = self.shared.lock();
        let ch = st.runs.get(run_id)?;
        let sub = Subscription { shared: self.shared.clone(), run_id: run_id.to_string(), next: ch.head };
        Some((ch.snapshot.clone(), sub))
    }

    /// A subscription starting at the oldest snapshot the run's ring holds, so a late
    /// joiner reads each held snapshot in order before live updates
    /// (`run.project.catch-up`). `None` for a run this hub has not seen.
    pub fn catch_up(&self, run_id: &str) -> Option<Subscription> {
        let st = self.shared.lock();
        let ch = st.runs.get(run_id)?;
        let next = ch.ring.front().map_or(ch.head, |(seq, _)| *seq);
        Some(Subscription { shared: self.shared.clone(), run_id: run_id.to_string(), next })
    }

    /// Connect, recovering a run this hub has not seen from its durable record.
    pub fn connect_or_recover(&self, record: &RunRow) -> (Snapshot, Subscription) {
        if let Some(c) = self.connect(&record.run_id) {
            return c;
        }
        let mut st = self.shared.lock();
        let version = self.shared.next_version();
        let ch = st.runs.entry(record.run_id.clone()).or_insert_with(|| {
            let mut snapshot = Snapshot::new(&record.run_id, &record.pipeline_id, version);
            snapshot.status = record.status;
            snapshot.started_at = Some(record.started_at);
            snapshot.ended_at = record.ended_at;
            RunChannel { snapshot, coalescer: Coalescer::default(), ring: VecDeque::new(), head: 0, terminal_at: None }
        });
        let sub = Subscription { shared: self.shared.clone(), run_id: record.run_id.clone(), next: ch.head };
        (ch.snapshot.clone(), sub)
    }

    /// Hold a projection to its durable record: a snapshot reading non-terminal behind
    /// a terminal row takes the row's status and broadcasts at once
    /// (`run.project.terminal-slot`).
    pub fn reconcile(&self, record: &RunRow, now: Instant) {
        if !record.status.is_terminal() {
            return;
        }
        let mut st = self.shared.lock();
        let behind = st.runs.get(&record.run_id).is_some_and(|ch| !ch.snapshot.is_terminal());
        if behind {
            let d = Delta {
                run_id: record.run_id.clone(),
                workflow_id: record.pipeline_id.clone(),
                version: self.shared.next_version(),
                change: Change::Status { status: record.status, at: record.ended_at, error: None },
            };
            fold(&mut st, &d, now);
        }
    }
}

fn fold(st: &mut State, d: &Delta, now: Instant) {
    let ch = st.runs.entry(d.run_id.clone()).or_insert_with(|| RunChannel {
        snapshot: Snapshot::new(&d.run_id, &d.workflow_id, Version { epoch: d.version.epoch, counter: 0 }),
        coalescer: Coalescer::default(),
        ring: VecDeque::new(),
        head: 0,
        terminal_at: None,
    });
    // A refused metadata write leaves the snapshot as it stood; the projection is best-effort.
    if reduce(&mut ch.snapshot, d).is_ok() {
        if let Some(s) = ch.coalescer.offer(ch.snapshot.clone(), now) {
            ch.broadcast(s);
        }
    }
}

impl Emitter {
    /// Emit a change to a run. A full channel drops the delta; the call never blocks.
    /// A terminal status bypasses the channel through the run's terminal slot.
    pub fn emit(&self, run_id: &str, workflow_id: &str, change: Change) {
        let d = Delta { run_id: run_id.to_string(), workflow_id: workflow_id.to_string(), version: self.shared.next_version(), change };
        if d.is_terminal() {
            self.shared.lock().terminal.insert(d.run_id.clone(), d);
            return;
        }
        match self.tx.try_send(d) {
            Ok(()) => {}
            Err(TrySendError::Full(_)) | Err(TrySendError::Disconnected(_)) => self.shared.lock().dropped += 1,
        }
    }
}

impl Subscription {
    /// The next update, or `None` when the subscriber is current.
    pub fn recv(&mut self) -> Option<Update> {
        let st = self.shared.lock();
        let ch = st.runs.get(&self.run_id)?;
        let oldest = ch.ring.front().map(|(seq, _)| *seq)?;
        if self.next < oldest {
            self.next = ch.head;
            return Some(Update::Resync(ch.snapshot.clone()));
        }
        let (seq, s) = ch.ring.iter().find(|(seq, _)| *seq == self.next)?;
        self.next = seq + 1;
        Some(Update::Snapshot(s.clone()))
    }
}
