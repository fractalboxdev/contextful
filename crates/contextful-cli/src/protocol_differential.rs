//! Seeded operation sequences compare the executable protocol model with the store's
//! lease, pointer and commit-log decisions after every step.

use anyhow::{bail, Context, Result};
use clap::Args as ClapArgs;
use contextful_core::store::commit_log::{self, CommitEntry, Kind};
use contextful_core::store::lease::{BucketLease, BucketPointer};
use contextful_core::store::object::Condition;
use contextful_core::time::Instant;
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

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum Target {
    Catalog,
    Cursor,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(tag = "op", rename_all = "snake_case")]
enum Step {
    Acquire { node: usize },
    Renew { node: usize },
    Expire,
    Release { node: usize },
    Send { node: usize, target: Target },
    Deliver { node: usize },
    Pause { node: usize },
    Resume { node: usize },
    Crash { node: usize },
}

impl Step {
    fn line(self) -> String {
        match self {
            Self::Acquire { node } => format!("acquire {node}"),
            Self::Renew { node } => format!("renew {node}"),
            Self::Expire => "expire".into(),
            Self::Release { node } => format!("release {node}"),
            Self::Send { node, target } => format!("send {node} {}", target.name()),
            Self::Deliver { node } => format!("deliver {node}"),
            Self::Pause { node } => format!("pause {node}"),
            Self::Resume { node } => format!("resume {node}"),
            Self::Crash { node } => format!("crash {node}"),
        }
    }
}

impl Target {
    fn name(self) -> &'static str {
        match self {
            Self::Catalog => "catalog",
            Self::Cursor => "cursor",
        }
    }
}

#[derive(Clone, Default)]
struct Node {
    belief: Option<u64>,
    paused: bool,
    pending: Option<(Target, u64)>,
}

// mirrors: assurance.model.protocol-model
/// The conditional store keeps an ETag beside each lease generation. The model projects
/// the holder and fence; an unexpected condition failure remains a harness error.
struct Store {
    lease: Option<BucketLease>,
    live: bool,
    etag: u64,
    catalog_etag: u64,
    cursor_etag: u64,
    catalog: BucketPointer,
    cursor: Vec<CommitEntry>,
    nodes: [Node; NODES],
    now: Instant,
}

impl Store {
    fn new() -> Self {
        Self {
            lease: None,
            live: false,
            etag: 0,
            catalog_etag: 0,
            cursor_etag: 0,
            catalog: BucketPointer::default(),
            cursor: Vec::new(),
            nodes: std::array::from_fn(|_| Node::default()),
            now: Instant::from_unix_secs(0).expect("epoch is representable"),
        }
    }

    fn cursor_fence(&self) -> u64 {
        self.cursor.iter().map(|e| e.fence).max().unwrap_or(0)
    }
    fn live(&self) -> bool {
        self.live
    }
    fn etag_text(&self) -> String {
        self.etag.to_string()
    }

