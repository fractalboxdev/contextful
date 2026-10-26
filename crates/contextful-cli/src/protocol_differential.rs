//! Seeded operation sequences compare the executable protocol model with the store's two real
//! lease paths after every step: the bucket compaction lease through `Syncer` over a scripted
//! object store, and the run lease through the local catalog and the commit log.

use anyhow::{bail, Context, Result};
use clap::Args as ClapArgs;
use proptest::prelude::{prop_oneof, Just, Strategy};
use proptest::strategy::ValueTree;
use proptest::test_runner::{Config, RngAlgorithm, TestRng, TestRunner};
use proptest_state_machine::ReferenceStateMachine;
use serde::{Deserialize, Serialize};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

const NODES: usize = 3;
const GENERATIONS: u64 = 4;
const STEPS_PER_CASE: usize = 32;
const DEFAULT_SEED: u64 = 76_004;
const REGRESSIONS: &str = "formal/protocol/regressions.jsonl";

#[derive(ClapArgs)]
pub struct Args {
    /// The generator seed, printed with the result.
    #[arg(long, default_value_t = DEFAULT_SEED)]
    seed: u64,
    /// Generated operation sequences after replaying regressions.
    #[arg(long, default_value_t = 64)]
    cases: usize,
    /// Print generated cases without running the model or the store.
    #[arg(long, hide = true)]
    dump_cases: bool,
    /// A prebuilt protocol executable; absent, lake builds the package.
    #[arg(long)]
    reference: Option<PathBuf>,
    /// The saved minimized sequences.
    #[arg(long)]
    regressions: Option<PathBuf>,
}

/// The lease paths every case replays on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum LeasePath {
    Compaction,
    Run,
}

impl LeasePath {
    const ALL: [LeasePath; 2] = [LeasePath::Compaction, LeasePath::Run];

    fn name(self) -> &'static str {
        match self {
            Self::Compaction => "compaction",
            Self::Run => "run",
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(tag = "op", rename_all = "snake_case")]
enum Step {
    Grant { node: usize },
    Raise { node: usize },
    Renew { node: usize },
    Expire,
    Release { node: usize },
    Send { node: usize },
    Deliver { node: usize },
    Pause { node: usize },
    Resume { node: usize },
    Crash { node: usize },
}

/// Every step kind the harness drives, as `protocol steps` lists the model's.
const KINDS: [&str; 10] = [
    "grant", "raise", "renew", "expire", "release", "send", "deliver", "pause", "resume", "crash",
];

impl Step {
    fn kind(self) -> &'static str {
        match self {
            Self::Grant { .. } => "grant",
            Self::Raise { .. } => "raise",
            Self::Renew { .. } => "renew",
            Self::Expire => "expire",
            Self::Release { .. } => "release",
            Self::Send { .. } => "send",
            Self::Deliver { .. } => "deliver",
            Self::Pause { .. } => "pause",
            Self::Resume { .. } => "resume",
            Self::Crash { .. } => "crash",
        }
    }

    fn line(self) -> String {
        match self {
            Self::Expire => self.kind().into(),
            Self::Grant { node }
            | Self::Raise { node }
            | Self::Renew { node }
            | Self::Release { node }
            | Self::Send { node }
            | Self::Deliver { node }
            | Self::Pause { node }
            | Self::Resume { node }
            | Self::Crash { node } => format!("{} {node}", self.kind()),
        }
    }
}

fn option<T: std::fmt::Display>(value: Option<T>) -> String {
    value
        .map(|v| format!("(some {v})"))
        .unwrap_or_else(|| "none".into())
}

/// One node's projected view, printed as the model prints `NodeState`.
#[derive(Clone, Copy, Default)]
struct View {
    belief: Option<u64>,
    raised: bool,
    paused: bool,
    pending: Option<u64>,
}

/// The state a driver projects after each step, printed as the model prints `State`.
struct Projection {
    /// The lease object's holder, fence and liveness; `None` before its first creation.
    lease: Option<(Option<usize>, u64, bool)>,
    fence: u64,
    head: u64,
    views: [View; NODES],
    writes: u64,
}

impl Projection {
    fn show(&self) -> String {
        let lease = match self.lease {
            None => "none".to_string(),
            Some((holder, fence, live)) => format!(
                "holder={} fence={fence} live={}",
                option(holder),
                live && holder.is_some()
            ),
        };
        let views = self
            .views
            .iter()
            .enumerate()
            .map(|(i, v)| {
                format!(
                    "n{i}(belief={} raised={} paused={} pending={})",
                    option(v.belief),
                    v.raised,
                    v.paused,
                    option(v.pending)
                )
            })
            .collect::<Vec<_>>()
            .join(" ");
        format!(
            "lease[{lease}] fence={} head={} {views} writes={}",
            self.fence, self.head, self.writes
        )
    }
}

