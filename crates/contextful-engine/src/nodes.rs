//! The node-plan executor (`run.compile.lowering`): a client of the execution handle that
//! lowers a compiled [`NodePlan`] node by node onto one execution under host scope
//! `job:<name>`.
//!
//! A `step` is one connector pull through [`ExecutionPort::step`]; a `sleep` records its
//! wake instant and waits out the remainder; an `awaitEvent` suspends on an awakeable and
//! reads its payload back; a `branch` records the arm label its predicate's output names.
//! A `parallel` node runs each body under labels scoped to it and collects the outcomes
//! into a [`FanOut`] that only its `join` node rejoins (`run.journal.fan-out-join`): a
//! failed body fails a strict join, and a join declaring `allow_partial` records each
//! failed branch on the run row before the run closes.

use crate::command::CommandSource;
use crate::drive::job_scope;
use crate::execution::{Close, Execution, Tally};
use crate::runner::{Engine, EngineError};
use contextful_core::run::join::{FanOut, Join, Joined};
use contextful_core::run::journal::EntryKey;
use contextful_core::run::nodes::{Node, NodeKind, NodePlan};
use contextful_core::run::own::PlanPins;
use contextful_core::run::plan::ConnectorSpec;
use contextful_core::run::ports::{BlobStore, ExecutionPort, JournalStore, Landed, OpenExecution, PullRequest, Source, Wake};
use contextful_core::run::record::RunRow;
use contextful_core::run::retry::Schedule;
use contextful_core::run::{Failure, FailureTag, RunError};
use contextful_core::time::{duration_secs, Instant};
use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;
use std::time::Duration;

/// The time-to-live of an `awaitEvent` declaring no timeout: the widest span an instant
/// adds without saturating.
const UNBOUNDED_WAIT_SECS: u64 = u32::MAX as u64;

/// Resolves a `step` node's connector reference to the source serving its pull.
pub trait Connectors {
    fn source(&self, node: &str, connector: &ConnectorSpec) -> Result<Box<dyn Source + '_>, Failure>;
}

/// Each connector's `command` serves its pull as a child process run in `cwd`.
pub struct CommandConnectors {
    pub cwd: PathBuf,
}

impl Connectors for CommandConnectors {
    fn source(&self, _node: &str, connector: &ConnectorSpec) -> Result<Box<dyn Source + '_>, Failure> {
        Ok(Box::new(CommandSource { argv: connector.command.clone(), cwd: self.cwd.clone() }))
    }
}

/// One fire of a plan job: the job's host scope, the compiled plan and the attempt.
pub struct PlanFire<'a> {
    /// The job's name; the execution opens under host scope `job:<name>`.
    pub job: String,
    pub plan: &'a NodePlan,
    pub run_id: String,
    pub site_id: String,
    pub pid: u32,
    pub boot_id: String,
    /// The interval between re-checks of a pending awakeable.
    pub poll: Duration,
}

/// How a leaf node — a step, a sleep or an awaitEvent — failed: as a node outcome a
/// partial join admits, or as the execution's own failure.
enum Leaf {
    Failed(Failure),
    Fatal(Close),
}

impl From<EngineError> for Leaf {
    fn from(e: EngineError) -> Leaf {
        match e {
            EngineError::Refused(RunError::StepFailed { label, failure }) => {
                Leaf::Failed(Failure { message: format!("step `{label}` closed on {}", failure.message), ..failure })
            }
            EngineError::Refused(e) => Leaf::Fatal(Close::Refused(e)),
            EngineError::Failure(f) => Leaf::Fatal(Close::Failed(f)),
            EngineError::Topology(t) => Leaf::Fatal(Close::Failed(Failure::new(FailureTag::Config, t.to_string()))),
        }
    }
}

impl From<Leaf> for Close {
    fn from(leaf: Leaf) -> Close {
        match leaf {
            Leaf::Failed(f) => Close::Failed(f),
            Leaf::Fatal(c) => c,
        }
    }
}

fn fatal(e: EngineError) -> Close {
    Leaf::from(e).into()
}