    fn apply(&mut self, step: Step) -> Result<()> {
        let id = |n: usize| n.to_string();
        match step {
            Step::Acquire { node } if !self.nodes[node].paused => {
                // The protocol scheduler does not replace a live grant held by the same node.
                if self.live()
                    && self.lease.as_ref().and_then(|l| l.holder.as_deref())
                        == Some(id(node).as_str())
                {
                    return Ok(());
                }
                let etag = self.etag_text();
                match BucketLease::acquire(
                    self.lease.as_ref().map(|l| (l, etag.as_str())),
                    &id(node),
                    self.now,
                ) {
                    Ok((lease, condition)) => {
                        self.check_condition(condition)?;
                        self.etag += 1;
                        self.nodes[node].belief = Some(lease.fence);
                        self.catalog.fence = self.catalog.fence.max(lease.fence);
                        self.catalog_etag += 1;
                        self.cursor.push(CommitEntry {
                            kind: Kind::Acquire,
                            table: "protocol".into(),
                            run_id: None,
                            cursor: None,
                            cursor_kind: None,
                            fence: lease.fence,
                        });
                        self.cursor_etag += 1;
                        self.lease = Some(lease);
                        self.live = true;
                    }
                    Err(contextful_core::store::StoreError::LeaseHeld(_)) if self.live() => {}
                    Err(err) => bail!("acquire: {err}"),
                }
            }
            Step::Acquire { .. } => {}
            Step::Renew { node } if !self.nodes[node].paused => {
                if let (Some(lease), Some(belief)) = (&self.lease, self.nodes[node].belief) {
                    if lease.holder.as_deref() == Some(id(node).as_str()) && lease.fence == belief {
                        let etag = self.etag_text();
                        let (renewed, condition) =
                            BucketLease::renew(lease, &etag, lease, self.now)?;
                        self.check_condition(condition)?;
                        self.etag += 1;
                        self.lease = Some(renewed);
                        self.live = true;
                    }
                }
            }
            Step::Renew { .. } => {}
            Step::Expire => {
                if let Some(expiry) = self.lease.as_ref().and_then(|l| l.expires_at) {
                    if self.now <= expiry {
                        self.now = expiry.plus_secs(31);
                    }
                }
                if self.lease.is_some() {
                    self.live = false;
                }
            }
            Step::Release { node } if !self.nodes[node].paused => {
                if let (Some(lease), Some(belief)) = (&self.lease, self.nodes[node].belief) {
                    if lease.holder.as_deref() == Some(id(node).as_str()) && lease.fence == belief {
                        let etag = self.etag_text();
                        let (released, condition) = BucketLease::release(lease, &etag, &id(node))?;
                        self.check_condition(condition)?;
                        self.etag += 1;
                        self.lease = Some(released);
                        self.nodes[node].belief = None;
                    }
                }
            }
            Step::Release { .. } => {}
            Step::Send { node, target } => {
                let n = &mut self.nodes[node];
                if !n.paused && n.pending.is_none() {
                    if let Some(fence) = n.belief {
                        n.pending = Some((target, fence));
                    }
                }
            }
            Step::Deliver { node } => {
                if let Some((target, fence)) = self.nodes[node].pending.take() {
                    match target {
                        Target::Catalog => {
                            if self.catalog.admit(fence).is_ok() {
                                self.catalog.fence = fence;
                                self.catalog_etag += 1;
                            }
                        }
                        Target::Cursor => {
                            if commit_log::admit(&self.cursor, "protocol", fence).is_ok() {
                                self.cursor.push(CommitEntry {
                                    kind: Kind::Commit,
                                    table: "protocol".into(),
                                    run_id: Some(format!("node-{node}")),
                                    cursor: None,
                                    cursor_kind: None,
                                    fence,
                                });
                                self.cursor_etag += 1;
                            }
                        }
                    }
                }
            }
            Step::Pause { node } => self.nodes[node].paused = true,
            Step::Resume { node } => self.nodes[node].paused = false,
            Step::Crash { node } => {
                self.nodes[node].belief = None;
                self.nodes[node].paused = false;
            }
        }
        Ok(())
    }

    fn check_condition(&self, condition: Condition) -> Result<()> {
        match condition {
            Condition::IfNoneMatch if self.lease.is_none() => Ok(()),
            Condition::IfMatch(etag) if self.lease.is_some() && etag == self.etag_text() => Ok(()),
            other => bail!("unexpected lease condition {other:?} at ETag {}", self.etag),
        }
    }

    fn show(&self) -> String {
        let lease = match &self.lease {
            None => "none".to_string(),
            Some(l) => format!(
                "holder={} fence={} live={}",
                option(l.holder.as_deref()),
                l.fence,
                self.live()
            ),
        };
        let views = self
            .nodes
            .iter()
            .enumerate()
            .map(|(i, n)| {
                let pending = n
                    .pending
                    .map(|(t, f)| format!("(some {}@{f})", t.name()))
                    .unwrap_or_else(|| "none".into());
                format!(
                    "n{i}(belief={} paused={} pending={pending})",
                    option(n.belief),
                    n.paused
                )
            })
            .collect::<Vec<_>>()
            .join(" ");
        format!(
            "lease[{lease}] catalog={} cursor={} {views} catalog-etag={} cursor-etag={}",
            self.catalog.fence,
            self.cursor_fence(),
            self.catalog_etag,
            self.cursor_etag
        )
    }
}

fn option<T: std::fmt::Display>(value: Option<T>) -> String {
    value
        .map(|v| format!("(some {v})"))
        .unwrap_or_else(|| "none".into())
}