/// A lease path driven through the store's own calls, one call per step.
trait Driver {
    fn apply(&mut self, step: Step) -> Result<()>;
    fn project(&self) -> Result<Projection>;
}

#[cfg(feature = "data-plane")]
mod drivers {
    //! The two real lease paths. Each driver plays the program around the store: it calls
    //! the store only where that program does, and reports what the store answered.

    use super::{Driver, Projection, Step, View, NODES};
    use anyhow::{bail, Context, Result};
    use contextful_context::{commit_log, ContextError, Store};
    use contextful_core::coordinate::{Catalog, Lease, LeaseKey};
    use contextful_core::ports::Clock;
    use contextful_core::store::commit_log::{CommitEntry, Kind};
    use contextful_core::store::lease::{BucketLease, BucketPointer, CLOCK_SKEW_SECS};
    use contextful_core::store::object::{Condition, ObjectError, ObjectStore, Put};
    use contextful_core::store::sync::SyncConfig;
    use contextful_core::store::StoreError;
    use contextful_core::time::Instant;
    use contextful_engine::LocalCatalog;
    use contextful_sync::sync::{Held, SyncError};
    use contextful_sync::{Redistribution, Syncer};
    use std::cell::Cell;
    use std::collections::BTreeMap;
    use std::sync::{Arc, Condvar, Mutex};
    use std::thread::JoinHandle;

    const PROJECT: &str = "protocol";
    const TABLE: &str = "protocol";
    const PREFIX: &str = "team";
    const LEASE_TTL_SECS: u64 = 600;

    fn node_id(n: usize) -> String {
        format!("node-{n}")
    }

    fn node_of(holder: Option<&str>) -> Result<Option<usize>> {
        holder
            .map(|h| {
                h.strip_prefix("node-")
                    .and_then(|n| n.parse().ok())
                    .with_context(|| format!("lease holder `{h}` names no protocol node"))
            })
            .transpose()
    }

    fn epoch() -> Instant {
        Instant::from_unix_secs(1_900_000_000).expect("the epoch is representable")
    }

    fn scratch_store(dir: &std::path::Path, node: &str) -> Result<Store> {
        let root = dir.join(".contextful/context").join(PROJECT);
        std::fs::create_dir_all(&root)?;
        std::fs::write(
            root.join("config.toml"),
            format!("[node]\nid = \"{node}\"\n"),
        )?;
        Ok(Store::open(dir, PROJECT)?)
    }

    thread_local! {
        /// The protocol node whose acquisition this thread runs; `None` on the driving thread.
        static ACTOR: Cell<Option<usize>> = const { Cell::new(None) };
    }

    /// Where a node's acquisition stands at the scripted bucket.
    #[derive(Clone, Copy, PartialEq, Eq)]
    enum Gate {
        /// Running toward the pointer write.
        Armed,
        /// Holding at the pointer write: the grant is written and the raise is not.
        Holding,
        /// The raise may write.
        Open,
        /// The node crashed: the pointer write fails.
        Abort,
        /// The acquisition returned.
        Done,
    }

    #[derive(Default)]
    struct Objects {
        objects: BTreeMap<String, (Vec<u8>, u64)>,
        etag: u64,
        applied: BTreeMap<String, u64>,
        gates: [Option<Gate>; NODES],
    }

    /// An in-memory bucket honouring every condition, which holds a node's acquisition at its
    /// pointer write until the driver releases it: the grant and the raise are two steps.
    #[derive(Default)]
    struct ScriptedBucket {
        state: Mutex<Objects>,
        wake: Condvar,
    }

    impl ScriptedBucket {
        fn lock(&self) -> std::sync::MutexGuard<'_, Objects> {
            self.state.lock().unwrap_or_else(|p| p.into_inner())
        }

        fn set_gate(&self, n: usize, gate: Option<Gate>) {
            self.lock().gates[n] = gate;
            self.wake.notify_all();
        }

        /// Wait until node `n`'s acquisition holds at its pointer write or returns.
        fn settle(&self, n: usize) -> Gate {
            let mut objects = self.lock();
            loop {
                match objects.gates[n] {
                    Some(g @ (Gate::Holding | Gate::Done)) => return g,
                    _ => objects = self.wake.wait(objects).unwrap_or_else(|p| p.into_inner()),
                }
            }
        }

