use crate::server;
use contextful_core::coordinate::{Cas, Catalog, CursorRow, Lease, LeaseKey, LeaseRow};
use contextful_core::run::own::{ExecutionOwner, OwnerScope, PlanPins};
use contextful_core::run::ports::{ExecutionPort, OpenExecution};
use contextful_core::run::record::RunRow;
use contextful_core::run::retry::Schedule;
use contextful_core::run::{Failure, RunError};
use contextful_core::time::Instant;
use contextful_engine::cancel::Keeper;
use contextful_engine::{Engine, Journal};
use contextful_engine::stores::{FileBlobStore, FileJournalStore};
use contextful_pg::PgCatalog;
use std::collections::BTreeMap;
use std::sync::{
    atomic::{AtomicBool, AtomicUsize, Ordering},
    Arc, Mutex,
};

struct RaceCatalog {
    db: Arc<PgCatalog>,
    thread: std::thread::ThreadId,
    armed: AtomicBool,
    reads: AtomicUsize,
    replacement: Mutex<Option<ExecutionOwner>>,
}
impl Catalog for RaceCatalog {
    fn supports_strict_execution_guards(&self) -> bool {
        self.db.supports_strict_execution_guards()
    }
    fn execution_guard(
        &self,
        a: &contextful_core::coordinate::ExecutionAdmission,
    ) -> Result<Box<dyn contextful_core::coordinate::ExecutionGuard>, Failure> {
        self.db.execution_guard(a)
    }
    fn now(&self) -> Result<Instant, Failure> {
        self.db.now()
    }
    fn acquire(&self, k: &LeaseKey, h: &str, t: u64) -> Result<Option<Lease>, Failure> {
        self.db.acquire(k, h, t)
    }
    fn release(&self, l: &Lease) -> Result<(), Failure> {
        self.db.release(l)
    }
    fn renew(&self, l: &Lease, t: u64) -> Result<Option<Lease>, Failure> {
        self.db.renew(l, t)
    }
    fn lease_holds(&self, l: &Lease) -> Result<bool, Failure> {
        self.db.lease_holds(l)
    }
    fn lease_row(&self, k: &LeaseKey) -> Result<LeaseRow, Failure> {
        self.db.lease_row(k)
    }
    fn cursor_at(&self, s: &OwnerScope) -> Result<CursorRow, Failure> {
        self.db.cursor_at(s)
    }
    fn cursor_cas_at(
        &self,
        s: &OwnerScope,
        v: u64,
        n: CursorRow,
        f: Option<&Lease>,
    ) -> Result<Cas, Failure> {
        self.db.cursor_cas_at(s, v, n, f)
    }
    fn owner_at(&self, s: &OwnerScope) -> Result<Option<ExecutionOwner>, Failure> {
        self.db.owner_at(s)
    }
    fn put_owner(&self, o: &ExecutionOwner) -> Result<(), Failure> {
        self.db.put_owner(o)
    }
    fn retire_at(
        &self,
        s: &OwnerScope,
        x: &str,
        c: Option<(CursorRow, u64)>,
        f: Option<&Lease>,
    ) -> Result<Cas, Failure> {
        self.db.retire_at(s, x, c, f)
    }
    fn retired_at(&self, s: &OwnerScope) -> Result<Option<String>, Failure> {
        self.db.retired_at(s)
    }
    fn put_run(&self, r: &RunRow) -> Result<(), Failure> {
        self.db.put_run(r)
    }
    fn run(&self, id: &str) -> Result<Option<RunRow>, Failure> {
        let row = self.db.run(id)?;
        if id == "old"
            && self.thread == std::thread::current().id()
            && self.armed.load(Ordering::SeqCst)
            && self.reads.fetch_add(1, Ordering::SeqCst) == 1
        {
            self.db
                .put_owner(self.replacement.lock().unwrap().as_ref().unwrap())?;
        }
        Ok(row)
    }
    fn runs(&self, p: Option<&str>) -> Result<Vec<RunRow>, Failure> {
        self.db.runs(p)
    }
    fn update_run(
        &self,
        id: &str,
        f: &mut dyn FnMut(&mut RunRow) -> Result<(), RunError>,
    ) -> Result<Option<Result<RunRow, RunError>>, Failure> {
        self.db.update_run(id, f)
    }
}