#[derive(Clone, Copy, Debug, Default)]
struct GenerationNode {
    belief: Option<u64>,
    paused: bool,
    pending: Option<(Target, u64)>,
}

#[derive(Clone, Debug, Default)]
struct GenerationState {
    fence: u64,
    holder: Option<usize>,
    live: bool,
    nodes: [GenerationNode; NODES],
}

struct ProtocolGenerator;

impl ReferenceStateMachine for ProtocolGenerator {
    type State = GenerationState;
    type Transition = Step;

    fn init_state() -> proptest::strategy::BoxedStrategy<Self::State> {
        Just(GenerationState::default()).boxed()
    }

    fn transitions(_state: &Self::State) -> proptest::strategy::BoxedStrategy<Self::Transition> {
        prop_oneof![
            (0..NODES).prop_map(|node| Step::Acquire { node }),
            (0..NODES).prop_map(|node| Step::Renew { node }),
            Just(Step::Expire),
            (0..NODES).prop_map(|node| Step::Release { node }),
            (0..NODES).prop_map(|node| Step::Send {
                node,
                target: Target::Catalog
            }),
            (0..NODES).prop_map(|node| Step::Send {
                node,
                target: Target::Cursor
            }),
            (0..NODES).prop_map(|node| Step::Deliver { node }),
            (0..NODES).prop_map(|node| Step::Pause { node }),
            (0..NODES).prop_map(|node| Step::Resume { node }),
            (0..NODES).prop_map(|node| Step::Crash { node }),
        ]
        .boxed()
    }

    fn preconditions(state: &Self::State, transition: &Self::Transition) -> bool {
        !matches!(transition, Step::Acquire { .. }) || state.fence < GENERATIONS
    }

    fn apply(mut state: Self::State, transition: &Self::Transition) -> Self::State {
        match *transition {
            Step::Acquire { node }
                if !state.nodes[node].paused && (state.holder.is_none() || !state.live) =>
            {
                state.fence += 1;
                state.holder = Some(node);
                state.live = true;
                state.nodes[node].belief = Some(state.fence);
            }
            Step::Renew { node }
                if !state.nodes[node].paused
                    && state.holder == Some(node)
                    && state.nodes[node].belief == Some(state.fence) =>
            {
                state.live = true;
            }
            Step::Expire if state.fence > 0 => state.live = false,
            Step::Release { node }
                if !state.nodes[node].paused
                    && state.holder == Some(node)
                    && state.nodes[node].belief == Some(state.fence) =>
            {
                state.holder = None;
                state.nodes[node].belief = None;
            }
            Step::Send { node, target }
                if !state.nodes[node].paused && state.nodes[node].pending.is_none() =>
            {
                if let Some(fence) = state.nodes[node].belief {
                    state.nodes[node].pending = Some((target, fence));
                }
            }
            Step::Deliver { node } => state.nodes[node].pending = None,
            Step::Pause { node } => state.nodes[node].paused = true,
            Step::Resume { node } => state.nodes[node].paused = false,
            Step::Crash { node } => {
                state.nodes[node].belief = None;
                state.nodes[node].paused = false;
            }
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

fn compare(reference: &Path, steps: &[Step]) -> Result<Option<(usize, String, String)>> {
    let mut child = Command::new(reference)
        .arg("run")
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
    let mut store = Store::new();
    for (i, (step, model)) in steps.iter().zip(states).enumerate() {
        store.apply(*step)?;
        let actual = store.show();
        if model != actual {
            return Ok(Some((i, model.into(), actual)));
        }
    }
    Ok(None)
}

fn minimize(reference: &Path, mut steps: Vec<Step>) -> Result<Vec<Step>> {
    let mut i = 0;
    while i < steps.len() && steps.len() > 1 {
        let mut candidate = steps.clone();
        candidate.remove(i);
        if compare(reference, &candidate)?.is_some() {
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
        if compare(&reference, &steps)?.is_some() {
            let reduced = minimize(&reference, steps)?;
            let (at, model, store) = compare(&reference, &reduced)?
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
            bail!("ProtocolConformanceDrift: seed={} case={case} step={at}; sequence={entry}; model={model}; store={store}", args.seed);
        }
    }
    println!(
        "stale-fence-differential: 0 drift cases; seed={}; generated={}; regressions={replayed}",
        args.seed, args.cases
    );
    Ok(())
}