        fn object(&self, key: &str) -> Option<Vec<u8>> {
            self.lock().objects.get(key).map(|(b, _)| b.clone())
        }

        fn applied(&self, key: &str) -> u64 {
            self.lock().applied.get(key).copied().unwrap_or(0)
        }
    }

    impl ObjectStore for ScriptedBucket {
        fn get(&self, key: &str) -> Result<Option<(Vec<u8>, String)>, ObjectError> {
            Ok(self
                .lock()
                .objects
                .get(key)
                .map(|(b, e)| (b.clone(), e.to_string())))
        }

        fn put(&self, key: &str, bytes: &[u8], condition: Condition) -> Result<Put, ObjectError> {
            let mut objects = self.lock();
            if let Some(n) = ACTOR
                .with(Cell::get)
                .filter(|_| key.ends_with(contextful_core::store::lay_out::POINTER_FILE))
            {
                if objects.gates[n] == Some(Gate::Armed) {
                    objects.gates[n] = Some(Gate::Holding);
                    self.wake.notify_all();
                }
                while objects.gates[n] == Some(Gate::Holding) {
                    objects = self.wake.wait(objects).unwrap_or_else(|p| p.into_inner());
                }
                if objects.gates[n] == Some(Gate::Abort) {
                    return Err(ObjectError::Transport(format!(
                        "node-{n} crashed before its raise"
                    )));
                }
            }
            let current = objects.objects.get(key).map(|(_, e)| e.to_string());
            let holds = match &condition {
                Condition::None => true,
                Condition::IfNoneMatch => current.is_none(),
                Condition::IfMatch(e) => current.as_deref() == Some(e.as_str()),
            };
            if !holds {
                return Ok(Put::ConditionFailed);
            }
            objects.etag += 1;
            let etag = objects.etag;
            objects
                .objects
                .insert(key.to_string(), (bytes.to_vec(), etag));
            *objects.applied.entry(key.to_string()).or_default() += 1;
            Ok(Put::Applied(etag.to_string()))
        }

        fn delete(&self, key: &str) -> Result<(), ObjectError> {
            self.lock().objects.remove(key);
            Ok(())
        }

        fn list(&self, prefix: &str) -> Result<Vec<String>, ObjectError> {
            Ok(self
                .lock()
                .objects
                .keys()
                .filter(|k| k.starts_with(prefix))
                .cloned()
                .collect())
        }
    }

    #[derive(Default)]
    struct CompactionNode {
        acquiring: Option<JoinHandle<Result<Held, SyncError>>>,
        /// The fence the grant wrote while the acquisition holds at its raise.
        granted: Option<u64>,
        held: Option<Held>,
        paused: bool,
        pending: Option<Held>,
    }

    /// The bucket compaction lease: `Syncer::acquire`, `publish` and `release` over a scripted
    /// bucket shared by three nodes, each with its own store.
    pub(super) struct Compaction {
        _dirs: Vec<tempfile::TempDir>,
        bucket: Arc<ScriptedBucket>,
        syncers: Vec<Arc<Syncer>>,
        nodes: [CompactionNode; NODES],
        now: Instant,
    }

    impl Compaction {
        pub(super) fn new() -> Result<Compaction> {
            let bucket = Arc::new(ScriptedBucket::default());
            let (mut dirs, mut syncers) = (Vec::new(), Vec::new());
            for n in 0..NODES {
                let dir = tempfile::tempdir()?;
                let store = scratch_store(dir.path(), &node_id(n))?;
                syncers.push(Arc::new(Syncer {
                    store,
                    bucket: bucket.clone(),
                    config: SyncConfig {
                        endpoint: "memory://protocol".into(),
                        bucket: "protocol".into(),
                        prefix: Some(PREFIX.into()),
                        coordination: Some("cas".into()),
                        ..SyncConfig::default()
                    },
                    prefix: PREFIX.into(),
                    project: PROJECT.into(),
                    node: node_id(n),
                    residency: None,
                    control_dir: None,
                    signer: None,
                    key_set: None,
                    redistribution: Redistribution::default(),
                }));
                dirs.push(dir);
            }
            Ok(Compaction {
                _dirs: dirs,
                bucket,
                syncers,
                nodes: Default::default(),
                now: epoch(),
            })
        }

        fn lease_key() -> String {
            format!(
                "{PREFIX}/{}",
                contextful_core::store::lease::compaction_key(PROJECT, TABLE)
            )
        }

        fn pointer_key() -> String {
            format!(
                "{PREFIX}/{PROJECT}/tables/{TABLE}/{}",
                contextful_core::store::lay_out::POINTER_FILE
            )
        }

        fn lease(&self) -> Result<Option<BucketLease>> {
            self.bucket
                .object(&Self::lease_key())
                .map(|b| serde_json::from_slice(&b).context("the lease object"))
                .transpose()
        }

        /// Wait for node `n`'s acquisition to return, and record what it returned.
        fn finish(&mut self, n: usize) -> Result<()> {
            let Some(handle) = self.nodes[n].acquiring.take() else {
                return Ok(());
            };
            let result = handle
                .join()
                .map_err(|_| anyhow::anyhow!("node-{n}'s acquisition panicked"))?;
            self.bucket.set_gate(n, None);
            self.nodes[n].granted = None;
            match result {
                Ok(held) => self.nodes[n].held = Some(held),
                Err(SyncError::Store(StoreError::LeaseHeld(_))) => {}
                // A crash before the raise fails the pointer write.
                Err(SyncError::Object(ObjectError::Transport(_))) => {}
                Err(e) => bail!("node-{n} acquire: {e}"),
            }
            Ok(())
        }
    }

    impl Driver for Compaction {
        fn apply(&mut self, step: Step) -> Result<()> {
            match step {
                Step::Grant { node } => {
                    let v = &self.nodes[node];
                    if v.paused || v.acquiring.is_some() {
                        return Ok(());
                    }
                    // The scheduler takes no lease its own node id holds unexpired.
                    if self.lease()?.is_some_and(|l| {
                        l.holder.as_deref() == Some(node_id(node).as_str())
                            && l.held_against("", self.now)
                    }) {
                        return Ok(());
                    }
                    self.bucket.set_gate(node, Some(Gate::Armed));
                    let (syncer, now) = (self.syncers[node].clone(), self.now);
                    let bucket = self.bucket.clone();
                    let handle = std::thread::spawn(move || {
                        ACTOR.with(|a| a.set(Some(node)));
                        let result = syncer.acquire(TABLE, now);
                        let mut objects = bucket.lock();
                        objects.gates[node] = Some(Gate::Done);
                        bucket.wake.notify_all();
                        result
                    });
                    self.nodes[node].acquiring = Some(handle);
                    match self.bucket.settle(node) {
                        Gate::Holding => {
                            self.nodes[node].granted = self.lease()?.map(|l| l.fence);
                            self.nodes[node].held = None;
                        }
                        _ => self.finish(node)?,
                    }
                }
                Step::Raise { node } => {
                    if !self.nodes[node].paused && self.nodes[node].acquiring.is_some() {
                        self.bucket.set_gate(node, Some(Gate::Open));
                        self.finish(node)?;
                    }
                }
                // `Syncer` renews no compaction lease: a grant lives its time to live.
                Step::Renew { .. } => {}
                Step::Expire => {
                    if let Some(expiry) = self.lease()?.and_then(|l| l.expires_at) {
                        let lapsed = expiry.plus_secs(CLOCK_SKEW_SECS + 1);
                        if self.now < lapsed {
                            self.now = lapsed;
                        }
                    }
                }
                Step::Release { node } => {
                    if !self.nodes[node].paused && self.nodes[node].held.take().is_some() {
                        match self.syncers[node].release(TABLE) {
                            Ok(()) | Err(SyncError::Store(StoreError::LeaseNotHeld(_))) => {}
                            Err(e) => bail!("node-{node} release: {e}"),
                        }
                    }
                }
                Step::Send { node } => {
                    let v = &mut self.nodes[node];
                    if !v.paused && v.pending.is_none() {
                        v.pending = v.held.clone();
                    }
                }
                Step::Deliver { node } => {
                    if let Some(held) = self.nodes[node].pending.take() {
                        let snapshot = format!("snapshot-{}", held.lease.fence);
                        match self.syncers[node].publish(TABLE, &snapshot, &held) {
                            Ok(()) | Err(SyncError::Store(StoreError::LeaseFenced(_))) => {}
                            Err(e) => bail!("node-{node} publish: {e}"),
                        }
                    }
                }
                Step::Pause { node } => self.nodes[node].paused = true,
                Step::Resume { node } => self.nodes[node].paused = false,
                Step::Crash { node } => {
                    if self.nodes[node].acquiring.is_some() {
                        self.bucket.set_gate(node, Some(Gate::Abort));
                    }
                    self.nodes[node].paused = false;
                    self.finish(node)?;
                    self.nodes[node].held = None;
                }
            }
            Ok(())
        }

        fn project(&self) -> Result<Projection> {
            let lease = self.lease()?.map(|l| -> Result<_> {
                let live = l
                    .expires_at
                    .is_some_and(|e| self.now < e.plus_secs(CLOCK_SKEW_SECS));
                Ok((node_of(l.holder.as_deref())?, l.fence, live))
            });
            let pointer: BucketPointer = match self.bucket.object(&Self::pointer_key()) {
                Some(b) => serde_json::from_slice(&b).context("the table pointer")?,
                None => BucketPointer::default(),
            };
            let head = match pointer.snapshot_id.as_deref() {
                Some(id) => id
                    .strip_prefix("snapshot-")
                    .and_then(|f| f.parse().ok())
                    .with_context(|| format!("snapshot `{id}` names no fence"))?,
                None => 0,
            };
            let views = std::array::from_fn(|n| {
                let v = &self.nodes[n];
                View {
                    belief: v.granted.or(v.held.as_ref().map(|h| h.lease.fence)),
                    raised: v.held.is_some(),
                    paused: v.paused,
                    pending: v.pending.as_ref().map(|h| h.lease.fence),
                }
            });
            Ok(Projection {
                lease: lease.transpose()?,
                fence: pointer.fence,
                head,
                views,
                writes: self.bucket.applied(&Self::pointer_key()),
            })
        }
    }

    impl Drop for Compaction {
        fn drop(&mut self) {
            for n in 0..NODES {
                if self.nodes[n].acquiring.is_some() {
                    self.bucket.set_gate(n, Some(Gate::Abort));
                    let _ = self.nodes[n].acquiring.take().map(JoinHandle::join);
                }
            }
        }
    }

    /// A catalog clock the driver moves: expiry is a step.
    struct StepClock(Mutex<Instant>);

    impl Clock for StepClock {
        fn now(&self) -> Instant {
            *self.0.lock().unwrap_or_else(|p| p.into_inner())
        }
    }

    #[derive(Default)]
    struct RunNode {
        lease: Option<Lease>,
        raised: bool,
        paused: bool,
        pending: Option<u64>,
    }

    /// The run lease: the runner's catalog row through `LocalCatalog`, its acquisition entry
    /// through `commit_log::open_fence`, the commit check through `lease_holds`, and the commit
    /// point through `commit_log::append`, three runs contending on one machine.
    pub(super) struct Run {
        _dir: tempfile::TempDir,
        store: Store,
        catalog: LocalCatalog,
        clock: Arc<StepClock>,
        nodes: [RunNode; NODES],
    }

    impl Run {
        pub(super) fn new() -> Result<Run> {
            let dir = tempfile::tempdir()?;
            let store = scratch_store(dir.path(), "machine")?;
            let clock = Arc::new(StepClock(Mutex::new(epoch())));
            let catalog = LocalCatalog::open(&dir.path().join("catalog"), clock.clone());
            Ok(Run {
                _dir: dir,
                store,
                catalog,
                clock,
                nodes: Default::default(),
            })
        }

        fn key() -> LeaseKey {
            LeaseKey::Pipeline(format!("{PROJECT}/{TABLE}"))
        }

        fn log(&self) -> Result<Vec<CommitEntry>> {
            Ok(commit_log::read(&self.store, PROJECT, "machine")?)
        }

        /// The runner closes a failed run and releases its row.
        fn abandon(&mut self, node: usize) -> Result<()> {
            if let Some(lease) = self.nodes[node].lease.take() {
                self.catalog
                    .release(&lease)
                    .map_err(|f| anyhow::anyhow!("release: {f}"))?;
            }
            self.nodes[node].raised = false;
            Ok(())
        }
    }

    fn fenced(e: &ContextError) -> bool {
        matches!(e, ContextError::Store(StoreError::LeaseFenced(_)))
    }

    impl Driver for Run {
        fn apply(&mut self, step: Step) -> Result<()> {
            let failure = |f: contextful_core::run::Failure| anyhow::anyhow!("catalog: {f}");
            match step {
                Step::Grant { node } => {
                    let v = &self.nodes[node];
                    if v.paused || (v.lease.is_some() && !v.raised) {
                        return Ok(());
                    }
                    if let Some(lease) = self
                        .catalog
                        .acquire(&Self::key(), &node_id(node), LEASE_TTL_SECS)
                        .map_err(failure)?
                    {
                        self.nodes[node].lease = Some(lease);
                        self.nodes[node].raised = false;
                    }
                }
                Step::Raise { node } => {
                    let v = &self.nodes[node];
                    if let (false, false, Some(lease)) = (v.paused, v.raised, &v.lease) {
                        match commit_log::open_fence(
                            &self.store,
                            PROJECT,
                            "machine",
                            TABLE,
                            lease.fence,
                        ) {
                            Ok(_) => self.nodes[node].raised = true,
                            Err(e) if fenced(&e) => self.abandon(node)?,
                            Err(e) => bail!("node-{node} open_fence: {e}"),
                        }
                    }
                }
                Step::Renew { node } => {
                    let v = &self.nodes[node];
                    if let (false, true, Some(lease)) = (v.paused, v.raised, &v.lease) {
                        if let Some(renewed) =
                            self.catalog.renew(lease, LEASE_TTL_SECS).map_err(failure)?
                        {
                            self.nodes[node].lease = Some(renewed);
                        }
                    }
                }
                Step::Expire => {
                    if let Some(expiry) = self
                        .catalog
                        .lease_row(&Self::key())
                        .map_err(failure)?
                        .expires_at
                    {
                        let mut now = self.clock.0.lock().unwrap_or_else(|p| p.into_inner());
                        if *now < expiry.plus_secs(1) {
                            *now = expiry.plus_secs(1);
                        }
                    }
                }
                Step::Release { node } => {
                    if !self.nodes[node].paused && self.nodes[node].raised {
                        self.abandon(node)?;
                    }
                }
                Step::Send { node } => {
                    let v = &self.nodes[node];
                    if let (false, true, None, Some(lease)) =
                        (v.paused, v.raised, v.pending, &v.lease)
                    {
                        if self.catalog.lease_holds(lease).map_err(failure)? {
                            self.nodes[node].pending = Some(lease.fence);
                        } else {
                            self.abandon(node)?;
                        }
                    }
                }
                Step::Deliver { node } => {
                    if let Some(fence) = self.nodes[node].pending.take() {
                        let entry = CommitEntry {
                            kind: Kind::Commit,
                            table: TABLE.into(),
                            run_id: Some(node_id(node)),
                            cursor: None,
                            cursor_kind: None,
                            fence,
                        };
                        match commit_log::append(&self.store, PROJECT, "machine", &entry) {
                            Ok(_) => {}
                            Err(e) if fenced(&e) => {}
                            Err(e) => bail!("node-{node} commit: {e}"),
                        }
                    }
                }
                Step::Pause { node } => self.nodes[node].paused = true,
                Step::Resume { node } => self.nodes[node].paused = false,
                Step::Crash { node } => {
                    let v = &mut self.nodes[node];
                    v.lease = None;
                    v.raised = false;
                    v.paused = false;
                }
            }
            Ok(())
        }

        fn project(&self) -> Result<Projection> {
            let row = self
                .catalog
                .lease_row(&Self::key())
                .map_err(|f| anyhow::anyhow!("catalog: {f}"))?;
            let now = self.clock.now();
            let lease = if row.fence == 0 {
                None
            } else {
                Some((
                    node_of(row.holder.as_deref())?,
                    row.fence,
                    row.expires_at.is_some_and(|e| now < e),
                ))
            };
            let log = self.log()?;
            let fence = log.iter().map(|e| e.fence).max().unwrap_or(0);
            let head = log
                .iter()
                .rev()
                .find(|e| e.kind == Kind::Commit)
                .map_or(0, |e| e.fence);
            let views = std::array::from_fn(|n| {
                let v = &self.nodes[n];
                View {
                    belief: v.lease.as_ref().map(|l| l.fence),
                    raised: v.raised,
                    paused: v.paused,
                    pending: v.pending,
                }
            });
            Ok(Projection {
                lease,
                fence,
                head,
                views,
                writes: log.len() as u64,
            })
        }
    }
}

