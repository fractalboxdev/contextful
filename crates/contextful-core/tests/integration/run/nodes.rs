//! `run.compile`: the node plan a host run lowers — its node kinds, its version and the
//! refusals at compile.

use contextful_core::run::journal::sha256_hex;
use contextful_core::run::nodes::{canonical, NodeKind, NodePlan, PLAN_VERSION_CHARS};
use contextful_core::run::RunError;
use serde_json::{json, Value};

fn step(id: &str) -> Value {
    json!({ "id": id, "kind": "step", "connector": { "id": "vendor", "version": "1", "command": ["sh", "fetch.sh"] } })
}

fn compile(nodes: Value) -> Result<NodePlan, RunError> {
    NodePlan::compile(json!({ "id": "translate", "nodes": nodes }).to_string().as_bytes())
}

#[test]
fn a_plan_of_every_node_kind_compiles_in_predecessor_order() {
    let plan = compile(json!([
        { "id": "merge", "kind": "join", "parallel": "fan", "allow_partial": true },
        { "id": "fan", "kind": "parallel", "nodes": ["en", "fr"], "after": ["pick"] },
        step("en"),
        step("fr"),
        { "id": "detect", "kind": "step", "connector": { "id": "lang", "version": "2", "command": ["sh", "detect.sh"] }, "retry": { "attempts": 2 } },
        { "id": "pick", "kind": "branch", "predicate": "detect", "arms": { "text": "fan", "image": "approve" }, "after": ["detect"] },
        { "id": "approve", "kind": "awaitEvent", "timeout": "1h" },
        { "id": "cool", "kind": "sleep", "duration": "5m", "after": ["merge"] },
    ]))
    .unwrap();
    let order: Vec<&str> = plan.order().map(|n| n.id.as_str()).collect();
    assert_eq!(order, ["detect", "pick", "fan", "merge", "approve", "cool"], "bodies run inside their parallel node");
    assert!(matches!(plan.node("merge").unwrap().kind, NodeKind::Join { allow_partial: true, .. }));
    assert_eq!(plan.schedule("detect").unwrap().attempts, 2);
    assert_eq!(plan.predecessors(plan.node("approve").unwrap()), ["pick"], "an arm follows its branch");
}

/// A node id repeated in one plan raises `PipelineNodeIdCollision`, naming the id and both positions.
// spec: run.compile.node-id-collision@443cd14f
#[test]
fn a_repeated_node_id_raises_pipeline_node_id_collision() {
    match compile(json!([step("fetch"), step("embed"), step("fetch")])) {
        Err(RunError::PipelineNodeIdCollision(m)) => assert!(m.contains("`fetch`") && m.contains("nodes[0]") && m.contains("nodes[2]"), "{m}"),
        other => panic!("expected PipelineNodeIdCollision, got {other:?}"),
    }
}

/// A `step` body other than a connector reference raises `PipelineInlineStepBody`.
// spec: run.compile.inline-step-body@9eb835b6
#[test]
fn a_step_body_other_than_a_connector_reference_raises_pipeline_inline_step_body() {
    let script = json!({ "id": "fetch", "kind": "step", "script": "curl https://example.com" });
    let beside = json!({ "id": "fetch", "kind": "step", "connector": { "id": "v", "version": "1", "command": ["v"] }, "run": "echo hi" });
    let named = json!({ "id": "fetch", "kind": "step", "connector": "vendor" });
    for node in [script, beside, named] {
        match compile(json!([node.clone()])) {
            Err(RunError::PipelineInlineStepBody(m)) => assert!(m.contains("`fetch`"), "{m}"),
            other => panic!("{node}: expected PipelineInlineStepBody, got {other:?}"),
        }
    }
}

/// A `parallel` node no `join` node names, or a node other than its join reading it or one of its bodies, raises
/// `PipelineParallelUnjoined` at compile.
// spec: run.compile.unjoined-parallel@3db0adda
#[test]
fn a_parallel_node_without_its_join_raises_pipeline_parallel_unjoined() {
    let fan = json!({ "id": "fan", "kind": "parallel", "nodes": ["en", "fr"] });
    let join = json!({ "id": "merge", "kind": "join", "parallel": "fan" });
    assert!(compile(json!([fan.clone(), step("en"), step("fr"), join.clone()])).is_ok());
    let around = json!({ "id": "next", "kind": "sleep", "duration": "1s", "after": ["fan"] });
    let reads_body = json!({ "id": "next", "kind": "sleep", "duration": "1s", "after": ["en"] });
    for nodes in [json!([fan.clone(), step("en"), step("fr")]), json!([fan.clone(), step("en"), step("fr"), join.clone(), around]), json!([fan, step("en"), step("fr"), join, reads_body])] {
        match compile(nodes.clone()) {
            Err(RunError::PipelineParallelUnjoined(m)) => assert!(m.contains("`fan`"), "{m}"),
            other => panic!("{nodes}: expected PipelineParallelUnjoined, got {other:?}"),
        }
    }
}

/// A plan's version is the leading 16 chars of the sha256 over the RFC 8785 canonical JSON of its `{id, nodes}`.
// spec: run.compile.plan-version@bd9c99e3
#[test]
fn a_plan_version_hashes_the_canonical_json_of_id_and_nodes() {
    assert_eq!(PLAN_VERSION_CHARS, 16);
    let written = r#"{ "nodes": [ { "kind": "sleep", "id": "cool", "duration": "5m" } ], "id": "p" }"#;
    let reordered = r#"{"id":"p","nodes":[{"duration":"5m","id":"cool","kind":"sleep"}]}"#;
    let plan = NodePlan::compile(written.as_bytes()).unwrap();
    assert_eq!(plan.version, sha256_hex(reordered.as_bytes())[..16], "members sort by name and carry no whitespace");
    assert_eq!(NodePlan::compile(reordered.as_bytes()).unwrap().version, plan.version);
    let moved = r#"{"id":"p","nodes":[{"duration":"6m","id":"cool","kind":"sleep"}]}"#;
    assert_ne!(NodePlan::compile(moved.as_bytes()).unwrap().version, plan.version, "a changed node changes the version");
    assert_eq!(canonical(&json!({ "b": 1.0, "a": [2.50, -0.0, "é"] })), r#"{"a":[2.5,0,"é"],"b":1}"#);
}
