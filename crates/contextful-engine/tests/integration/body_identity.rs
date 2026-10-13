use crate::support::{Rig, T0};
use contextful_core::run::drive::{
    Bodies, BodyDependencies, Emitted, InputRow, InputSet, ProviderModelIdentity, RowBody,
    RowCalls, RowStop, StoreInput,
};
use contextful_core::run::own::{ExecutionOwner, OwnerScope};
use contextful_core::run::ports::Landed;
use contextful_core::run::retry::Schedule;
use contextful_core::run::{Failure, FailureTag};
use contextful_engine::drive::Drive;
use serde_json::json;
use std::collections::BTreeMap;
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
};
use std::time::Duration;

struct Body {
    entered: Arc<AtomicUsize>,
    paid: Arc<AtomicUsize>,
}
impl RowBody for Body {
    fn run(&self, _: &InputRow, calls: &dyn RowCalls) -> Result<Emitted, RowStop> {
        self.entered.fetch_add(1, Ordering::SeqCst);
        calls.call("paid", b"input", &mut |_| {
            self.paid.fetch_add(1, Ordering::SeqCst);
            Ok(vec![1])
        })?;
        Ok(BTreeMap::new())
    }
}
struct Replacement(Body);
impl RowBody for Replacement {
    fn run(&self, row: &InputRow, calls: &dyn RowCalls) -> Result<Emitted, RowStop> {
        self.0.run(row, calls)
    }
}
fn fire<'a>(body: &'a dyn RowBody, id: &str) -> Drive<'a> {
    Drive {
        job: "score".into(),
        input: StoreInput {
            body: "score".into(),
            statement: "select * from docs".into(),
            as_of: None,
        },
        body,
        max_in_flight: 1,
        run_id: id.into(),
        site_id: "site".into(),
        pid: 1,
        boot_id: "boot".into(),
        schedule: Schedule::single_attempt(),
        poll: Duration::ZERO,
    }
}
fn read(_: &str) -> Result<InputSet, Failure> {
    Ok(InputSet {
        as_of: T0.into(),
        snapshots: BTreeMap::new(),
        rows: vec![json!({"id":"a"}).as_object().unwrap().clone()],
    })
}
fn dependencies(prompt: &str, model: &str, policy: &str) -> BodyDependencies {
    BodyDependencies::new(
        prompt.as_bytes(),
        &ProviderModelIdentity::new("https://model.example/v1", model).unwrap(),
        policy.as_bytes(),
    )
}
fn body(entered: &Arc<AtomicUsize>, paid: &Arc<AtomicUsize>) -> Arc<Body> {
    Arc::new(Body {
        entered: entered.clone(),
        paid: paid.clone(),
    })
}

#[test]
fn same_name_replacement_refuses_before_input_replay_or_body_entry() {
    let rig = Rig::new();
    let entered = Arc::new(AtomicUsize::new(0));
    let paid = Arc::new(AtomicUsize::new(0));
    let mut original = Bodies::default();
    original.register("score", body(&entered, &paid)).unwrap();
    let original = original.get("score").unwrap();
    rig.engine
        .drive(&fire(original.as_ref(), "first"), &mut read, &mut |_| {
            Err(Failure::new(FailureTag::Transient, "hold"))
        })
        .unwrap();
    let held = rig
        .catalog()
        .owner_at(&OwnerScope::host("job:score"))
        .unwrap()
        .unwrap();
    let contextful_core::run::own::OwnerPins::Plan(verified) = &held.pins else {
        panic!("body holds plan pins")
    };
    let executable = std::fs::read(std::env::current_exe().unwrap()).unwrap();
    assert_eq!(
        verified.identities["body_implementation"],
        contextful_core::run::journal::sha256_hex(&executable)
    );
    let replacement = Replacement(Body {
        entered: entered.clone(),
        paid: paid.clone(),
    });
    let error = rig
        .engine
        .drive(
            &fire(&replacement, "second"),
            &mut |_| panic!("changed implementation reads nothing"),
            &mut |_| panic!("changed implementation lands nothing"),
        )
        .unwrap_err();
    assert!(error.to_string().contains("ExecutionPinMismatch"));
    assert_eq!(entered.load(Ordering::SeqCst), 1);
    assert_eq!(paid.load(Ordering::SeqCst), 1);
}