#[cfg(feature = "data-plane")]
fn driver(path: LeasePath) -> Result<Box<dyn Driver>> {
    Ok(match path {
        LeasePath::Compaction => Box::new(drivers::Compaction::new()?),
        LeasePath::Run => Box::new(drivers::Run::new()?),
    })
}

#[cfg(not(feature = "data-plane"))]
fn driver(_: LeasePath) -> Result<Box<dyn Driver>> {
    Err(crate::ProfileError::ProfileCapabilityAbsent(format!(
        "`formal protocol-differential` drives the run path, which `{}` does not link; run it on a `contextful-full` build",
        crate::PROFILE
    ))
    .into())
}

#[derive(Clone, Copy, Debug, Default)]
struct GenerationNode {
    belief: Option<u64>,
    raised: bool,
    paused: bool,
}

#[derive(Clone, Debug, Default)]
struct GenerationState {
    fence: u64,
    holder: Option<usize>,
    live: bool,
    nodes: [GenerationNode; NODES],
}

/// The generator's coarse view of the protocol, enough to bound grants to the model's lease
/// generations; the model decides every step.
struct ProtocolGenerator;

impl ReferenceStateMachine for ProtocolGenerator {
    type State = GenerationState;
    type Transition = Step;

    fn init_state() -> proptest::strategy::BoxedStrategy<Self::State> {
        Just(GenerationState::default()).boxed()
    }

