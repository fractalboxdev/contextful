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

#[test]
fn coordinated_receiver_activates_all_members_once_and_refuses_rollback() {
    use contextful_core::export::publication::{publication as group, GroupBatch, PublicationPlan};
    use contextful_sqlite::PublicationReceiver;
    use std::collections::BTreeMap;
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("receiver.sqlite");
    let mut receiver = PublicationReceiver::open(&path).unwrap();
    let plan = PublicationPlan { name:"copy".into(), incarnation:"store-1".into(), namespace:"initial".into(), base:None, order:1,
        destination:"receiver".into(),read_identity:"reader".into(),
        input:json!({"snapshot":"one"}), members:[("a".into(),"items".into()),("b".into(),"empty".into())].into() };
    let first_state = [("a".into(),[("{\"id\":1}".into(),json!({"id":1,"body":"one"}))].into()),("b".into(),BTreeMap::new())].into();
    let first = group(plan.clone(), &BTreeMap::new(), &first_state).unwrap();
    let batch = |events| GroupBatch { version:1, group:first.group.clone(), events };
    receiver.receive(&batch(first.events[..2].to_vec())).unwrap();
    assert!(receiver.active("copy").unwrap().is_none());
    assert!(receiver.state("copy").unwrap().is_empty());
    drop(receiver);
    let mut receiver = PublicationReceiver::open(&path).unwrap();
    receiver.receive(&batch(first.events.clone())).unwrap();
    assert_eq!(receiver.state("copy").unwrap(), first_state);
    let mut second_plan = plan.clone();
    second_plan.base = Some(first.group.id.clone());
    second_plan.order = 2;
    let second_state = [("a".into(),BTreeMap::new()),("b".into(),[("{\"id\":2}".into(),json!({"id":2}))].into())].into();
    let second = group(second_plan, &first_state, &second_state).unwrap();
    receiver.receive(&GroupBatch {version:1,group:second.group.clone(),events:second.events[..2].to_vec()}).unwrap();
    assert_eq!(receiver.state("copy").unwrap(), first_state);
    receiver.receive(&GroupBatch {version:1,group:second.group.clone(),events:second.events[2..].to_vec()}).unwrap();
    assert_eq!(receiver.state("copy").unwrap(), second_state);
    receiver.receive(&batch(first.events.clone())).unwrap();
    assert_eq!(receiver.state("copy").unwrap(), second_state);
    let mut stale_plan = plan.clone();
    stale_plan.input = json!({"snapshot":"stale-but-different"});
    let stale = group(stale_plan, &BTreeMap::new(), &first_state).unwrap();
    assert!(receiver.receive(&GroupBatch {version:1,group:stale.group,events:stale.events}).is_err());
    let mut rebuild_plan = plan;
    rebuild_plan.namespace = "rebuilt".into();
    let rebuilt = group(rebuild_plan.clone(), &BTreeMap::new(), &second_state).unwrap();
    receiver.receive(&GroupBatch {version:1,group:rebuilt.group.clone(),events:rebuilt.events[..2].to_vec()}).unwrap();
    assert_eq!(receiver.active("copy").unwrap().unwrap().plan.namespace, "initial");
    receiver.receive(&GroupBatch {version:1,group:rebuilt.group.clone(),events:rebuilt.events[2..].to_vec()}).unwrap();
    assert_eq!(receiver.active("copy").unwrap().unwrap().plan.namespace, "rebuilt");
    rebuild_plan.incarnation = "store-2".into();
    rebuild_plan.namespace = "foreign".into();
    let foreign = group(rebuild_plan, &BTreeMap::new(), &first_state).unwrap();
    assert!(receiver.receive(&GroupBatch {version:1,group:foreign.group,events:foreign.events}).is_err());
}

