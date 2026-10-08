use contextful_context::encrypt::AesGcmFileCipher;
use contextful_core::coordinate::Catalog;
use contextful_core::export::{Change, ChangeEvent, ChangeState};
use contextful_core::run::journal::{EntryKey, Stored};
use contextful_core::run::ports::JournalStore;
use contextful_core::store::encrypt::FileCipher;
use contextful_sqlite::{ExportLedger, ExportPublication, MachineCatalog, SqliteRunStores};
use serde_json::json;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

fn cipher() -> Arc<dyn FileCipher> { Arc::new(AesGcmFileCipher::new([7; 32], 1)) }

fn publication() -> ExportPublication<'static> {
    ExportPublication { source: "frontier-1", id: "publication-1", identity: "reader-1", destination: "target-1" }
}

fn fixture(name: &str) -> (Vec<ChangeEvent>, ChangeState) {
    let row = json!({"id": "one", "body": "sealed-export-canary-74"});
    let events = vec![
        ChangeEvent { version: 1, id: format!("{name}:publication-1:0"), publication: "publication-1".into(), sequence: 0, table: "documents".into(), change: Change::Upsert { key: json!({"id": "one"}), row: row.clone() } },
        ChangeEvent { version: 1, id: format!("{name}:publication-1:1"), publication: "publication-1".into(), sequence: 1, table: "documents".into(), change: Change::PublicationComplete { changes: 1 } },
    ];
    (events, [("one".into(), row)].into())
}

#[test]
fn sealed_export_reloads_independent_writers_and_preserves_catalog_and_journal() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("machine.sqlite");
    let catalog = MachineCatalog::open_sealed(&path, Arc::new(crate::SetClock::new()), cipher()).unwrap();
    catalog.put_run(&crate::run_row("run-1", "feed", contextful_core::run::record::RunStatus::Running)).unwrap();
    let mut first = ExportLedger::open_sealed(&path, cipher()).unwrap();
    let mut second = ExportLedger::open_sealed(&path, cipher()).unwrap();
    let (a, state) = fixture("a");
    let (b, _) = fixture("b");
    assert!(first.stage("a", publication(), &a, &state).unwrap());
    let stores = SqliteRunStores::open_sealed(&path, cipher()).unwrap();
    let key = EntryKey { execution_id: "execution-1".into(), step_label: "step-1".into(), input_hash: "input-1".into() };
    stores.journal.record(&key, &Stored::place(b"sealed-export-canary-74")).unwrap();
    assert!(second.stage("b", publication(), &b, &state).unwrap());
    assert_eq!(first.pending("b", 2).unwrap(), b);
    assert_eq!(second.pending("a", 2).unwrap(), a);
    second.acknowledge("a", 1).unwrap();
    assert_eq!(first.state("a").unwrap(), state);
    assert_eq!(first.position("a").unwrap().source_publication.as_deref(), Some("frontier-1"));
    assert!(catalog.run("run-1").unwrap().is_some());
    assert!(stores.journal.read(&key).unwrap().is_some());
    drop((first, second));
    let mut reopened = ExportLedger::open_sealed(&path, cipher()).unwrap();
    assert_eq!(reopened.pending("b", 2).unwrap(), b);
    assert_eq!(reopened.state("a").unwrap(), state);
    let before = std::fs::read(&path).unwrap();
    assert!(ExportLedger::open_sealed(&path, Arc::new(AesGcmFileCipher::new([8; 32], 1))).is_err());
    assert!(ExportLedger::open(&path).is_err());
    assert_eq!(std::fs::read(&path).unwrap(), before);
    for entry in std::fs::read_dir(dir.path()).unwrap() {
        let bytes = std::fs::read(entry.unwrap().path()).unwrap();
        assert!(!bytes.starts_with(b"SQLite format 3"));
        assert!(!bytes.windows(b"sealed-export-canary-74".len()).any(|b| b == b"sealed-export-canary-74"));
    }
}