    fn transitions(_state: &Self::State) -> proptest::strategy::BoxedStrategy<Self::Transition> {
        prop_oneof![
            (0..NODES).prop_map(|node| Step::Grant { node }),
            (0..NODES).prop_map(|node| Step::Raise { node }),
            (0..NODES).prop_map(|node| Step::Renew { node }),
            Just(Step::Expire),
            (0..NODES).prop_map(|node| Step::Release { node }),
            (0..NODES).prop_map(|node| Step::Send { node }),
            (0..NODES).prop_map(|node| Step::Deliver { node }),
            (0..NODES).prop_map(|node| Step::Pause { node }),
            (0..NODES).prop_map(|node| Step::Resume { node }),
            (0..NODES).prop_map(|node| Step::Crash { node }),
        ]
        .boxed()
    }

    fn preconditions(state: &Self::State, transition: &Self::Transition) -> bool {
        !matches!(transition, Step::Grant { .. }) || state.fence < GENERATIONS
    }

    fn apply(mut state: Self::State, transition: &Self::Transition) -> Self::State {
        match *transition {
            Step::Grant { node }
                if !state.nodes[node].paused
                    && !(state.nodes[node].belief.is_some() && !state.nodes[node].raised)
                    && (state.holder.is_none() || !state.live) =>
            {
                state.fence += 1;
                state.holder = Some(node);
                state.live = true;
                state.nodes[node] = GenerationNode {
                    belief: Some(state.fence),
                    raised: false,
                    paused: false,
                };
            }
            Step::Raise { node }
                if !state.nodes[node].paused && state.nodes[node].belief.is_some() =>
            {
                state.nodes[node].raised = true
            }
            Step::Renew { node } if state.holder == Some(node) => state.live = true,
            Step::Expire => state.live = false,
            Step::Release { node } if !state.nodes[node].paused && state.nodes[node].raised => {
                if state.holder == Some(node) {
                    state.holder = None;
                }
                state.nodes[node].belief = None;
                state.nodes[node].raised = false;
            }
            Step::Pause { node } => state.nodes[node].paused = true,
            Step::Resume { node } => state.nodes[node].paused = false,
            Step::Crash { node } => state.nodes[node] = GenerationNode::default(),
            _ => {}
        }
        state
    }
}

