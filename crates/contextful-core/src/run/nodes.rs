//! `run.compile.plan-node`: the node plan a host run lowers onto one execution.
//!
//! A plan is JSON `{id, nodes}`: a flat node list, each node an `id`, its predecessor
//! edges under `after`, and a `kind` — `step`, `sleep`, `awaitEvent`, `branch`,
//! `parallel` or `join`. [`NodePlan::compile`] refuses a repeated id, a step body other
//! than a connector reference and a `parallel` node its own `join` does not rejoin, and
//! computes the plan's version and the order the executor lowers it in.

use super::journal::sha256_hex;
use super::plan::ConnectorSpec;
use super::retry::{Schedule, ScheduleSpec};
use super::RunError;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};

/// Hash characters forming a plan's version (`run.compile.plan-version`).
pub const PLAN_VERSION_CHARS: usize = 16;

/// The keys a `step` node carries beside its connector reference.
const STEP_KEYS: [&str; 5] = ["id", "kind", "after", "connector", "retry"];

/// One node's kind and the fields that kind declares.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase", deny_unknown_fields)]
pub enum NodeKind {
    /// One connector pull, recorded once, under an optional retry schedule.
    Step {
        connector: ConnectorSpec,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        retry: Option<ScheduleSpec>,
    },
    /// A durable wait, spelled as a span such as `30s` or `5m`.
    Sleep { duration: String },
    /// A suspension on an awakeable, with an optional time-to-live span.
    AwaitEvent {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        timeout: Option<String>,
    },
    /// An arm chosen by a predecessor's recorded output: the label-to-node-id map.
    Branch { predicate: String, arms: BTreeMap<String, String> },
    /// Fan-out bodies, each a node id run as one branch.
    Parallel { nodes: Vec<String> },
    /// The explicit join node rejoining one `parallel` node's bodies.
    Join {
        parallel: String,
        #[serde(default)]
        allow_partial: bool,
    },
}

impl NodeKind {
    /// The kind as a plan spells it.
    pub fn name(&self) -> &'static str {
        match self {
            NodeKind::Step { .. } => "step",
            NodeKind::Sleep { .. } => "sleep",
            NodeKind::AwaitEvent { .. } => "awaitEvent",
            NodeKind::Branch { .. } => "branch",
            NodeKind::Parallel { .. } => "parallel",
            NodeKind::Join { .. } => "join",
        }
    }
}

/// One plan node: its id, its predecessor edges and its kind.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Node {
    pub id: String,
    pub after: Vec<String>,
    pub kind: NodeKind,
}

/// A compiled node plan: its nodes as written, its version, and the order the executor
/// lowers its top-level nodes in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NodePlan {
    pub id: String,
    pub nodes: Vec<Node>,
    /// The leading [`PLAN_VERSION_CHARS`] hex chars of the sha256 over the canonical JSON
    /// of `{id, nodes}` (`run.compile.plan-version`).
    pub version: String,
    order: Vec<usize>,
    schedules: BTreeMap<String, Schedule>,
}

fn invalid(message: String) -> RunError {
    RunError::Invalid(message)
}

