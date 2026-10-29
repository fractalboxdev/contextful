use crate::support::Rig;
use contextful_core::coordinate::{Cas, Catalog, CursorRow, Lease, LeaseKey, LeaseRow};
use contextful_core::run::own::{ExecutionOwner, OwnerScope, PlanPins};
use contextful_core::run::ports::{ExecutionPort, OpenExecution};
use contextful_core::run::record::RunRow;
use contextful_core::run::retry::Schedule;
use contextful_core::run::{Failure, FailureTag, RunError};
use contextful_core::time::Instant;
use std::collections::BTreeMap;
use std::sync::{
    atomic::{AtomicBool, AtomicUsize, Ordering},
    Arc,
};

struct Gate {
    inner: Arc<dyn Catalog + Send + Sync>,
    held: Arc<AtomicUsize>,
    refuse: AtomicBool,
    parent: Option<OwnerScope>,
}
struct Held(Arc<AtomicUsize>);
impl contextful_core::coordinate::ExecutionGuard for Held {}
impl Drop for Held {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}
impl Catalog for Gate {
    fn supports_strict_execution_guards(&self) -> bool {
        true
    }
    fn execution_guard(
        &self,
        admission: &contextful_core::coordinate::ExecutionAdmission,
    ) -> Result<Box<dyn contextful_core::coordinate::ExecutionGuard>, Failure> {
        if let Some(parent) = &self.parent {
            if admission.publication {
                assert!(admission.owners().iter().any(|owner| &owner.scope == parent), "actual publication omits its prepared parent from durable admission");
            }
        } else {
            assert_eq!(admission.owners().len(), 1);
            assert_eq!(admission.owners()[0].run_id, "guarded-1");
        }
        if self.refuse.load(Ordering::SeqCst) {
            return Err(Failure::new(
                FailureTag::Transient,
                "durable admission moved",
            ));
        }
        self.held.fetch_add(1, Ordering::SeqCst);
        Ok(Box::new(Held(self.held.clone())))
    }
    fn now(&self) -> Result<Instant, Failure> {
        self.inner.now()
    }
    fn acquire(&self, k: &LeaseKey, h: &str, t: u64) -> Result<Option<Lease>, Failure> {
        self.inner.acquire(k, h, t)
    }
    fn release(&self, l: &Lease) -> Result<(), Failure> {
        self.inner.release(l)
    }
    fn renew(&self, l: &Lease, t: u64) -> Result<Option<Lease>, Failure> {
        self.inner.renew(l, t)
    }
    fn lease_holds(&self, l: &Lease) -> Result<bool, Failure> {
        self.inner.lease_holds(l)
    }
    fn lease_row(&self, k: &LeaseKey) -> Result<LeaseRow, Failure> {
        self.inner.lease_row(k)
    }
    fn cursor_at(&self, s: &OwnerScope) -> Result<CursorRow, Failure> {
        self.inner.cursor_at(s)
    }
    fn cursor_cas_at(
        &self,
        s: &OwnerScope,
        v: u64,
        n: CursorRow,
        f: Option<&Lease>,
    ) -> Result<Cas, Failure> {
        self.inner.cursor_cas_at(s, v, n, f)
    }
    fn owner_at(&self, s: &OwnerScope) -> Result<Option<ExecutionOwner>, Failure> {
        self.inner.owner_at(s)
    }
    fn put_owner(&self, o: &ExecutionOwner) -> Result<(), Failure> {
        self.inner.put_owner(o)
    }
    fn retire_at(
        &self,
        s: &OwnerScope,
        x: &str,
        c: Option<(CursorRow, u64)>,
        f: Option<&Lease>,
    ) -> Result<Cas, Failure> {
        self.inner.retire_at(s, x, c, f)
    }
    fn retired_at(&self, s: &OwnerScope) -> Result<Option<String>, Failure> {
        self.inner.retired_at(s)
    }
    fn put_run(&self, r: &RunRow) -> Result<(), Failure> {
        self.inner.put_run(r)
    }
    fn run(&self, id: &str) -> Result<Option<RunRow>, Failure> {
        let row = self.inner.run(id)?;
        Ok(row)
    }
    fn runs(&self, p: Option<&str>) -> Result<Vec<RunRow>, Failure> {
        self.inner.runs(p)
    }
    fn update_run(
        &self,
        id: &str,
        f: &mut dyn FnMut(&mut RunRow) -> Result<(), RunError>,
    ) -> Result<Option<Result<RunRow, RunError>>, Failure> {
        self.inner.update_run(id, f)
    }
}