fn generator(seed: u64) -> TestRunner {
    let bytes = seed.to_le_bytes().repeat(4);
    TestRunner::new_with_rng(
        Config::default(),
        TestRng::from_seed(RngAlgorithm::ChaCha, &bytes),
    )
}

fn generate(runner: &mut TestRunner) -> Result<Vec<Step>> {
    let strategy = ProtocolGenerator::sequential_strategy(STEPS_PER_CASE);
    let tree = strategy
        .new_tree(runner)
        .map_err(|e| anyhow::anyhow!("generating a protocol case: {e}"))?;
    Ok(tree.current().1)
}

/// The model's step kinds equal the harness's, or the run raises `ProtocolVocabularyDrift`.
fn check_vocabulary(reference: &Path) -> Result<()> {
    let out = Command::new(reference)
        .arg("steps")
        .output()
        .with_context(|| format!("starting {}", reference.display()))?;
    let listed = String::from_utf8_lossy(&out.stdout);
    let mut model: Vec<&str> = listed
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .collect();
    let mut harness = KINDS.to_vec();
    model.sort_unstable();
    harness.sort_unstable();
    if !out.status.success() || model != harness {
        bail!(
            "ProtocolVocabularyDrift: the model steps are [{}] and the harness steps are [{}]",
            model.join(", "),
            harness.join(", ")
        );
    }
    Ok(())
}

