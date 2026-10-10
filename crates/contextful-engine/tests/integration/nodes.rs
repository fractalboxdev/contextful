//! `run.compile.lowering`: the node-plan executor over a scripted connector set — branch
//! arms, fan-out bodies and their join, sleeps and awaited events.

use crate::support::Rig;
use contextful_core::run::join::FailedBranch;
use contextful_core::run::nodes::NodePlan;
use contextful_core::run::plan::ConnectorSpec;
use contextful_core::run::ports::{AwakeableStore, Cancellation, PullRequest, Source};
use contextful_core::run::record::RunStatus;
use contextful_core::run::{Failure, FailureTag};
use contextful_engine::awake::Registry;
use contextful_engine::nodes::{Connectors, PlanFire};
use contextful_engine::stores::FileAwakeableStore;
use contextful_engine::Engine;
use serde_json::{json, Value};
use std::panic::AssertUnwindSafe;
use std::sync::{Arc, Mutex};
use std::time::Duration;

type Answer = Box<dyn Fn(&str) -> Result<Vec<u8>, Failure> + Send + Sync>;

/// A connector set answering each step node by its id, logging every pull's step label.
struct Scripted {
    labels: Mutex<Vec<String>>,
    answer: Answer,
}

impl Scripted {
    fn new(answer: impl Fn(&str) -> Result<Vec<u8>, Failure> + Send + Sync + 'static) -> Scripted {
        Scripted { labels: Mutex::default(), answer: Box::new(answer) }
    }

    fn labels(&self) -> Vec<String> {
        self.labels.lock().unwrap().clone()
    }
}

struct Pull<'a>(&'a Scripted, String);

impl Source for Pull<'_> {
    fn pull(&mut self, request: &PullRequest, _: &dyn Cancellation) -> Result<Vec<u8>, Failure> {
        self.0.labels.lock().unwrap().push(request.step_label.clone());
        (self.0.answer)(&self.1)
    }
}

impl Connectors for Scripted {
    fn source(&self, node: &str, _: &ConnectorSpec) -> Result<Box<dyn Source + '_>, Failure> {
        Ok(Box::new(Pull(self, node.to_string())))
    }
}

fn step(id: &str, after: &[&str]) -> Value {
    json!({ "id": id, "kind": "step", "after": after, "connector": { "id": "vendor", "version": "1", "command": ["vendor"] } })
}

fn compile(nodes: Value) -> NodePlan {
    NodePlan::compile(json!({ "id": "translate", "nodes": nodes }).to_string().as_bytes()).unwrap()
}

fn fire<'a>(plan: &'a NodePlan, run_id: &str) -> PlanFire<'a> {
    PlanFire { job: "translate".into(), plan, run_id: run_id.into(), site_id: "site-a".into(), pid: 4242, boot_id: "boot-a".into(), poll: Duration::from_millis(20) }
}

fn fan_out(allow_partial: bool) -> NodePlan {
    compile(json!([
        step("fetch", &[]),
        { "id": "fan", "kind": "parallel", "nodes": ["en", "fr", "de"], "after": ["fetch"] },
        step("en", &[]),
        step("fr", &[]),
        step("de", &[]),
        { "id": "merge", "kind": "join", "parallel": "fan", "allow_partial": allow_partial },
        step("publish", &["merge"]),
    ]))
}

fn throttled_fr(node: &str) -> Result<Vec<u8>, Failure> {
    match node {
        "fr" => Err(Failure::new(FailureTag::Permanent, "the vendor refused the language")),
        other => Ok(format!("{other}-out").into_bytes()),
    }
}