pub(crate) fn for_parent(inner: Arc<dyn Catalog + Send + Sync>, parent: OwnerScope) -> (Arc<dyn Catalog + Send + Sync>, Arc<AtomicUsize>) {
    let held = Arc::new(AtomicUsize::new(0));
    (Arc::new(Gate { inner, held: held.clone(), refuse: AtomicBool::new(false), parent: Some(parent) }), held)
}

#[test]
fn provider_callback_retains_the_catalog_guard_and_a_refusal_dispatches_nothing() {
    let mut rig = Rig::new();
    let held = Arc::new(AtomicUsize::new(0));
    let gate = Arc::new(Gate {
        inner: rig.engine.catalog.clone(),
        held: held.clone(),
        refuse: AtomicBool::new(false),
        parent: None,
    });
    rig.engine.catalog = gate.clone();
    let mut execution = rig
        .engine
        .open_execution(&OpenExecution {
            scope: OwnerScope::host("guarded"),
            pins: PlanPins {
                plan_ref: "plan".into(),
                identities: BTreeMap::new(),
            }
            .into(),
            run_id: "guarded-1".into(),
            site_id: "site".into(),
            pid: 1,
            boot_id: "boot".into(),
            trace_id: None,
            connector: None,
            schedule: Schedule::single_attempt(),
        })
        .unwrap();
    let calls = AtomicUsize::new(0);
    execution
        .step("first", b"input", &mut |_| {
            assert_eq!(
                held.load(Ordering::SeqCst),
                1,
                "admission remains held across the actual callback"
            );
            calls.fetch_add(1, Ordering::SeqCst);
            Ok(b"recorded".to_vec())
        })
        .unwrap();
    assert_eq!(held.load(Ordering::SeqCst), 0);
    gate.refuse.store(true, Ordering::SeqCst);
    assert!(execution
        .step("second", b"input", &mut |_| {
            calls.fetch_add(1, Ordering::SeqCst);
            Ok(b"wrong".to_vec())
        })
        .is_err());
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}

/// The journal rows a released guard found for its entry: `true` once that entry held a
/// recorded value when the guard dropped.
type Seen = Arc<std::sync::Mutex<Vec<(contextful_core::run::journal::EntryKey, Option<u64>, bool)>>>;

/// A catalog whose guards note the effect each admission names and, when dropped, whether
/// the journal already recorded that effect's value.
struct Recording {
    gate: Gate,
    dir: std::path::PathBuf,
    seen: Seen,
}
struct Noting {
    dir: std::path::PathBuf,
    entry: contextful_core::run::journal::EntryKey,
    window: Option<u64>,
    seen: Seen,
}
impl contextful_core::coordinate::ExecutionGuard for Noting {}
impl Drop for Noting {
    fn drop(&mut self) {
        use contextful_core::run::ports::JournalStore;
        let row = contextful_engine::stores::FileJournalStore::open(&self.dir).read(&self.entry).unwrap();
        let recorded = matches!(row, Some(contextful_core::run::journal::Row::Recorded { .. }));
        self.seen.lock().unwrap().push((self.entry.clone(), self.window, recorded));
    }
}