/// The first step after which the model and the store disagree on `path`: its index and both
/// printed states.
fn compare(
    reference: &Path,
    path: LeasePath,
    steps: &[Step],
) -> Result<Option<(usize, String, String)>> {
    let mut child = Command::new(reference)
        .args(["run", path.name()])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .with_context(|| format!("starting {}", reference.display()))?;
    {
        let stdin = child.stdin.as_mut().context("protocol stdin")?;
        for step in steps {
            writeln!(stdin, "{}", step.line())?;
        }
    }
    let out = child.wait_with_output()?;
    if !out.status.success() {
        bail!("protocol model: {}", String::from_utf8_lossy(&out.stderr));
    }
    let text = String::from_utf8(out.stdout)?;
    let states: Vec<_> = text.lines().collect();
    if states.len() != steps.len() {
        bail!(
            "protocol model returned {} states for {} steps",
            states.len(),
            steps.len()
        );
    }
    let mut store = driver(path)?;
    for (i, (step, model)) in steps.iter().zip(states).enumerate() {
        store.apply(*step)?;
        let actual = store.project()?.show();
        if model != actual {
            return Ok(Some((i, model.into(), actual)));
        }
    }
    Ok(None)
}

fn minimize(reference: &Path, path: LeasePath, mut steps: Vec<Step>) -> Result<Vec<Step>> {
    let mut i = 0;
    while i < steps.len() && steps.len() > 1 {
        let mut candidate = steps.clone();
        candidate.remove(i);
        if compare(reference, path, &candidate)?.is_some() {
            steps = candidate;
            i = 0;
        } else {
            i += 1;
        }
    }
    Ok(steps)
}