#[test]
fn takeover_after_the_last_cooperative_read_refuses_provider_dispatch() {
    let Some(server) = server::start() else {
        return;
    };
    let db = Arc::new(PgCatalog::connect(&server.conninfo).unwrap());
    let gate = Arc::new(RaceCatalog {
        db: db.clone(),
        thread: std::thread::current().id(),
        armed: AtomicBool::new(false),
        reads: AtomicUsize::new(0),
        replacement: Mutex::new(None),
    });
    let dir = tempfile::tempdir().unwrap();
    let engine = Engine {
        catalog: gate.clone(),
        journal: Journal::over(FileJournalStore::open(dir.path()), FileBlobStore::open(dir.path())),
        awakeables: None,
        keeper: Keeper::default(),
        emitter: None,
        worlds: Vec::new(),
    };
    let scope = OwnerScope::host("dispatch-race");
    let mut x = engine
        .open_execution(&OpenExecution {
            scope: scope.clone(),
            pins: PlanPins {
                plan_ref: "plan".into(),
                identities: BTreeMap::new(),
            }
            .into(),
            run_id: "old".into(),
            site_id: "site".into(),
            pid: 1,
            boot_id: "boot".into(),
            trace_id: None,
            connector: None,
            schedule: Schedule::single_attempt(),
        })
        .unwrap();
    let mut replacement = db.owner_at(&scope).unwrap().unwrap();
    replacement.execution_id = "replacement".into();
    replacement.attempts = vec!["replacement".into()];
    *gate.replacement.lock().unwrap() = Some(replacement);
    gate.armed.store(true, Ordering::SeqCst);
    let calls = AtomicUsize::new(0);
    let result = x.step("provider", b"input", &mut |_| {
        calls.fetch_add(1, Ordering::SeqCst);
        Ok(Vec::new())
    });
    assert!(
        result.is_err(),
        "a takeover between the final read and callback must refuse dispatch"
    );
    assert_eq!(calls.load(Ordering::SeqCst), 0);
}

