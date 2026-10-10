//! `run.journal.retire-order`: a journal store apart from the catalog deletes a retired
//! owner's rows after the catalog commits, and the scope's next open deletes them again.

use crate::support::{SetClock, T0};
use contextful_core::coordinate::Catalog;
use contextful_core::run::journal::{EntryKey, Row, Stored};
use contextful_core::run::own::{OwnerPins, OwnerScope, PlanPins};
use contextful_core::run::ports::{ExecutionPort, JournalStore, OpenExecution};
use contextful_core::run::retry::Schedule;
use contextful_core::run::{Failure, FailureTag};
use contextful_engine::cancel::{Cadence, Keeper};
use contextful_engine::stores::{FileBlobStore, FileJournalStore};
use contextful_engine::{Engine, EngineError, Journal, LocalCatalog};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

const SCOPE: &str = "index-7";

/// A file journal whose next `failures` deletes fail, noting at each delete whether the
/// catalog still held the owner of [`SCOPE`].
#[derive(Clone)]
struct DeleteFails {
    inner: FileJournalStore,
    catalog: Arc<dyn Catalog + Send + Sync>,
    failures: Arc<AtomicUsize>,
    /// The scope's pending owner as each delete found it.
    seen: Arc<Mutex<Vec<Option<String>>>>,
}

impl JournalStore for DeleteFails {
    fn create_pending(&self, key: &EntryKey, run_id: &str) -> Result<bool, Failure> {
        self.inner.create_pending(key, run_id)
    }
    fn read(&self, key: &EntryKey) -> Result<Option<Row>, Failure> {
        self.inner.read(key)
    }
    fn replace_if_pending(&self, key: &EntryKey, holder: &str, run_id: &str) -> Result<bool, Failure> {
        self.inner.replace_if_pending(key, holder, run_id)
    }
    fn record(&self, key: &EntryKey, value: &Stored) -> Result<Option<Stored>, Failure> {
        self.inner.record(key, value)
    }
    fn release(&self, key: &EntryKey, run_id: &str) -> Result<(), Failure> {
        self.inner.release(key, run_id)
    }
    fn note_attempts(&self, key: &EntryKey, run_id: &str, attempts: u32) -> Result<(), Failure> {
        self.inner.note_attempts(key, run_id, attempts)
    }
    fn rows(&self, execution_id: &str) -> Result<Vec<Row>, Failure> {
        self.inner.rows(execution_id)
    }
    fn retire(&self, execution_id: &str) -> Result<(), Failure> {
        let owner = self.catalog.owner_at(&OwnerScope::host(SCOPE))?.map(|o| o.execution_id);
        self.seen.lock().unwrap().push(owner);
        if self.failures.load(Ordering::SeqCst) > 0 {
            self.failures.fetch_sub(1, Ordering::SeqCst);
            return Err(Failure::new(FailureTag::Storage, "the journal store went away mid-delete"));
        }
        self.inner.retire(execution_id)
    }
    fn executions(&self) -> Result<Vec<String>, Failure> {
        self.inner.executions()
    }
}

fn open(run_id: &str) -> OpenExecution {
    OpenExecution {
        scope: OwnerScope::host(SCOPE),
        pins: OwnerPins::from(PlanPins { plan_ref: "plan-a".into(), identities: Default::default() }),
        run_id: run_id.into(),
        site_id: "site-a".into(),
        pid: 4242,
        boot_id: "boot-a".into(),
        trace_id: None,
        connector: None,
        schedule: Schedule::single_attempt(),
    }
}

/// A journal store apart from the catalog deletes a retired owner's rows after the catalog commits the
/// retirement, and the next open under that scope repeats the delete.
// spec: run.journal.retire-order@0cf4769a
#[test]
fn a_delete_the_catalog_commit_outran_repeats_at_the_next_open() {
    let dir = tempfile::tempdir().unwrap();
    let catalog: Arc<dyn Catalog + Send + Sync> = Arc::new(LocalCatalog::open(dir.path(), Arc::new(SetClock::new(T0))));
    let rows = DeleteFails { inner: FileJournalStore::open(dir.path()), catalog: catalog.clone(), failures: Arc::new(AtomicUsize::new(1)), seen: Arc::default() };
    let engine = Engine {
        catalog: catalog.clone(),
        journal: Journal::over(rows.clone(), FileBlobStore::open(dir.path())),
        awakeables: None,
        keeper: Keeper::new(Cadence { poll: Duration::from_millis(20), renew: Duration::from_secs(10) }),
        emitter: None,
        worlds: Vec::new(),
    };

    let mut x = engine.open_execution(&open("job-1")).unwrap();
    let retired = x.execution_id().to_string();
    x.step("fetch", b"doc-1", &mut |_| Ok(b"fetched".to_vec())).unwrap();
    // The catalog commits the retirement; the journal delete that follows dies.
    let died = x.commit(None);
    assert!(matches!(&died, Err(EngineError::Failure(f)) if f.tag == FailureTag::Storage), "{died:?}");
    drop(x);
    assert_eq!(*rows.seen.lock().unwrap(), [None], "the delete ran only once the catalog had retired the owner");
    assert!(catalog.owner_at(&OwnerScope::host(SCOPE)).unwrap().is_none(), "the retirement committed");
    assert_eq!(catalog.retired_at(&OwnerScope::host(SCOPE)).unwrap().as_deref(), Some(retired.as_str()));
    assert_eq!(engine.journal.recorded(&retired).unwrap(), 1, "the late delete left the retired owner's row");

    // The next open under the scope deletes the rows no replay reaches before it claims.
    let y = engine.open_execution(&open("job-2")).unwrap();
    assert_ne!(y.execution_id(), retired, "the next open takes a fresh execution");
    assert_eq!(engine.journal.recorded(&retired).unwrap(), 0, "the open repeated the delete");
    assert!(!engine.journal.row_store().executions().unwrap().contains(&retired));
}