pub fn run(args: Args) -> Result<()> {
    let mut runner = generator(args.seed);
    if args.dump_cases {
        for _ in 0..args.cases {
            println!("{}", serde_json::to_string(&generate(&mut runner)?)?);
        }
        return Ok(());
    }
    let cwd = std::env::current_dir()?;
    let repo = Command::new("git")
        .args(["rev-parse", "--show-toplevel"])
        .current_dir(&cwd)
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| PathBuf::from(String::from_utf8_lossy(&o.stdout).trim()))
        .unwrap_or(cwd);
    let reference = match args.reference {
        Some(path) => path,
        None => {
            let root = repo.join("formal/protocol");
            let out = Command::new("lake")
                .arg("build")
                .current_dir(&root)
                .output()
                .context("building protocol model")?;
            if !out.status.success() {
                bail!(
                    "building protocol model: {}",
                    String::from_utf8_lossy(&out.stderr)
                );
            }
            root.join(".lake/build/bin/protocol")
        }
    };
    check_vocabulary(&reference)?;
    let regressions = args.regressions.unwrap_or_else(|| repo.join(REGRESSIONS));
    let saved = if regressions.exists() {
        std::fs::read_to_string(&regressions)?
    } else {
        String::new()
    };
    let mut cases = Vec::new();
    for line in saved.lines().filter(|line| !line.trim().is_empty()) {
        cases.push(serde_json::from_str::<Vec<Step>>(line)?);
    }
    let replayed = cases.len();
    for _ in 0..args.cases {
        cases.push(generate(&mut runner)?);
    }
    for (case, steps) in cases.into_iter().enumerate() {
        for path in LeasePath::ALL {
            if compare(&reference, path, &steps)?.is_none() {
                continue;
            }
            let reduced = minimize(&reference, path, steps)?;
            let (at, model, store) = compare(&reference, path, &reduced)?
                .context("the minimized protocol sequence no longer reproduces the drift")?;
            if let Some(parent) = regressions.parent() {
                std::fs::create_dir_all(parent)?;
            }
            let entry = serde_json::to_string(&reduced)?;
            if !saved.lines().any(|line| line == entry) {
                let mut file = std::fs::OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(&regressions)?;
                writeln!(file, "{entry}")?;
            }
            bail!(
                "ProtocolConformanceDrift: seed={} case={case} path={} step={at}; sequence={entry}; model={model}; store={store}",
                args.seed,
                path.name()
            );
        }
    }
    println!(
        "stale-fence-differential: 0 drift cases; seed={}; generated={}; regressions={replayed}; paths={}",
        args.seed,
        args.cases,
        LeasePath::ALL.map(LeasePath::name).join(",")
    );
    Ok(())
}