// spec: run.journal.guard-callback@b1964147
// spec: run.journal.durable-dispatch@2044517c
#[test]
fn durable_guard_serializes_takeover_and_keeps_run_metadata_reentrant() {
    use contextful_core::coordinate::{ExecutionAdmission, ExecutionExpectation};
    use std::sync::mpsc;
    use std::time::Duration;
    let Some(server) = server::start() else {
        return;
    };
    let db = Arc::new(PgCatalog::connect(&server.conninfo).unwrap());
    let dir = tempfile::tempdir().unwrap();
    let engine = Engine {
        catalog: db.clone(),
        journal: Journal::over(FileJournalStore::open(dir.path()), FileBlobStore::open(dir.path())),
        awakeables: None,
        keeper: Keeper::default(),
        emitter: None,
        worlds: Vec::new(),
    };
    let scope = OwnerScope::host("guarded");
    let _x = engine
        .open_execution(&OpenExecution {
            scope: scope.clone(),
            pins: PlanPins {
                plan_ref: "plan".into(),
                identities: BTreeMap::new(),
            }
            .into(),
            run_id: "old".into(),
            site_id: "site".into(),
            pid: 1,
            boot_id: "boot".into(),
            trace_id: None,
            connector: None,
            schedule: Schedule::single_attempt(),
        })
        .unwrap();
    let owner = db.owner_at(&scope).unwrap().unwrap();
    let lease_key = LeaseKey::Pipeline("entity".into());
    let lease = db.acquire(&lease_key, "old", 1).unwrap().unwrap();
    let admission = ExecutionAdmission::new(
        vec![ExecutionExpectation {
            scope: scope.clone(),
            execution_id: owner.execution_id.clone(),
            pins: owner.pins.clone(),
            run_id: "old".into(),
        }],
        vec![lease.clone()],
        false,
    )
    .unwrap();
    let replacement_owner = db.clone();
    let replacement_lease = PgCatalog::connect(&server.conninfo).unwrap();
    let guard = db.execution_guard(&admission).unwrap();
    assert!(db.supports_strict_execution_guards());
    assert!(db.put_owner(&owner).is_err());
    assert!(db
        .retire_at(&scope, &owner.execution_id, None, None)
        .is_err());
    assert!(db.release(&lease).is_err());
    let nested = db.execution_guard(&admission).unwrap();
    drop(nested);
    let extra = ExecutionAdmission::new(
        Vec::new(),
        vec![Lease {
            key: "new".into(),
            ..lease.clone()
        }],
        false,
    )
    .unwrap();
    assert!(db.execution_guard(&extra).is_err());
    assert!(db
        .update_run("old", &mut |r| {
            r.rows = 7;
            Ok(())
        })
        .unwrap()
        .unwrap()
        .is_ok());
    let (sent, received) = mpsc::channel();
    let mut next = owner.clone();
    next.execution_id = "new".into();
    next.attempts = vec!["new".into()];
    let writer = std::thread::spawn(move || {
        replacement_owner.put_owner(&next).unwrap();
        sent.send(()).unwrap();
    });
    assert!(received.recv_timeout(Duration::from_millis(150)).is_err());
    let metadata = db.clone();
    let (metadata_sent, metadata_received) = mpsc::channel();
    let metadata_writer = std::thread::spawn(move || {
        let result = metadata.update_run("old", &mut |r| {
            r.rows = 8;
            Ok(())
        });
        let _ = metadata_sent.send(result);
    });
    let metadata_result = metadata_received.recv_timeout(Duration::from_secs(2));
    if metadata_result.is_err() {
        drop(guard);
        writer.join().unwrap();
        metadata_writer.join().unwrap();
        panic!("owner lock wait must not block callback run metadata through the shared catalog client");
    }
    assert!(metadata_result.unwrap().unwrap().unwrap().is_ok());
    metadata_writer.join().unwrap();
    // Expiry does not let takeover pass an in-flight bounded callback.
    std::thread::sleep(Duration::from_millis(1100));
    let (sent_lease, received_lease) = mpsc::channel();
    let taker = std::thread::spawn(move || {
        let next = replacement_lease
            .acquire(&lease_key, "new", 60)
            .unwrap()
            .unwrap();
        sent_lease.send(next).unwrap();
    });
    assert!(received_lease
        .recv_timeout(Duration::from_millis(150))
        .is_err());
    drop(guard);
    received.recv_timeout(Duration::from_secs(5)).unwrap();
    let next = received_lease.recv_timeout(Duration::from_secs(5)).unwrap();
    assert!(next.fence > lease.fence);
    writer.join().unwrap();
    taker.join().unwrap();
    assert!(db.execution_guard(&admission).is_err());
}