/// A step's guard names the step's journal entry and a `none` window, and the holder deletes
/// it when the callback returns, before the journal records the callback's value.
// spec: run.callback-guard.release@6750f8fe
#[test]
fn a_step_guard_names_its_entry_and_releases_before_the_value_is_journaled() {
    let mut rig = Rig::new();
    let seen: Seen = Arc::default();
    let recording = Arc::new(Recording {
        gate: Gate { inner: rig.engine.catalog.clone(), held: Arc::new(AtomicUsize::new(0)), refuse: AtomicBool::new(false), parent: None },
        dir: rig.dir.path().to_path_buf(),
        seen: seen.clone(),
    });
    rig.engine.catalog = recording.clone();
    let mut execution = rig
        .engine
        .open_execution(&OpenExecution {
            scope: OwnerScope::host("guarded"),
            pins: PlanPins { plan_ref: "plan".into(), identities: BTreeMap::new() }.into(),
            run_id: "guarded-1".into(),
            site_id: "site".into(),
            pid: 1,
            boot_id: "boot".into(),
            trace_id: None,
            connector: None,
            schedule: Schedule::single_attempt(),
        })
        .unwrap();
    execution.step("first", b"input", &mut |_| Ok(b"recorded".to_vec())).unwrap();
    let seen = seen.lock().unwrap();
    assert_eq!(seen.len(), 1, "one guarded callback");
    let (entry, window, recorded) = &seen[0];
    assert_eq!(entry.step_label, "first");
    assert_eq!(entry, &contextful_core::run::journal::EntryKey::new(&entry.execution_id, "first", b"input"));
    assert_eq!(*window, None);
    assert!(!recorded, "the guard released before the journal recorded the value");
}

impl Catalog for Recording {
    fn supports_strict_execution_guards(&self) -> bool {
        true
    }
    fn execution_guard(
        &self,
        admission: &contextful_core::coordinate::ExecutionAdmission,
    ) -> Result<Box<dyn contextful_core::coordinate::ExecutionGuard>, Failure> {
        let effect = admission.effect().expect("a guarded step names its effect").clone();
        drop(self.gate.execution_guard(admission)?);
        Ok(Box::new(Noting { dir: self.dir.clone(), entry: effect.entry, window: effect.window_secs, seen: self.seen.clone() }))
    }
    fn now(&self) -> Result<Instant, Failure> { self.gate.now() }
    fn acquire(&self, k: &LeaseKey, h: &str, t: u64) -> Result<Option<Lease>, Failure> { self.gate.acquire(k, h, t) }
    fn release(&self, l: &Lease) -> Result<(), Failure> { self.gate.release(l) }
    fn renew(&self, l: &Lease, t: u64) -> Result<Option<Lease>, Failure> { self.gate.renew(l, t) }
    fn lease_holds(&self, l: &Lease) -> Result<bool, Failure> { self.gate.lease_holds(l) }
    fn lease_row(&self, k: &LeaseKey) -> Result<LeaseRow, Failure> { self.gate.lease_row(k) }
    fn cursor_at(&self, s: &OwnerScope) -> Result<CursorRow, Failure> { self.gate.cursor_at(s) }
    fn cursor_cas_at(&self, s: &OwnerScope, v: u64, n: CursorRow, f: Option<&Lease>) -> Result<Cas, Failure> { self.gate.cursor_cas_at(s, v, n, f) }
    fn owner_at(&self, s: &OwnerScope) -> Result<Option<ExecutionOwner>, Failure> { self.gate.owner_at(s) }
    fn put_owner(&self, o: &ExecutionOwner) -> Result<(), Failure> { self.gate.put_owner(o) }
    fn retire_at(&self, s: &OwnerScope, x: &str, c: Option<(CursorRow, u64)>, f: Option<&Lease>) -> Result<Cas, Failure> { self.gate.retire_at(s, x, c, f) }
    fn retired_at(&self, s: &OwnerScope) -> Result<Option<String>, Failure> { self.gate.retired_at(s) }
    fn put_run(&self, r: &RunRow) -> Result<(), Failure> { self.gate.put_run(r) }
    fn run(&self, id: &str) -> Result<Option<RunRow>, Failure> { self.gate.run(id) }
    fn runs(&self, p: Option<&str>) -> Result<Vec<RunRow>, Failure> { self.gate.runs(p) }
    fn update_run(&self, id: &str, f: &mut dyn FnMut(&mut RunRow) -> Result<(), RunError>) -> Result<Option<Result<RunRow, RunError>>, Failure> { self.gate.update_run(id, f) }
}

/// Swap the rig's catalog for one noting every guard's effect, answering what it noted.
fn noting(rig: &mut Rig) -> Seen {
    let seen: Seen = Arc::default();
    rig.engine.catalog = Arc::new(Recording {
        gate: Gate { inner: rig.engine.catalog.clone(), held: Arc::new(AtomicUsize::new(0)), refuse: AtomicBool::new(false), parent: None },
        dir: rig.dir.path().to_path_buf(),
        seen: seen.clone(),
    });
    seen
}