impl NodePlan {
    /// Compile plan JSON: parse every node, refuse what `run.compile` refuses, and order
    /// the top-level nodes so each follows its predecessors.
    pub fn compile(bytes: &[u8]) -> Result<NodePlan, RunError> {
        let value: Value = serde_json::from_slice(bytes).map_err(|e| invalid(format!("the plan is no JSON document: {e}")))?;
        let Value::Object(top) = &value else { return Err(invalid("a plan is a JSON object `{id, nodes}`".into())) };
        if let Some(key) = top.keys().find(|k| !matches!(k.as_str(), "id" | "nodes")) {
            return Err(invalid(format!("a plan carries `id` and `nodes`, not `{key}`")));
        }
        let id = top.get("id").and_then(Value::as_str).filter(|s| !s.is_empty()).ok_or_else(|| invalid("a plan names no `id`".into()))?.to_string();
        let raw = top.get("nodes").and_then(Value::as_array).ok_or_else(|| invalid(format!("plan `{id}` lists no `nodes` array")))?;
        let mut seen: BTreeMap<String, usize> = BTreeMap::new();
        let mut nodes = Vec::with_capacity(raw.len());
        for (position, node) in raw.iter().enumerate() {
            let node = parse_node(position, node)?;
            if let Some(first) = seen.insert(node.id.clone(), position) {
                return Err(RunError::PipelineNodeIdCollision(format!("node id `{}` appears at nodes[{first}] and nodes[{position}] of plan `{id}`", node.id)));
            }
            nodes.push(node);
        }
        let version = sha256_hex(canonical(&Value::Object([("id".to_string(), Value::String(id.clone())), ("nodes".to_string(), Value::Array(raw.clone()))].into_iter().collect())).as_bytes())[..PLAN_VERSION_CHARS].to_string();
        let mut plan = NodePlan { id, nodes, version, order: Vec::new(), schedules: BTreeMap::new() };
        plan.check()?;
        Ok(plan)
    }

    /// The node named `id`.
    pub fn node(&self, id: &str) -> Option<&Node> {
        self.nodes.iter().find(|n| n.id == id)
    }

    /// The top-level nodes in lowering order: each after its predecessors, ties in
    /// declaration order. A `parallel` node's bodies run inside it and are absent here.
    pub fn order(&self) -> impl Iterator<Item = &Node> {
        self.order.iter().map(|i| &self.nodes[*i])
    }

    /// The compiled retry schedule a `step` node declares.
    pub fn schedule(&self, node: &str) -> Option<&Schedule> {
        self.schedules.get(node)
    }

    /// Every edge into `node`: its `after` list, the branch whose arm names it, and for a
    /// join, the `parallel` node it rejoins.
    pub fn predecessors<'a>(&'a self, node: &'a Node) -> Vec<&'a str> {
        let mut preds: Vec<&str> = node.after.iter().map(String::as_str).collect();
        for other in &self.nodes {
            if let NodeKind::Branch { arms, .. } = &other.kind {
                if arms.values().any(|t| *t == node.id) && !preds.contains(&other.id.as_str()) {
                    preds.push(&other.id);
                }
            }
        }
        if let NodeKind::Join { parallel, .. } = &node.kind {
            if !preds.contains(&parallel.as_str()) {
                preds.push(parallel);
            }
        }
        preds
    }

    /// The branch nodes whose arms name `node`.
    pub fn arm_of(&self, node: &str) -> Vec<&str> {
        self.nodes.iter().filter(|b| matches!(&b.kind, NodeKind::Branch { arms, .. } if arms.values().any(|t| t == node))).map(|b| b.id.as_str()).collect()
    }