impl<J: JournalStore + Clone, B: BlobStore + Clone> Engine<J, B> {
    /// Lower `fire.plan` onto one execution under `job:<name>`, pinned to the plan's
    /// version, resuming the scope's pending execution when one holds; the run closes
    /// `success` once every reached node completes, and `failed` on the first node failure
    /// no partial join admits.
    pub fn run_plan(&self, fire: &PlanFire<'_>, connectors: &dyn Connectors) -> Result<RunRow, EngineError> {
        let open = OpenExecution {
            scope: job_scope(&fire.job),
            pins: PlanPins { plan_ref: fire.plan.version.clone(), identities: Default::default() }.into(),
            run_id: fire.run_id.clone(),
            site_id: fire.site_id.clone(),
            pid: fire.pid,
            boot_id: fire.boot_id.clone(),
            trace_id: None,
            connector: None,
            schedule: Schedule::default(),
        };
        let mut x = self.open_execution(&open)?;
        let lowered = Lowering { x: &mut x, plan: fire.plan, connectors, poll: fire.poll }.run();
        match lowered {
            Ok(partial) => {
                if !partial.is_empty() {
                    self.catalog.update_run(&fire.run_id, &mut |row| {
                        for joined in &partial {
                            joined.record(row);
                        }
                        Ok(())
                    })?;
                }
                x.close_with(Ok((Landed::default(), Tally::default())))
            }
            Err(close) => x.close_with(Err(close)),
        }
    }
}

struct Lowering<'x, 'e, J: JournalStore + Clone, B: BlobStore + Clone> {
    x: &'x mut Execution<'e, J, B>,
    plan: &'x NodePlan,
    connectors: &'x dyn Connectors,
    poll: Duration,
}

impl<J: JournalStore + Clone, B: BlobStore + Clone> Lowering<'_, '_, J, B> {
    /// Lower every top-level node in order; answers the partial joins whose failed
    /// branches the run row records.
    fn run(&mut self) -> Result<Vec<Joined<Vec<u8>>>, Close> {
        let mut outputs: BTreeMap<String, Vec<u8>> = BTreeMap::new();
        let mut skipped: BTreeSet<String> = BTreeSet::new();
        let mut chosen: BTreeMap<String, String> = BTreeMap::new();
        let mut fans: BTreeMap<String, FanOut<Vec<u8>>> = BTreeMap::new();
        let mut partial = Vec::new();
        let plan = self.plan;
        for node in plan.order() {
            if self.skips(node, &skipped, &chosen) {
                skipped.insert(node.id.clone());
                continue;
            }
            match &node.kind {
                NodeKind::Step { .. } | NodeKind::Sleep { .. } | NodeKind::AwaitEvent { .. } => {
                    let input = self.input(node, &outputs);
                    let output = self.leaf(node, &node.id, &input)?;
                    outputs.insert(node.id.clone(), output);
                }
                NodeKind::Branch { predicate, arms } => {
                    let read = outputs.get(predicate).cloned().unwrap_or_default();
                    let label = String::from_utf8_lossy(read.as_slice()).trim().to_string();
                    let recorded = self.x.step(&format!("branch:{}", node.id), &read, &mut |_| Ok(label.clone().into_bytes())).map_err(fatal)?;
                    let label = String::from_utf8_lossy(&recorded).into_owned();
                    let arm = arms.get(&label).ok_or_else(|| {
                        Close::Failed(Failure::deterministic(FailureTag::Permanent, format!("branch `{}` read label `{label}` from `{predicate}`, which names no arm", node.id)))
                    })?;
                    chosen.insert(node.id.clone(), arm.clone());
                    outputs.insert(node.id.clone(), recorded);
                }
                NodeKind::Parallel { nodes } => {
                    let input = self.input(node, &outputs);
                    let mut fan = FanOut::new();
                    for body in nodes {
                        let body_node = plan.node(body).expect("a compiled body exists");
                        let outcome = match self.leaf(body_node, &format!("{}/{body}", node.id), &input) {
                            Ok(output) => Ok(output),
                            Err(Leaf::Failed(f)) => Err(f),
                            Err(Leaf::Fatal(c)) => return Err(c),
                        };
                        fan = fan.branch(body, outcome);
                    }
                    fans.insert(node.id.clone(), fan);
                }
                NodeKind::Join { parallel, allow_partial } => {
                    let fan = fans.remove(parallel).expect("a join follows its parallel node");
                    let joined = fan.join(&Join { label: node.id.clone(), allow_partial: *allow_partial }).map_err(Close::Failed)?;
                    let merged: BTreeMap<&str, String> = joined.outputs.iter().map(|(l, o)| (l.as_str(), String::from_utf8_lossy(o).into_owned())).collect();
                    outputs.insert(node.id.clone(), serde_json::to_vec(&merged).unwrap_or_default());
                    if !joined.failed.is_empty() {
                        partial.push(joined);
                    }
                }
            }
        }
        Ok(partial)
    }

