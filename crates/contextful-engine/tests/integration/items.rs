use crate::support::{Rig, T0};
use contextful_core::run::drive::{
    Emitted, InputRow, InputSet, RowBody, RowCalls, RowStop, StoreInput,
};
use contextful_core::run::ports::Landed;
use contextful_core::run::retry::Schedule;
use contextful_core::run::{Failure, FailureTag};
use contextful_engine::drive::{Drive, ItemPolicy, ItemStatus};
use serde_json::json;
use std::collections::BTreeMap;
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Mutex,
};
use std::time::Duration;

struct Body {
    calls: Mutex<Vec<String>>,
    entered: AtomicUsize,
}
impl RowBody for Body {
    fn run(&self, row: &InputRow, calls: &dyn RowCalls) -> Result<Emitted, RowStop> {
        self.entered.fetch_add(1, Ordering::SeqCst);
        let id = row.row["id"].as_str().unwrap();
        calls.call("paid", id.as_bytes(), &mut |_| {
            self.calls.lock().unwrap().push(id.into());
            Ok(id.as_bytes().to_vec())
        })?;
        if id == "bad" {
            return Err(Failure::deterministic(FailureTag::Permanent, "invalid item").into());
        }
        if id == "parked" {
            return Err(RowStop::Parked);
        }
        Ok(BTreeMap::from([("output".into(), vec![row.row.clone()])]))
    }
}
fn fire<'a>(body: &'a Body, run: &str) -> Drive<'a> {
    Drive {
        job: "score".into(),
        input: StoreInput {
            body: "score".into(),
            statement: "select * from docs".into(),
            as_of: None,
        },
        body,
        max_in_flight: 2,
        run_id: run.into(),
        site_id: "site".into(),
        pid: 42,
        boot_id: "boot".into(),
        schedule: Schedule::single_attempt(),
        poll: Duration::ZERO,
    }
}
fn policy() -> ItemPolicy {
    ItemPolicy {
        logical_key: "id".into(),
        entity_key: "entity".into(),
        entity_namespace: "documents".into(),
        max_attempts: 2,
    }
}
fn input(ids: &[&str]) -> InputSet {
    InputSet {
        as_of: T0.into(),
        snapshots: BTreeMap::new(),
        rows: ids
            .iter()
            .map(|id| json!({"id":id,"entity":id}).as_object().unwrap().clone())
            .collect(),
    }
}

#[test]
fn item_failures_commit_healthy_siblings_and_reordered_retry_skips_paid_work() {
    let rig = Rig::new();
    let body = Body {
        calls: Mutex::default(),
        entered: AtomicUsize::new(0),
    };
    let mut landed = Vec::new();
    let mut land = |_: &str, out: &Emitted| {
        landed.push(out["output"][0]["id"].as_str().unwrap().to_string());
        Ok(Landed { rows: 1, bytes: 1 })
    };
    let first = rig
        .engine
        .drive_items(
            &fire(&body, "first"),
            &policy(),
            &mut |_| Ok(input(&["good-a", "bad", "good-b"])),
            &mut land,
        )
        .unwrap();
    assert!(!first.succeeded());
    assert_eq!(
        first
            .items
            .iter()
            .filter(|i| matches!(i.status, ItemStatus::Succeeded(_)))
            .count(),
        2
    );
    assert_eq!(
        first
            .items
            .iter()
            .filter(|i| matches!(i.status, ItemStatus::Failed(_)))
            .count(),
        1
    );
    let next = rig
        .engine
        .drive_items(
            &fire(&body, "second"),
            &policy(),
            &mut |_| Ok(input(&["bad", "good-b", "good-a"])),
            &mut land,
        )
        .unwrap();
    assert!(!next.succeeded());
    assert_eq!(body.calls.lock().unwrap().len(), 3);
    drop(land);
    landed.sort();
    assert_eq!(landed, ["good-a", "good-b"]);
    let ids = |result: &contextful_engine::drive::ItemReport| {
        result
            .items
            .iter()
            .map(|i| (i.logical_id.clone(), i.execution_id.clone()))
            .collect::<BTreeMap<_, _>>()
    };
    assert_eq!(ids(&first), ids(&next));
}