/// Each body of a `parallel` node runs under step labels prefixed by the parallel node's id, and its outcome
/// reaches only the join, as {{run.journal.fan-out-join}} states.
// spec: run.compile.branch-scope@699e61fa
#[test]
fn fan_out_bodies_run_under_labels_scoped_to_their_parallel_node() {
    let rig = Rig::new();
    let connectors = Scripted::new(throttled_fr);
    let plan = fan_out(true);
    let row = rig.engine.run_plan(&fire(&plan, "plan-1"), &connectors).unwrap();
    assert_eq!(connectors.labels(), ["fetch", "fan/en", "fan/fr", "fan/de", "publish"]);
    assert_eq!(row.status, RunStatus::Success, "{:?}", row.error_message);
    assert_eq!(row.failed_branches, [FailedBranch { label: "fr".into(), tag: FailureTag::Permanent }]);
    assert_eq!(row.host_scope.as_deref(), Some("job:translate"));

    let strict = Rig::new();
    let connectors = Scripted::new(throttled_fr);
    let plan = fan_out(false);
    let row = strict.engine.run_plan(&fire(&plan, "plan-2"), &connectors).unwrap();
    assert_eq!(connectors.labels(), ["fetch", "fan/en", "fan/fr", "fan/de"], "a strict join that fails runs nothing after it");
    assert_eq!((row.status, row.error_kind), (RunStatus::Failed, Some(FailureTag::Permanent)));
    let message = row.error_message.unwrap_or_default();
    assert!(message.contains("branch `fr`") && message.contains("join `merge`"), "{message}");
    assert!(row.failed_branches.is_empty(), "a failed run lists no partial branch");
}

/// A `branch` node records, once, its predicate node's output trimmed of surrounding whitespace as the arm label,
/// so every replay takes the arm the first attempt took; the other arms' nodes are skipped.
// spec: run.compile.branch-arm@8d16672f
#[test]
fn a_branch_records_its_arm_and_a_resume_takes_the_same_one() {
    let rig = Rig::new();
    let plan = compile(json!([
        step("detect", &[]),
        { "id": "pick", "kind": "branch", "predicate": "detect", "after": ["detect"], "arms": { "text": "translate", "image": "caption" } },
        step("translate", &[]),
        step("caption", &[]),
        step("publish", &["translate"]),
    ]));
    let first = Scripted::new(|node| match node {
        "detect" => Ok(b"  image\n".to_vec()),
        "caption" => panic!("the host process dies inside the caption call"),
        other => Ok(other.as_bytes().to_vec()),
    });
    let died = std::panic::catch_unwind(AssertUnwindSafe(|| rig.engine.run_plan(&fire(&plan, "plan-1"), &first)));
    assert!(died.is_err());
    assert_eq!(first.labels(), ["detect", "caption"]);

    // The detector now answers another label; the recorded one decides the arm.
    rig.clock.advance(60);
    let second = Scripted::new(|node| match node {
        "detect" => Ok(b"text".to_vec()),
        other => Ok(other.as_bytes().to_vec()),
    });
    let row = rig.engine.run_plan(&fire(&plan, "plan-2"), &second).unwrap();
    assert_eq!(row.status, RunStatus::Success, "{:?}", row.error_message);
    assert_eq!(second.labels(), ["caption"], "detect and the branch replay; translate and publish are skipped");
}

#[test]
fn an_await_event_resumes_on_its_payload_and_a_sleep_waits_out_its_span() {
    let rig = Rig::new();
    let store = Arc::new(FileAwakeableStore::open(rig.dir.path()));
    let engine = Engine { awakeables: Some(store.clone()), ..rig.engine.clone() };
    let registry = Registry::over(store.clone(), rig.engine.journal.clone());
    let plan = compile(json!([
        { "id": "approve", "kind": "awaitEvent", "timeout": "1h" },
        { "id": "cool", "kind": "sleep", "duration": "1s", "after": ["approve"] },
        step("publish", &["cool"]),
    ]));
    let connectors = Scripted::new(|node| Ok(node.as_bytes().to_vec()));
    let started = std::time::Instant::now();
    let row = std::thread::scope(|s| {
        let run = s.spawn(|| engine.run_plan(&fire(&plan, "plan-1"), &connectors).unwrap());
        let token = loop {
            let rows = store.rows().unwrap();
            if let Some(row) = rows.first() {
                break row.token.clone();
            }
            std::thread::sleep(Duration::from_millis(10));
        };
        assert_eq!(rig.row("plan-1").status, RunStatus::Waiting);
        registry.resolve(&token, b"approved", rig.catalog().now().unwrap()).unwrap();
        run.join().unwrap()
    });
    assert_eq!(row.status, RunStatus::Success, "{:?}", row.error_message);
    assert!(started.elapsed() >= Duration::from_secs(1), "the sleep waited its span");
    assert_eq!(connectors.labels(), ["publish"]);
}