    fn check(&mut self) -> Result<(), RunError> {
        let ids: BTreeSet<&str> = self.nodes.iter().map(|n| n.id.as_str()).collect();
        let known = |from: &str, to: &str, edge: &str| -> Result<(), RunError> {
            if to == from {
                return Err(invalid(format!("node `{from}` names itself as its {edge}")));
            }
            if !ids.contains(to) {
                return Err(invalid(format!("node `{from}` names `{to}` as its {edge}, and the plan holds no such node")));
            }
            Ok(())
        };
        let mut bodies: BTreeMap<&str, &str> = BTreeMap::new();
        let mut joins: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
        for node in &self.nodes {
            for pred in &node.after {
                known(&node.id, pred, "predecessor")?;
            }
            match &node.kind {
                NodeKind::Step { retry, .. } => {
                    if let Some(spec) = retry {
                        self.schedules.insert(node.id.clone(), spec.compile()?);
                    }
                }
                NodeKind::Sleep { duration } => {
                    crate::time::duration_secs(duration).ok_or_else(|| invalid(format!("sleep `{}`: `{duration}` is no span of digits followed by `s`, `m`, `h` or `d`", node.id)))?;
                }
                NodeKind::AwaitEvent { timeout: Some(t) } => {
                    crate::time::duration_secs(t).ok_or_else(|| invalid(format!("awaitEvent `{}`: `{t}` is no span of digits followed by `s`, `m`, `h` or `d`", node.id)))?;
                }
                NodeKind::AwaitEvent { timeout: None } => {}
                NodeKind::Branch { predicate, arms } => {
                    known(&node.id, predicate, "predicate")?;
                    if !node.after.contains(predicate) {
                        return Err(invalid(format!("branch `{}` reads predicate `{predicate}`, which its `after` does not list", node.id)));
                    }
                    if arms.is_empty() {
                        return Err(invalid(format!("branch `{}` declares no arm", node.id)));
                    }
                    for target in arms.values() {
                        known(&node.id, target, "arm")?;
                    }
                }
                NodeKind::Parallel { nodes } => {
                    if nodes.is_empty() {
                        return Err(invalid(format!("parallel `{}` lists no body", node.id)));
                    }
                    for body in nodes {
                        known(&node.id, body, "body")?;
                        if let Some(other) = bodies.insert(body, &node.id) {
                            return Err(invalid(format!("node `{body}` is a body of both `{other}` and `{}`", node.id)));
                        }
                    }
                }
                NodeKind::Join { parallel, .. } => {
                    known(&node.id, parallel, "parallel")?;
                    joins.entry(parallel.as_str()).or_default().push(&node.id);
                }
            }
        }
        for (body, parallel) in &bodies {
            let node = self.node(body).expect("a known body");
            if !matches!(node.kind, NodeKind::Step { .. } | NodeKind::Sleep { .. } | NodeKind::AwaitEvent { .. }) {
                return Err(invalid(format!("body `{body}` of parallel `{parallel}` is a `{}`; a body is a step, sleep or awaitEvent", node.kind.name())));
            }
            if !node.after.is_empty() || !self.arm_of(body).is_empty() {
                return Err(invalid(format!("body `{body}` of parallel `{parallel}` runs as its branch alone, and takes no edge of its own")));
            }
        }
        for node in &self.nodes {
            for pred in &node.after {
                if let Some(parallel) = bodies.get(pred.as_str()) {
                    return Err(RunError::PipelineParallelUnjoined(format!("node `{}` reads body `{pred}` of parallel `{parallel}`, whose outputs leave only through its join", node.id)));
                }
            }
            if let NodeKind::Join { parallel, .. } = &node.kind {
                if !matches!(self.node(parallel).map(|p| &p.kind), Some(NodeKind::Parallel { .. })) {
                    return Err(invalid(format!("join `{}` names `{parallel}`, which is no parallel node", node.id)));
                }
            }
        }
        for node in &self.nodes {
            let NodeKind::Parallel { .. } = node.kind else { continue };
            match joins.get(node.id.as_str()).map(Vec::as_slice) {
                None | Some([]) => {
                    return Err(RunError::PipelineParallelUnjoined(format!("parallel `{}` has no join node naming it; fan-out bodies rejoin only at a declared join", node.id)));
                }
                Some([_]) => {}
                Some(many) => return Err(invalid(format!("parallel `{}` is rejoined by {} join nodes: {}", node.id, many.len(), many.join(", ")))),
            }
            let join = joins[node.id.as_str()][0];
            for other in &self.nodes {
                let reads = other.after.contains(&node.id) || matches!(&other.kind, NodeKind::Branch { predicate, .. } if *predicate == node.id);
                if reads && other.id != join {
                    return Err(RunError::PipelineParallelUnjoined(format!("node `{}` follows parallel `{}` around its join `{join}`", other.id, node.id)));
                }
            }
        }
        self.order = self.topological(&bodies)?;
        Ok(())
    }