#[test]
fn dependency_only_changes_refuse_and_unchanged_resume_reuses_paid_calls() {
    for changed in [
        dependencies("prompt-b", "model-a", "policy-a"),
        dependencies("prompt-a", "model-b", "policy-a"),
        dependencies("prompt-a", "model-a", "policy-b"),
    ] {
        let rig = Rig::new();
        let entered = Arc::new(AtomicUsize::new(0));
        let paid = Arc::new(AtomicUsize::new(0));
        let original = dependencies("prompt-a", "model-a", "policy-a");
        let mut bodies = Bodies::default();
        bodies
            .register_with_dependencies("score", body(&entered, &paid), original.clone())
            .unwrap();
        let first = bodies.get("score").unwrap();
        rig.engine
            .drive(&fire(first.as_ref(), "first"), &mut read, &mut |_| {
                Err(Failure::new(FailureTag::Transient, "hold"))
            })
            .unwrap();
        let pins = rig
            .catalog()
            .owner_at(&OwnerScope::host("job:score"))
            .unwrap()
            .unwrap()
            .pins;
        let mut bodies = Bodies::default();
        bodies
            .register_with_dependencies("score", body(&entered, &paid), changed)
            .unwrap();
        let changed = bodies.get("score").unwrap();
        assert!(rig
            .engine
            .drive(
                &fire(changed.as_ref(), "changed"),
                &mut |_| panic!("changed semantics read nothing"),
                &mut |_| panic!("changed semantics land nothing")
            )
            .is_err());
        assert_eq!(
            rig.catalog()
                .owner_at(&OwnerScope::host("job:score"))
                .unwrap()
                .unwrap()
                .pins,
            pins
        );
        rig.engine
            .drive(
                &fire(first.as_ref(), "resumed"),
                &mut |_| panic!("unchanged resume replays input"),
                &mut |_| Ok(Landed::default()),
            )
            .unwrap();
        assert_eq!(paid.load(Ordering::SeqCst), 1);
        let fresh = rig
            .engine
            .drive(&fire(changed.as_ref(), "fresh"), &mut read, &mut |_| {
                Err(Failure::new(FailureTag::Transient, "hold"))
            })
            .unwrap();
        assert_ne!(
            rig.catalog()
                .owner_at(&OwnerScope::host("job:score"))
                .unwrap()
                .unwrap()
                .pins,
            pins
        );
        assert_ne!(fresh.execution_id, rig.row("first").execution_id);
        assert_ne!(fresh.plan_ref, rig.row("first").plan_ref);
        assert_eq!(rig.row("first").plan_ref, rig.row("resumed").plan_ref);
        assert_eq!(
            rig.row("first").execution_id,
            rig.row("resumed").execution_id
        );
        assert_eq!(paid.load(Ordering::SeqCst), 2);
    }
}

#[test]
fn owners_without_implementation_pins_refuse_before_input_or_body() {
    let rig = Rig::new();
    let entered = Arc::new(AtomicUsize::new(0));
    let paid = Arc::new(AtomicUsize::new(0));
    let body = body(&entered, &paid);
    let drive = fire(body.as_ref(), "resume");
    rig.catalog()
        .put_owner(&ExecutionOwner {
            execution_id: "unversioned-owner".into(),
            scope: OwnerScope::host("job:score"),
            pins: drive.input.pins(),
            attempts: vec![],
            opened_at: rig.catalog().now().unwrap(),
        })
        .unwrap();
    assert!(rig
        .engine
        .drive(
            &drive,
            &mut |_| panic!("unversioned input refused"),
            &mut |_| panic!("unversioned output refused")
        )
        .is_err());
    assert_eq!(entered.load(Ordering::SeqCst), 0);
}

#[test]
fn isolated_items_pin_semantic_dependencies_before_replaying_the_input_set() {
    use contextful_engine::drive::ItemPolicy;
    let rig = Rig::new();
    let entered = Arc::new(AtomicUsize::new(0));
    let paid = Arc::new(AtomicUsize::new(0));
    let policy = ItemPolicy {
        logical_key: "id".into(),
        entity_key: "id".into(),
        entity_namespace: "documents".into(),
        max_attempts: 3,
    };
    let mut original = Bodies::default();
    original
        .register_with_dependencies(
            "score",
            body(&entered, &paid),
            dependencies("prompt-a", "model-a", "policy-a"),
        )
        .unwrap();
    let original = original.get("score").unwrap();
    rig.engine
        .drive_items(
            &fire(original.as_ref(), "first"),
            &policy,
            &mut read,
            &mut |_, _| Err(Failure::new(FailureTag::Transient, "hold")),
        )
        .unwrap();
    let mut changed = Bodies::default();
    changed
        .register_with_dependencies(
            "score",
            body(&entered, &paid),
            dependencies("prompt-b", "model-a", "policy-a"),
        )
        .unwrap();
    let changed = changed.get("score").unwrap();
    assert!(rig
        .engine
        .drive_items(
            &fire(changed.as_ref(), "changed"),
            &policy,
            &mut |_| panic!("changed item semantics read nothing"),
            &mut |_, _| panic!("changed semantics land nothing")
        )
        .is_err());
    assert_eq!(entered.load(Ordering::SeqCst), 1);
    assert_eq!(paid.load(Ordering::SeqCst), 1);
    assert!(rig
        .engine
        .drive_items(
            &fire(original.as_ref(), "resume"),
            &policy,
            &mut |_| panic!("recorded input replays"),
            &mut |_, _| Ok(Landed::default())
        )
        .unwrap()
        .succeeded());
    assert_eq!(paid.load(Ordering::SeqCst), 1);
}