    /// A node runs unless a branch naming it chose another arm, or every edge into it
    /// comes from a skipped node (`run.compile.branch-arm`).
    fn skips(&self, node: &Node, skipped: &BTreeSet<String>, chosen: &BTreeMap<String, String>) -> bool {
        let branches = self.plan.arm_of(&node.id);
        if !branches.is_empty() && !branches.iter().any(|b| chosen.get(*b) == Some(&node.id)) {
            return true;
        }
        let preds = self.plan.predecessors(node);
        !preds.is_empty() && preds.iter().all(|p| skipped.contains(*p))
    }

    /// The recorded outputs of a node's predecessors, keyed by node id: the input its
    /// entry key hashes.
    fn input(&self, node: &Node, outputs: &BTreeMap<String, Vec<u8>>) -> Vec<u8> {
        let read: BTreeMap<&str, String> = self.plan.predecessors(node).into_iter().filter_map(|p| outputs.get(p).map(|o| (p, String::from_utf8_lossy(o).into_owned()))).collect();
        serde_json::to_vec(&read).unwrap_or_default()
    }

    /// Lower one leaf node under step label `label`.
    fn leaf(&mut self, node: &Node, label: &str, input: &[u8]) -> Result<Vec<u8>, Leaf> {
        match &node.kind {
            NodeKind::Step { connector, .. } => {
                let mut source = self.connectors.source(&node.id, connector).map_err(Leaf::Failed)?;
                let key = EntryKey::new(self.x.execution_id(), label, input);
                let request = PullRequest { step_label: label.to_string(), position: None, idempotency_key: key.idempotency_key() };
                let declared = self.plan.schedule(&node.id).cloned();
                let previous = declared.map(|s| self.x.swap_schedule(s));
                let output = self.x.step(label, input, &mut |cancel| source.pull(&request, cancel));
                if let Some(previous) = previous {
                    self.x.swap_schedule(previous);
                }
                Ok(output?)
            }
            NodeKind::Sleep { duration } => {
                let secs = duration_secs(duration).unwrap_or_default();
                let now = self.x.engine().catalog.now().map_err(|f| Leaf::Fatal(Close::Failed(f)))?;
                let recorded = self.x.step(&format!("sleep:{label}"), input, &mut |_| Ok(now.plus_secs(secs).to_string().into_bytes()))?;
                let wake = Instant::parse(&String::from_utf8_lossy(&recorded)).map_err(|e| Leaf::Fatal(Close::Failed(Failure::new(FailureTag::Storage, format!("sleep `{label}` recorded no instant: {e}")))))?;
                let now = self.x.engine().catalog.now().map_err(|f| Leaf::Fatal(Close::Failed(f)))?;
                let remaining = now.secs_until(wake);
                if remaining > 0 && !self.x.token().wait_timeout(Duration::from_secs(remaining)) {
                    return Err(Leaf::Fatal(Close::Failed(Failure::canceled(format!("stopped during sleep `{label}`")))));
                }
                Ok(recorded)
            }
            NodeKind::AwaitEvent { timeout } => {
                let ttl = timeout.as_deref().and_then(duration_secs).unwrap_or(UNBOUNDED_WAIT_SECS);
                let token = self.x.suspend(label, ttl)?;
                loop {
                    match self.x.awaited(&token)? {
                        Wake::Resumed(payload) => return Ok(payload),
                        Wake::TimedOut => return Err(Leaf::Failed(Failure::deterministic(FailureTag::Permanent, format!("awaitEvent `{label}` passed its deadline unresolved")))),
                        Wake::Pending => {
                            if !self.x.token().wait_timeout(self.poll) {
                                return Err(Leaf::Fatal(Close::Failed(Failure::canceled(format!("stopped while awaiting `{label}`")))));
                            }
                        }
                    }
                }
            }
            other => unreachable!("a compiled plan lowers `{}` outside the leaf path", other.name()),
        }
    }
}