#[test]
fn coordinated_outbox_retries_identical_bytes_and_checkpoint_rejects_incompatibility() {
    use contextful_core::export::publication::PublicationPlan;
    use contextful_sqlite::PublicationLedger;
    use std::collections::BTreeMap;
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("sender.sqlite");
    let mut ledger = PublicationLedger::open(&path).unwrap();
    let plan = PublicationPlan {name:"copy".into(),incarnation:"store-1".into(),namespace:"one".into(),destination:"receiver".into(),read_identity:"read-identity".into(),base:None,order:1,input:json!("frontier"),members:[("a".into(),"items".into())].into()};
    let states = [("a".into(),[("{\"id\":1}".into(),json!({"id":1}))].into())].into();
    ledger.stage(&plan, &states, "read-identity").unwrap();
    let checkpoint = ledger.checkpoint("copy", "one").unwrap();
    let first = ledger.batch("copy", "one").unwrap().unwrap();
    assert_eq!(serde_json::to_vec(&first).unwrap(), serde_json::to_vec(&ledger.batch("copy", "one").unwrap().unwrap()).unwrap());
    assert!(ledger.acknowledge("copy", "one", &first.group.id, 0).is_err());
    assert!(ledger.stage(&plan, &states, "different-reader").is_err());
    ledger.acknowledge("copy", "one", &first.group.id, first.events.last().unwrap().sequence).unwrap();
    assert!(ledger.batch("copy", "one").unwrap().is_none());
    ledger.restore("copy", "one", &checkpoint, "store-1", "read-identity").unwrap();
    let replay = ledger.batch("copy", "one").unwrap().unwrap();
    assert_eq!(first, replay);
    assert!(ledger.restore("copy", "one", &checkpoint, "store-2", "read-identity").is_err());
    assert!(ledger.restore("copy", "one", &checkpoint, "store-1", "other-reader").is_err());
    assert!(ledger.stage(&PublicationPlan {members:BTreeMap::new(),..plan}, &BTreeMap::new(), "read-identity").is_err());
}

#[test]
fn restored_publication_checkpoint_cannot_reverse_a_durable_receiver_tombstone() {
    use contextful_core::export::publication::PublicationPlan;
    use contextful_sqlite::{PublicationLedger,PublicationReceiver};
    let dir = tempfile::tempdir().unwrap();
    let sender_path = dir.path().join("sender.sqlite");
    let receiver_path = dir.path().join("receiver.sqlite");
    let mut sender = PublicationLedger::open(&sender_path).unwrap();
    let mut receiver = PublicationReceiver::open(&receiver_path).unwrap();
    let mut plan = PublicationPlan {name:"copy".into(),incarnation:"store-1".into(),namespace:"one".into(),destination:"receiver".into(),read_identity:"reader".into(),base:None,order:1,input:json!("frontier-one"),members:[("a".into(),"items".into())].into()};
    let first_state = [("a".into(),[("{\"id\":1}".into(),json!({"id":1}))].into())].into();
    sender.stage(&plan,&first_state,"reader").unwrap();
    let checkpoint = sender.checkpoint("copy","one").unwrap();
    let first = sender.batch("copy","one").unwrap().unwrap();
    receiver.receive(&first).unwrap();
    sender.acknowledge("copy","one",&first.group.id,first.events.last().unwrap().sequence).unwrap();
    plan.base = Some(first.group.id.clone());
    plan.order = 2;
    plan.input = json!("frontier-two");
    let second_state = [("a".into(),[("{\"id\":2}".into(),json!({"id":2}))].into())].into();
    sender.stage(&plan,&second_state,"reader").unwrap();
    let second = sender.batch("copy","one").unwrap().unwrap();
    receiver.receive(&second).unwrap();
    sender.acknowledge("copy","one",&second.group.id,second.events.last().unwrap().sequence).unwrap();
    assert_eq!(receiver.state("copy").unwrap(),second_state);
    drop(receiver);
    sender.restore("copy","one",&checkpoint,"store-1","reader").unwrap();
    let restored = sender.batch("copy","one").unwrap().unwrap();
    assert_eq!(serde_json::to_vec(&restored).unwrap(),serde_json::to_vec(&first).unwrap());
    let mut receiver = PublicationReceiver::open(&receiver_path).unwrap();
    receiver.receive(&restored).unwrap();
    assert_eq!(receiver.state("copy").unwrap(),second_state);
    assert_eq!(receiver.active("copy").unwrap().unwrap(),second.group);
    let mut altered = first;
    altered.events[0].change = contextful_core::export::publication::GroupChange::Delete {key:json!({"id":1})};
    assert!(receiver.receive(&altered).is_err());
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
        if bytes.is_empty() {
            continue;
        }
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