#[test]
fn item_parked_and_entity_overlap_report_individual_outcomes() {
    let rig = Rig::new();
    let body = Body {
        calls: Mutex::default(),
        entered: AtomicUsize::new(0),
    };
    let held = rig
        .catalog()
        .acquire(
            &contextful_engine::drive::entity_lease_key("documents", &json!("busy")),
            "other-job-v2",
            30,
        )
        .unwrap()
        .unwrap();
    let result = rig
        .engine
        .drive_items(
            &fire(&body, "first"),
            &policy(),
            &mut |_| Ok(input(&["busy", "parked", "good"])),
            &mut |_, _| Ok(Landed { rows: 1, bytes: 1 }),
        )
        .unwrap();
    assert_eq!(
        result
            .items
            .iter()
            .filter(|i| matches!(i.status, ItemStatus::Deferred))
            .count(),
        1
    );
    assert_eq!(
        result
            .items
            .iter()
            .filter(|i| matches!(i.status, ItemStatus::Parked))
            .count(),
        1
    );
    assert_eq!(
        result
            .items
            .iter()
            .filter(|i| matches!(i.status, ItemStatus::Succeeded(_)))
            .count(),
        1
    );
    assert!(!result.succeeded());
    assert_eq!(body.entered.load(Ordering::SeqCst), 2);
    rig.catalog().release(&held).unwrap();
}

#[test]
fn superseded_entity_fence_refuses_paid_effect_before_provider_dispatch() {
    struct Stale<'a> {
        rig: &'a Rig,
        called: AtomicUsize,
    }
    impl RowBody for Stale<'_> {
        fn run(&self, row: &InputRow, calls: &dyn RowCalls) -> Result<Emitted, RowStop> {
            self.rig.clock.advance(31);
            let key = contextful_engine::drive::entity_lease_key("documents", &row.row["entity"]);
            self.rig
                .catalog()
                .acquire(&key, "replacement", 30)
                .unwrap()
                .unwrap();
            calls.call("effect", b"input", &mut |_| {
                self.called.fetch_add(1, Ordering::SeqCst);
                Ok(vec![])
            })?;
            Ok(BTreeMap::new())
        }
    }
    let rig = Rig::new();
    let body = Stale {
        rig: &rig,
        called: AtomicUsize::new(0),
    };
    let normal = Body {
        calls: Mutex::default(),
        entered: AtomicUsize::new(0),
    };
    let drive = Drive {
        body: &body,
        ..fire(&normal, "stale")
    };
    let result = rig
        .engine
        .drive_items(
            &drive,
            &policy(),
            &mut |_| Ok(input(&["good"])),
            &mut |_, _| panic!("stale item lands nothing"),
        )
        .unwrap();
    assert!(!result.succeeded());
    assert_eq!(body.called.load(Ordering::SeqCst), 0);
}

#[test]
fn superseded_aggregate_parent_refuses_an_items_paid_callback() {
    struct Stale<'a> { rig: &'a Rig, called: AtomicUsize }
    impl RowBody for Stale<'_> {
        fn run(&self, _: &InputRow, calls: &dyn RowCalls) -> Result<Emitted, RowStop> {
            let catalog=self.rig.catalog();
            let mut parent=catalog.owner_at(&contextful_engine::drive::job_scope("score")).unwrap().unwrap();
            parent.execution_id="replacement".into();parent.attempts=vec!["replacement".into()];
            catalog.put_owner(&parent).unwrap();
            calls.call("effect",b"input",&mut |_| {self.called.fetch_add(1,Ordering::SeqCst);Ok(Vec::new())})?;
            Ok(BTreeMap::new())
        }
    }
    let rig=Rig::new();
    let body=Stale {rig:&rig,called:AtomicUsize::new(0)};
    let normal=Body {calls:Mutex::default(),entered:AtomicUsize::new(0)};
    let result=rig.engine.drive_items(&Drive {body:&body,..fire(&normal,"parent-stale")},&policy(),&mut |_| Ok(input(&["good"])),&mut |_,_| Ok(Landed::default()));
    assert!(result.is_err() || !result.unwrap().succeeded());
    assert_eq!(body.called.load(Ordering::SeqCst),0,"parent takeover must fence the paid item callback");
}

