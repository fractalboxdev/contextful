//! `run.compile`: the node plan a host run lowers — its node kinds, its version and the
//! refusals at compile.

use contextful_core::run::journal::sha256_hex;
use contextful_core::run::nodes::{canonical, plan_schema, NodeKind, NodePlan, PLAN_VERSION_CHARS};
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

/// A data-dependent conditional or loop in a workflow body raises `PipelineUndeclaredControlFlow`; branching travels as
/// `branch` and fan-out as `parallel`.
// spec: run.compile.control-flow@9c6dad12
#[test]
fn a_conditional_or_a_loop_in_a_plan_raises_pipeline_undeclared_control_flow() {
    let refused = |nodes: Value, named: &str| match compile(nodes.clone()) {
        Err(RunError::PipelineUndeclaredControlFlow(m)) => assert!(m.contains(named), "{m}"),
        other => panic!("{nodes}: expected PipelineUndeclaredControlFlow, got {other:?}"),
    };
    // Predecessor edges closing on themselves loop.
    let looped = json!([
        { "id": "fetch", "kind": "step", "connector": { "id": "v", "version": "1", "command": ["v"] }, "after": ["wait"] },
        { "id": "wait", "kind": "sleep", "duration": "1m", "after": ["fetch"] },
    ]);
    refused(looped, "fetch, wait");
    // A one-node loop: a node after itself, and a branch whose arm names the branch.
    refused(json!([{ "id": "fetch", "kind": "step", "connector": { "id": "v", "version": "1", "command": ["v"] }, "after": ["fetch"] }]), "`fetch`");
    refused(json!([step("fetch"), { "id": "pick", "kind": "branch", "predicate": "fetch", "arms": { "again": "pick" }, "after": ["fetch"] }]), "`pick`");
    // A guard on data, on a step or any other node.
    refused(json!([step("fetch"), { "id": "wait", "kind": "sleep", "duration": "1m", "after": ["fetch"], "if": "fetch.rows > 0" }]), "`wait`");
    let mut guarded = step("fetch");
    guarded["when"] = json!("$.page < 10");
    refused(json!([guarded]), "`fetch`");
    // A node kind looping or switching on data.
    for kind in ["if", "while", "forEach", "loop", "switch"] {
        refused(json!([step("fetch"), { "id": "again", "kind": kind, "after": ["fetch"], "body": "fetch" }]), "`again`");
    }
    // The same intent declared as a branch and a joined fan-out compiles.
    let declared = json!([
        step("fetch"),
        { "id": "pick", "kind": "branch", "predicate": "fetch", "arms": { "more": "fan", "done": "rest" }, "after": ["fetch"] },
        { "id": "fan", "kind": "parallel", "nodes": ["a", "b"] },
        step("a"),
        step("b"),
        { "id": "merge", "kind": "join", "parallel": "fan" },
        { "id": "rest", "kind": "sleep", "duration": "1m" },
    ]);
    assert!(compile(declared).is_ok());
}

/// Every `"kind"` constant a schema's subschemas fix, in document order.
fn kind_constants(schema: &Value, out: &mut Vec<String>) {
    match schema {
        Value::Object(map) => {
            if let Some(kind) = map.get("properties").and_then(|p| p.get("kind")).and_then(|k| k.get("const")).and_then(Value::as_str) {
                out.push(kind.to_string());
            }
            map.values().for_each(|v| kind_constants(v, out));
        }
        Value::Array(items) => items.iter().for_each(|v| kind_constants(v, out)),
        _ => {}
    }
}

/// The plan type is defined once in a schema library; its JSON Schema is the contract every language binds to, and
/// the run path deserializes plan JSON against it.
// spec: run.compile.plan-schema@db8d70e3
#[test]
fn the_plan_json_schema_derives_from_the_types_compile_deserializes() {
    let schema = plan_schema();
    assert_eq!(schema["$schema"], "https://json-schema.org/draft/2020-12/schema");
    assert_eq!(schema["required"], json!(["id", "nodes"]));
    assert_eq!(schema["additionalProperties"], json!(false), "a plan carries `id` and `nodes` alone");
    let node = &schema["$defs"]["Node"];
    assert_eq!(node["required"], json!(["id"]));
    assert!(node["properties"]["after"].is_object(), "{node}");
    assert_eq!(node["unevaluatedProperties"], json!(false), "a node key no kind declares is outside the contract");
    let mut kinds = Vec::new();
    kind_constants(node, &mut kinds);
    assert_eq!(kinds, ["step", "sleep", "awaitEvent", "branch", "parallel", "join"]);
    // Each kind's schema names the fields the compiler reads for it.
    let variant = |kind: &str| node["oneOf"].as_array().unwrap().iter().find(|v| v["properties"]["kind"]["const"] == kind).cloned().unwrap();
    assert_eq!(variant("step")["required"], json!(["kind", "connector"]));
    assert_eq!(variant("branch")["required"], json!(["kind", "predicate", "arms"]));
    assert_eq!(variant("join")["properties"]["allow_partial"]["default"], json!(false));
    assert_eq!(schema["$defs"]["ConnectorSpec"]["required"], json!(["id", "version", "command"]));
    // A node of every kind the schema lists compiles; a kind it does not list is refused.
    let plan = compile(json!([
        step("fetch"),
        { "id": "wait", "kind": "sleep", "duration": "1m", "after": ["fetch"] },
        { "id": "hold", "kind": "awaitEvent", "after": ["wait"] },
    ]))
    .unwrap();
    assert_eq!(plan.nodes.iter().map(|n| n.kind.name()).collect::<Vec<_>>(), ["step", "sleep", "awaitEvent"]);
    assert!(compile(json!([{ "id": "x", "kind": "script", "source": "return 1" }])).is_err());
}