#[test]
fn actual_destination_commit_retains_admission_after_precommit() {
    use contextful_core::run::plan::Plan;
    use contextful_core::run::ports::{
        Cancellation, Commit, Destination, Landed, Marker, Part, PullRequest, Source, Stage,
    };
    use std::sync::mpsc;
    use std::time::Duration;
    struct Input;
    impl Source for Input {
        fn pull(&mut self, _: &PullRequest, _: &dyn Cancellation) -> Result<Vec<u8>, Failure> {
            Ok(br#"{"rows":[{"id":"a","updated_at":1}]}"#.to_vec())
        }
    }
    struct Output {
        db: Arc<PgCatalog>,
        observer: postgres::Client,
        writer: Option<std::thread::JoinHandle<()>>,
        published: bool,
    }
    impl Destination for Output {
        fn stage_batch(&mut self, s: Stage) -> Result<Part, Failure> {
            Ok(Part {
                name: "part".into(),
                rows: s.rows.len() as u64,
                bytes: 1,
            })
        }
        fn commit(
            &mut self,
            _: Commit,
            precommit: &dyn Fn() -> Result<(), Failure>,
        ) -> Result<Landed, Failure> {
            precommit()?;
            let mut replacement = self
                .db
                .owner_at(&OwnerScope::table("feed", "filings"))?
                .unwrap();
            replacement.execution_id = "replacement".into();
            replacement.attempts = vec!["replacement".into()];
            let db = self.db.clone();
            let (send, receive) = mpsc::channel();
            self.writer = Some(std::thread::spawn(move || {
                db.put_owner(&replacement).unwrap();
                let _ = send.send(());
            }));
            assert!(
                receive.recv_timeout(Duration::from_millis(150)).is_err(),
                "takeover cannot pass between precommit and the actual destination publication"
            );
            let deadline=std::time::Instant::now()+Duration::from_secs(2);
            loop {
                let blocked:bool=self.observer.query_one("SELECT EXISTS (SELECT 1 FROM pg_stat_activity WHERE datname=current_database() AND wait_event_type='Lock' AND query LIKE 'UPDATE contextful_lease SET expires_us%')",&[]).unwrap().get(0);
                if blocked { break; }
                assert!(std::time::Instant::now()<deadline,"the real keeper renewal must reach the guard's lease lock");
                std::thread::sleep(Duration::from_millis(5));
            }
            // The fast keeper's lease renewal now waits behind this guard. A final
            // precommit still reads the held lease without waiting on that renewal.
            precommit()?;
            self.published = true;
            Ok(Landed { rows: 1, bytes: 1 })
        }
        fn discard(&mut self, _: &str, _: &str) -> Result<(), Failure> {
            Ok(())
        }
        fn newest_marker(&self, _: &str, _: &str) -> Result<Option<Marker>, Failure> {
            Ok(None)
        }
    }
    let Some(server) = server::start() else {
        return;
    };
    let db = Arc::new(PgCatalog::connect(&server.conninfo).unwrap());
    let dir = tempfile::tempdir().unwrap();
    let engine = Engine {
        catalog: db.clone(),
        journal: Journal::over(FileJournalStore::open(dir.path()), FileBlobStore::open(dir.path())),
        awakeables: None,
        keeper: Keeper::new(contextful_engine::cancel::Cadence { poll:Duration::from_millis(5),renew:Duration::from_millis(5) }),
        emitter: None,
        worlds: Vec::new(),
    };
    let plan=Plan::compile(b"pipeline = \"feed\"\ntable = \"filings\"\n[connector]\nid = \"vendor\"\nversion = \"1.0.0\"\ncommand = [\"vendor\"]\n[cursor]\nkind = \"opaque-token\"\n").unwrap();
    let spec = contextful_engine::RunSpec {
        connector: plan.connector_pin("artifact"),
        plan,
        run_id: "publish".into(),
        site_id: "site".into(),
        pid: 1,
        boot_id: "boot".into(),
        trace_id: None,
    };
    let mut output = Output {
        db,
        observer:postgres::Client::connect(&server.conninfo,postgres::NoTls).unwrap(),
        writer: None,
        published: false,
    };
    let (send,receive)=mpsc::channel();
    let running=std::thread::spawn(move || {
        let row = engine.run(&spec, &mut Input, &mut output).unwrap();
        output.writer.take().unwrap().join().unwrap();
        send.send((row,output.published)).unwrap();
    });
    let (row,published)=receive.recv_timeout(Duration::from_secs(8)).expect("guarded commit and keeper renewal must not deadlock");
    running.join().unwrap();
    assert!(published);
    assert_eq!(row.status, contextful_core::run::record::RunStatus::Success);
}