#[test]
fn independent_retries_keep_paid_results_and_stop_at_the_durable_attempt_bound() {
    struct Retry {
        paid: AtomicUsize,
        entered: AtomicUsize,
    }
    impl RowBody for Retry {
        fn run(&self, _: &InputRow, calls: &dyn RowCalls) -> Result<Emitted, RowStop> {
            self.entered.fetch_add(1, Ordering::SeqCst);
            calls.call("paid", b"constant", &mut |_| {
                self.paid.fetch_add(1, Ordering::SeqCst);
                Ok(vec![])
            })?;
            Err(Failure::new(FailureTag::Transient, "try again").into())
        }
    }
    let rig = Rig::new();
    let body = Retry {
        paid: AtomicUsize::new(0),
        entered: AtomicUsize::new(0),
    };
    let normal = Body {
        calls: Mutex::default(),
        entered: AtomicUsize::new(0),
    };
    for run in ["one", "two", "three"] {
        let drive = Drive {
            body: &body,
            ..fire(&normal, run)
        };
        let result = rig
            .engine
            .drive_items(
                &drive,
                &policy(),
                &mut |_| Ok(input(&["retry"])),
                &mut |_, _| panic!("failed item lands nothing"),
            )
            .unwrap();
        assert!(matches!(result.items[0].status, ItemStatus::Failed(_)));
    }
    assert_eq!(body.entered.load(Ordering::SeqCst), 2);
    assert_eq!(body.paid.load(Ordering::SeqCst), 1);
}

#[test]
fn another_job_version_defers_the_same_entity_while_unrelated_entities_progress() {
    struct Other<'a> {
        rig: &'a Rig,
        entered: AtomicUsize,
    }
    impl RowBody for Other<'_> {
        fn run(&self, _: &InputRow, _: &dyn RowCalls) -> Result<Emitted, RowStop> {
            self.entered.fetch_add(1, Ordering::SeqCst);
            let other = Body {
                calls: Mutex::default(),
                entered: AtomicUsize::new(0),
            };
            let mut drive = fire(&other, "other");
            drive.job = "another-job".into();
            drive.input.statement = "select * from docs where revision = 2".into();
            let report = self
                .rig
                .engine
                .drive_items(
                    &drive,
                    &policy(),
                    &mut |_| Ok(input(&["shared", "unrelated"])),
                    &mut |_, _| Ok(Landed::default()),
                )
                .unwrap();
            assert_eq!(
                report
                    .items
                    .iter()
                    .filter(|i| matches!(i.status, ItemStatus::Deferred))
                    .count(),
                1
            );
            assert_eq!(
                report
                    .items
                    .iter()
                    .filter(|i| matches!(i.status, ItemStatus::Succeeded(_)))
                    .count(),
                1
            );
            Ok(BTreeMap::new())
        }
    }
    let rig = Rig::new();
    let body = Other {
        rig: &rig,
        entered: AtomicUsize::new(0),
    };
    let normal = Body {
        calls: Mutex::default(),
        entered: AtomicUsize::new(0),
    };
    let drive = Drive {
        body: &body,
        ..fire(&normal, "first")
    };
    assert!(rig
        .engine
        .drive_items(
            &drive,
            &policy(),
            &mut |_| Ok(input(&["shared"])),
            &mut |_, _| Ok(Landed::default())
        )
        .unwrap()
        .succeeded());
    assert_eq!(body.entered.load(Ordering::SeqCst), 1);
}