/// The window each noted guard of a step labelled by `label` recorded.
fn windows(seen: &Seen, label: &dyn Fn(&str) -> bool) -> Vec<Option<u64>> {
    seen.lock().unwrap().iter().filter(|(entry, _, _)| label(&entry.step_label)).map(|(_, window, _)| *window).collect()
}

/// A plan's connector declares its destination's window, and each pull's guard records it; an
/// undeclared connector's pulls record `none`.
// spec: connector.declare-capability.dedup-window@b04e69cb
#[test]
fn each_pull_guard_records_the_window_its_connector_declares() {
    use crate::support::{plan, three_pages, Pages, Sink};
    let pull = |label: &str| label != "publication";
    let mut rig = Rig::new();
    let seen = noting(&mut rig);
    let text = "pipeline = \"feed\"\ntable = \"filings\"\n[connector]\nid = \"vendor\"\nversion = \"1.0.0\"\ncommand = [\"vendor\"]\ndedup_window_secs = 600\n[cursor]\nkind = \"opaque-token\"\n";
    let windowed = contextful_core::run::plan::Plan::compile(text.as_bytes()).unwrap();
    rig.run(&windowed, "1.0.0", "guarded-1", &mut Pages::new(three_pages()), &mut Sink::default()).unwrap();
    let declared = windows(&seen, &pull);
    assert!(!declared.is_empty(), "the run's pulls took guards");
    assert!(declared.iter().all(|w| *w == Some(600)), "{declared:?}");

    let mut rig = Rig::new();
    let seen = noting(&mut rig);
    rig.run(&plan("kind = \"opaque-token\"", ""), "1.0.0", "guarded-1", &mut Pages::new(three_pages()), &mut Sink::default()).unwrap();
    let undeclared = windows(&seen, &pull);
    assert!(!undeclared.is_empty(), "the run's pulls took guards");
    assert!(undeclared.iter().all(Option::is_none), "{undeclared:?}");
}

/// One paid call through a body that names the delegate's window.
struct Windowed;
impl contextful_core::run::drive::RowBody for Windowed {
    fn run(&self, row: &contextful_core::run::drive::InputRow, calls: &dyn contextful_core::run::drive::RowCalls) -> Result<contextful_core::run::drive::Emitted, contextful_core::run::drive::RowStop> {
        calls.call_within("delegate/1", row.key.as_bytes(), Some(900), &mut |_| Ok(b"answer".to_vec()))?;
        calls.call("local", row.key.as_bytes(), &mut |_| Ok(b"local".to_vec()))?;
        Ok(BTreeMap::new())
    }
}

/// A row body names the window of the destination a call reaches, and that call's guard records
/// it while the body's other steps record `none`.
// spec: surface.delegate.dedup-window@10ae0bc6
#[test]
fn a_row_call_guard_records_the_window_its_body_names() {
    use contextful_core::run::drive::{InputSet, StoreInput};
    let mut rig = Rig::new();
    let seen = noting(&mut rig);
    let drive = contextful_engine::drive::Drive {
        job: "windowed".into(),
        input: StoreInput { body: "windowed".into(), statement: "SELECT 1".into(), as_of: Some(crate::support::T0.into()), sweep: None, structural_identity: None },
        body: &Windowed,
        max_in_flight: 1,
        run_id: "guarded-1".into(),
        site_id: "site".into(),
        pid: 1,
        boot_id: "boot".into(),
        schedule: Schedule::single_attempt(),
        poll: std::time::Duration::from_millis(10),
    };
    let row = serde_json::json!({ "id": "a" }).as_object().cloned().unwrap();
    let mut read = |as_of: &str| Ok(InputSet { as_of: as_of.into(), snapshots: BTreeMap::new(), rows: vec![row.clone()] });
    rig.engine.drive(&drive, &mut read, &mut |_| Ok(contextful_core::run::ports::Landed::default())).unwrap();
    let called = windows(&seen, &|label| label.ends_with("/delegate/1"));
    assert_eq!(called, [Some(900)], "the delegate call's guard records its window");
    let local = windows(&seen, &|label| label.ends_with("/local"));
    assert_eq!(local, [None], "a call naming no window records `none`");
}