    /// Kahn's order over the top-level nodes, ties broken by declaration position; a cycle
    /// refuses.
    fn topological(&self, bodies: &BTreeMap<&str, &str>) -> Result<Vec<usize>, RunError> {
        let top: Vec<usize> = (0..self.nodes.len()).filter(|i| !bodies.contains_key(self.nodes[*i].id.as_str())).collect();
        let mut placed: BTreeSet<&str> = BTreeSet::new();
        let mut order = Vec::with_capacity(top.len());
        while order.len() < top.len() {
            let next = top.iter().copied().find(|i| {
                let node = &self.nodes[*i];
                !placed.contains(node.id.as_str()) && self.predecessors(node).iter().all(|p| placed.contains(p))
            });
            let Some(i) = next else {
                let stuck: Vec<&str> = top.iter().map(|i| self.nodes[*i].id.as_str()).filter(|id| !placed.contains(id)).collect();
                return Err(invalid(format!("plan `{}` holds a cycle through {}", self.id, stuck.join(", "))));
            };
            placed.insert(&self.nodes[i].id);
            order.push(i);
        }
        Ok(order)
    }
}

fn parse_node(position: usize, value: &Value) -> Result<Node, RunError> {
    let Value::Object(map) = value else { return Err(invalid(format!("nodes[{position}] is no JSON object"))) };
    let id = map.get("id").and_then(Value::as_str).filter(|s| !s.is_empty()).ok_or_else(|| invalid(format!("nodes[{position}] names no `id`")))?.to_string();
    let after: Vec<String> = match map.get("after") {
        None => Vec::new(),
        Some(v) => serde_json::from_value(v.clone()).map_err(|_| invalid(format!("node `{id}`: `after` lists node ids")))?,
    };
    if map.get("kind").and_then(Value::as_str) == Some("step") {
        if let Some(key) = map.keys().find(|k| !STEP_KEYS.contains(&k.as_str())) {
            return Err(RunError::PipelineInlineStepBody(format!("step `{id}` carries `{key}`; a step body is a connector reference alone")));
        }
        if !matches!(map.get("connector"), Some(Value::Object(_))) {
            return Err(RunError::PipelineInlineStepBody(format!("step `{id}` names no connector reference")));
        }
    }
    let mut rest = map.clone();
    rest.remove("id");
    rest.remove("after");
    let kind: NodeKind = serde_json::from_value(Value::Object(rest)).map_err(|e| invalid(format!("node `{id}`: {e}")))?;
    Ok(Node { id, after, kind })
}

/// RFC 8785 canonical JSON: object members sorted by key, no insignificant whitespace,
/// and a number with an integral value written without a fraction or exponent.
pub fn canonical(value: &Value) -> String {
    let mut out = String::new();
    write_canonical(value, &mut out);
    out
}

fn write_canonical(value: &Value, out: &mut String) {
    match value {
        Value::Object(map) => {
            let mut keys: Vec<&String> = map.keys().collect();
            // RFC 8785 orders members by the UTF-16 code units of their names.
            keys.sort_by(|a, b| a.encode_utf16().cmp(b.encode_utf16()));
            out.push('{');
            for (i, key) in keys.into_iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                out.push_str(&Value::String(key.clone()).to_string());
                out.push(':');
                write_canonical(&map[key], out);
            }
            out.push('}');
        }
        Value::Array(items) => {
            out.push('[');
            for (i, item) in items.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                write_canonical(item, out);
            }
            out.push(']');
        }
        Value::Number(n) => match n.as_f64() {
            Some(f) if n.is_f64() && f == 0.0 => out.push('0'),
            Some(f) if n.is_f64() && f.fract() == 0.0 && f.abs() < 1e21 => out.push_str(&format!("{f:.0}")),
            _ => out.push_str(&n.to_string()),
        },
        other => out.push_str(&other.to_string()),
    }
}