#[test]
fn raw_item_publication_refuses_a_fence_transferred_during_staging() {
    let rig = Rig::new();
    let body = Body {
        calls: Mutex::default(),
        entered: AtomicUsize::new(0),
    };
    let mut sink = crate::support::Sink::default();
    let clock = rig.clock.clone();
    let catalog = rig.engine.catalog.clone();
    sink.on_stage = Some(Box::new(move |_| {
        clock.advance(31);
        catalog
            .acquire(
                &contextful_engine::drive::entity_lease_key("documents", &json!("good")),
                "replacement",
                30,
            )
            .unwrap()
            .unwrap();
    }));
    let result = rig
        .engine
        .drive_items_admitted(
            &fire(&body, "first"),
            &policy(),
            &mut |_| Ok(input(&["good"])),
            &mut |_, _, owner| {
                let plan = crate::support::plan("kind='opaque-token'", "journal=false");
                let spec = contextful_engine::RunSpec {
                    connector: plan.connector_pin("native"),
                    plan,
                    run_id: "output".into(),
                    site_id: "site".into(),
                    pid: 42,
                    boot_id: "boot".into(),
                    trace_id: None,
                };
                let mut source = crate::support::Pages::new(vec![vec![json!({"id":"good"})]]);
                let row = rig
                    .engine
                    .run_body_rows(&spec, owner, &mut source, &mut sink)
                    .map_err(|e| Failure::new(FailureTag::Storage, e.to_string()))?;
                if row.status != contextful_core::run::record::RunStatus::Success {
                    return Err(Failure::new(FailureTag::Transient, "fenced"));
                }
                Ok(Landed::default())
            },
        )
        .unwrap();
    assert!(!result.succeeded());
    assert!(sink.commits.is_empty());
}

#[test]
fn terminal_sibling_failure_does_not_pin_later_fires_to_old_input() {
    let rig = Rig::new();
    let body = Body {
        calls: Mutex::default(),
        entered: AtomicUsize::new(0),
    };
    let land = &mut |_: &str, _: &Emitted| Ok(Landed::default());
    assert!(!rig
        .engine
        .drive_items(
            &fire(&body, "first"),
            &policy(),
            &mut |_| Ok(input(&["good", "bad"])),
            land
        )
        .unwrap()
        .succeeded());
    let next = rig
        .engine
        .drive_items(
            &fire(&body, "second"),
            &policy(),
            &mut |_| Ok(input(&["good", "bad", "later"])),
            land,
        )
        .unwrap();
    assert_eq!(next.items.len(), 3);
    assert_eq!(body.calls.lock().unwrap().len(), 3);
}

#[test]
fn an_item_parent_stop_after_staging_keeps_publication_uncut() {
    let rig = Rig::new();
    let body = Body {
        calls: Mutex::default(),
        entered: AtomicUsize::new(0),
    };
    let catalog = rig.engine.catalog.clone();
    let mut sink = crate::support::Sink::default();
    sink.on_stage = Some(Box::new(move |_| {
        let parent = catalog
            .runs(None)
            .unwrap()
            .into_iter()
            .find(|row| {
                row.host_scope
                    .as_deref()
                    .is_some_and(|scope| scope.starts_with("item:"))
            })
            .unwrap();
        let now = catalog.now().unwrap();
        catalog
            .update_run(&parent.run_id, &mut |row| {
                contextful_core::run::cancel::mark(
                    row,
                    contextful_core::run::cancel::Scope::Run,
                    None,
                    now,
                )
            })
            .unwrap()
            .unwrap()
            .unwrap();
    }));
    let result = rig
        .engine
        .drive_items_admitted(
            &fire(&body, "late-stop"),
            &policy(),
            &mut |_| Ok(input(&["good"])),
            &mut |_, _, owner| {
                let plan = crate::support::plan("kind='opaque-token'", "journal=false");
                let spec = contextful_engine::RunSpec {
                    connector: plan.connector_pin("native"),
                    plan,
                    run_id: "output".into(),
                    site_id: "site".into(),
                    pid: 42,
                    boot_id: "boot".into(),
                    trace_id: None,
                };
                let mut source = crate::support::Pages::new(vec![vec![json!({"id":"good"})]]);
                let row = rig
                    .engine
                    .run_body_rows(&spec, owner, &mut source, &mut sink)
                    .map_err(|e| Failure::new(FailureTag::Storage, e.to_string()))?;
                if row.status != contextful_core::run::record::RunStatus::Success {
                    return Err(Failure::new(
                        FailureTag::Transient,
                        "publication cut after staging",
                    ));
                }
                Ok(Landed::default())
            },
        )
        .unwrap();
    assert!(result.succeeded());
    assert_eq!(sink.commits.len(), 1);
}