fn exact_acknowledgement(mut ledger: ExportLedger, reopen: impl FnOnce() -> ExportLedger) {
    let (events, state) = fixture("a");
    let mut invalid = events.clone();
    invalid[1].sequence = 3;
    assert!(ledger.stage("a", publication(), &invalid, &state).is_err());
    assert_eq!(ledger.position("a").unwrap().next_sequence, 0);
    assert!(ledger.stage("a", publication(), &events, &state).unwrap());
    assert!(!ledger.stage("a", publication(), &events, &state).unwrap());
    assert_eq!(ledger.pending("a", 2).unwrap(), events);
    assert!(ledger.acknowledge("a", 0).is_err());
    ledger.offer("a", 0).unwrap();
    ledger.acknowledge("a", 0).unwrap();
    assert!(ledger.state("a").unwrap().is_empty());
    drop(ledger);
    let mut reopened = reopen();
    assert!(reopened.acknowledge("a", 1).is_err());
    assert_eq!(reopened.pending("a", 2).unwrap(), events[1..]);
    reopened.acknowledge("a", 1).unwrap();
    assert_eq!(reopened.state("a").unwrap(), state);
    assert!(reopened.position("a").unwrap().pending_publication.is_none());
    assert!(!reopened.stage("a", publication(), &events, &state).unwrap());
}

#[test]
fn sealed_export_rolls_back_invalid_publication_and_retains_exact_acknowledgements() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("machine.sqlite");
    exact_acknowledgement(ExportLedger::open_sealed(&path, cipher()).unwrap(), || ExportLedger::open_sealed(&path, cipher()).unwrap());
}

#[test]
fn plain_export_retains_exact_acknowledgements() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("machine.sqlite");
    exact_acknowledgement(ExportLedger::open(&path).unwrap(), || ExportLedger::open(&path).unwrap());
}

struct FailingSeal {
    fail: Arc<AtomicBool>,
    inner: AesGcmFileCipher,
}

impl FileCipher for FailingSeal {
    fn key_version(&self) -> u32 { self.inner.key_version() }
    fn seal(&self, bytes: &[u8]) -> Result<Vec<u8>, contextful_core::store::encrypt::SealError> {
        if self.fail.load(Ordering::SeqCst) {
            Err(contextful_core::store::encrypt::SealError("sealing is unavailable".into()))
        } else {
            self.inner.seal(bytes)
        }
    }
    fn open(&self, bytes: &[u8]) -> Result<Vec<u8>, contextful_core::store::encrypt::SealError> { self.inner.open(bytes) }
}

#[test]
fn a_failed_export_seal_preserves_durable_state_and_reloads_before_retry() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("machine.sqlite");
    let fail = Arc::new(AtomicBool::new(false));
    let binding: Arc<dyn FileCipher> = Arc::new(FailingSeal { fail: fail.clone(), inner: AesGcmFileCipher::new([7; 32], 1) });
    let mut ledger = ExportLedger::open_sealed(&path, binding).unwrap();
    let (events, state) = fixture("a");
    let before = std::fs::read(&path).unwrap();
    fail.store(true, Ordering::SeqCst);
    assert!(ledger.stage("a", publication(), &events, &state).is_err());
    assert_eq!(std::fs::read(&path).unwrap(), before);
    assert_eq!(ledger.position("a").unwrap().next_sequence, 0);
    fail.store(false, Ordering::SeqCst);
    assert!(ledger.stage("a", publication(), &events, &state).unwrap());
    assert_eq!(ledger.pending("a", 2).unwrap(), events);
    let staged = std::fs::read(&path).unwrap();
    fail.store(true, Ordering::SeqCst);
    assert!(ledger.acknowledge("a", 1).is_err());
    assert_eq!(std::fs::read(&path).unwrap(), staged);
    assert!(ledger.state("a").unwrap().is_empty());
    fail.store(false, Ordering::SeqCst);
    ledger.acknowledge("a", 1).unwrap();
    assert_eq!(ledger.state("a").unwrap(), state);
}

#[test]
fn sealed_export_refuses_plaintext_and_corrupt_envelopes_without_fallback() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("machine.sqlite");
    let (events, state) = fixture("a");
    let mut plain = ExportLedger::open(&path).unwrap();
    plain.stage("a", publication(), &events, &state).unwrap();
    drop(plain);
    let before = std::fs::read(&path).unwrap();
    assert!(ExportLedger::open_sealed(&path, cipher()).is_err());
    assert_eq!(std::fs::read(&path).unwrap(), before);
    let sealed_path = dir.path().join("sealed.sqlite");
    drop(ExportLedger::open_sealed(&sealed_path, cipher()).unwrap());
    let mut corrupted = std::fs::read(&sealed_path).unwrap();
    *corrupted.last_mut().unwrap() ^= 1;
    std::fs::write(&sealed_path, &corrupted).unwrap();
    assert!(ExportLedger::open_sealed(&sealed_path, cipher()).is_err());
    assert_eq!(std::fs::read(&sealed_path).unwrap(), corrupted);
}
