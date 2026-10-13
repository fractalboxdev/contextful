//! Disposable processes reopen one database's execution stores.

use contextful_core::run::journal::{EntryKey, Row, Stored};
use contextful_core::run::ports::{AwakeableStore, BlobStore, JournalStore};
use contextful_core::run::suspend::Awakeable;
use contextful_engine::{
    awake::{Awaited, Registry},
    conformance, Journal,
};
use contextful_pg::PgRunStores;
use std::sync::atomic::{AtomicUsize, Ordering};

struct Never;
impl contextful_core::run::ports::Cancellation for Never {
    fn requested(&self) -> bool {
        false
    }
}

#[test]
fn recorded_paid_calls_blobs_and_pending_callbacks_survive_an_empty_local_disk() {
    let Some(server) = super::server::start() else {
        return;
    };
    let key = EntryKey {
        execution_id: "paid-execution".into(),
        step_label: "provider".into(),
        input_hash: "same-request".into(),
    };
    let bytes = vec![0xa5; contextful_core::run::journal::INLINE_CUTOFF_BYTES + 1];
    let stored = Stored::place(&bytes);
    let sha = stored.blob().expect("above inline cutoff");
    let callback = Awakeable::mint(
        "0123456789abcdef0123456789abcdef",
        "paid-execution",
        "callback",
        super::T0,
        3600,
    )
    .unwrap();
    let paid_calls = AtomicUsize::new(0);
    {
        let stores = PgRunStores::connect(&server.conninfo).unwrap();
        let journal = Journal::over(stores.journal.clone(), stores.blobs.clone());
        let recorded = journal
            .step(
                &key,
                "old-container",
                &|_| Ok(true),
                &Never,
                &|_| true,
                &mut || {
                    paid_calls.fetch_add(1, Ordering::SeqCst);
                    Ok(bytes.clone())
                },
            )
            .unwrap();
        assert_eq!(recorded.bytes(), bytes);
        stores.awakeables.insert(&callback).unwrap();
    }
    let empty_disk = tempfile::tempdir().unwrap();
    assert_eq!(std::fs::read_dir(empty_disk.path()).unwrap().count(), 0);
    let replacement = PgRunStores::connect(&server.conninfo).unwrap();
    assert_eq!(
        replacement.journal.read(&key).unwrap(),
        Some(Row::Recorded {
            key: key.clone(),
            value: stored.clone()
        })
    );
    assert_eq!(replacement.blobs.get(sha).unwrap(), Some(bytes.clone()));
    assert_eq!(
        replacement.awakeables.get(&callback.token).unwrap(),
        Some(callback.clone())
    );
    assert!(!replacement
        .journal
        .create_pending(&key, "replacement")
        .unwrap());
    let journal = Journal::over(replacement.journal.clone(), replacement.blobs.clone());
    let replayed = journal
        .step(
            &key,
            "replacement",
            &|_| Ok(false),
            &Never,
            &|_| true,
            &mut || {
                paid_calls.fetch_add(1, Ordering::SeqCst);
                Ok(b"another paid response".to_vec())
            },
        )
        .unwrap();
    assert_eq!(replayed.bytes(), bytes);
    assert_eq!(paid_calls.load(Ordering::SeqCst), 1);
    let registry = Registry::over(replacement.awakeables.clone(), journal);
    let payload = vec![b'p'; contextful_core::run::journal::INLINE_CUTOFF_BYTES + 1];
    assert_eq!(
        registry
            .resolve(&callback.token, &payload, super::at(super::T0))
            .unwrap(),
        payload
    );
    drop(registry);
    drop(replacement);
    let replacement = PgRunStores::connect(&server.conninfo).unwrap();
    let registry = Registry::over(
        replacement.awakeables.clone(),
        Journal::over(replacement.journal.clone(), replacement.blobs.clone()),
    );
    assert_eq!(
        registry
            .awaited("paid-execution", &callback.token, super::at(super::T0))
            .unwrap(),
        Awaited::Resumed(payload)
    );
    assert_eq!(
        replacement
            .journal
            .record(&key, &Stored::place(b"another paid response"))
            .unwrap(),
        Some(stored)
    );
}

#[test]
fn postgres_execution_stores_conform_to_the_existing_ports() {
    let Some(server) = super::server::start() else {
        return;
    };
    let fresh = || {
        let mut admin = postgres::Client::connect(&server.conninfo, postgres::NoTls).unwrap();
        admin.batch_execute("TRUNCATE contextful_journal, contextful_blob, contextful_awakeable, contextful_sweep").unwrap();
        PgRunStores::connect(&server.conninfo).unwrap()
    };
    drop(PgRunStores::connect(&server.conninfo).unwrap());
    conformance::blob_store("Postgres", &mut || fresh().blobs);
    conformance::journal_store("Postgres", &mut || {
        let s = fresh();
        (s.journal, s.blobs)
    });
    conformance::awakeable_store("Postgres", &mut || {
        let s = fresh();
        (s.awakeables, s.journal, s.blobs)
    });
    fresh();
    conformance::awakeable_restart("Postgres", &mut || {
        let s = PgRunStores::connect(&server.conninfo).unwrap();
        (s.awakeables, s.journal, s.blobs)
    });
}

#[test]
fn conditional_claims_and_attempt_counts_are_shared_across_connections() {
    let Some(server) = super::server::start() else {
        return;
    };
    let first = PgRunStores::connect(&server.conninfo).unwrap();
    let second = PgRunStores::connect(&server.conninfo).unwrap();
    let key = EntryKey {
        execution_id: "retry".into(),
        step_label: "call".into(),
        input_hash: "input".into(),
    };
    assert!(first.journal.create_pending(&key, "old").unwrap());
    first.journal.note_attempts(&key, "old", 2).unwrap();
    assert!(!second
        .journal
        .replace_if_pending(&key, "unrelated", "replacement")
        .unwrap());
    assert!(second
        .journal
        .replace_if_pending(&key, "old", "replacement")
        .unwrap());
    first.journal.release(&key, "old").unwrap();
    assert_eq!(
        first.journal.read(&key).unwrap(),
        Some(Row::Pending {
            key: key.clone(),
            run_id: "replacement".into(),
            attempts: 2
        })
    );
    second
        .journal
        .record(&key, &Stored::place(b"recorded"))
        .unwrap();
    assert!(!first
        .journal
        .replace_if_pending(&key, "replacement", "old")
        .unwrap());
    assert_eq!(first.journal.executions().unwrap(), vec!["retry"]);
    first.journal.retire("retry").unwrap();
    assert_eq!(second.journal.read(&key).unwrap(), None);
}

#[test]
fn incompatible_execution_schema_refuses_before_any_work() {
    let Some(server) = super::server::start() else {
        return;
    };
    drop(PgRunStores::connect(&server.conninfo).unwrap());
    let mut admin = postgres::Client::connect(&server.conninfo, postgres::NoTls).unwrap();
    admin
        .execute("UPDATE contextful_execution_schema SET version = 999", &[])
        .unwrap();
    let error = match PgRunStores::connect(&server.conninfo) {
        Ok(_) => panic!("future schema admitted"),
        Err(e) => e,
    };
    assert!(
        error.message.contains("ExecutionSchemaIncompatible"),
        "{error}"
    );
}

#[test]
fn a_corpus_only_restore_cannot_initialize_execution_state_at_startup() {
    let Some(server) = super::server::start() else {
        return;
    };
    let missing =
        PgRunStores::verify_recovery(&server.conninfo, "research", "recovery-1").unwrap_err();
    assert!(
        missing.message.contains("ExecutionRecoveryMismatch"),
        "{missing}"
    );
    PgRunStores::initialize_recovery(&server.conninfo, "research", "recovery-1").unwrap();
    PgRunStores::verify_recovery(&server.conninfo, "research", "recovery-1").unwrap();
    assert!(
        PgRunStores::verify_recovery(&server.conninfo, "research", "other-generation").is_err()
    );
    assert!(
        PgRunStores::verify_recovery(&server.conninfo, "different-project", "recovery-1").is_err()
    );
    assert!(
        PgRunStores::initialize_recovery(&server.conninfo, "research", "other-generation").is_err()
    );
    PgRunStores::verify_recovery(&server.conninfo, "research", "recovery-1").unwrap();
    let mut admin = postgres::Client::connect(&server.conninfo, postgres::NoTls).unwrap();
    admin
        .batch_execute("DROP TABLE contextful_journal")
        .unwrap();
    let partial =
        PgRunStores::verify_recovery(&server.conninfo, "research", "recovery-1").unwrap_err();
    assert!(partial.message.contains("contextful_journal"), "{partial}");
    let missing: Option<String> = admin
        .query_one("SELECT to_regclass('contextful_journal')::TEXT", &[])
        .unwrap()
        .get(0);
    assert_eq!(
        missing, None,
        "a preflight never repairs a partial restore into empty state"
    );
    assert!(PgRunStores::initialize_recovery(&server.conninfo, "research", "recovery-1").is_err());
}

#[test]
fn a_resolution_interrupted_after_journaling_preserves_the_original_callback_bytes() {
    let Some(server) = super::server::start() else {
        return;
    };
    let stores = PgRunStores::connect(&server.conninfo).unwrap();
    let journal = Journal::over(stores.journal.clone(), stores.blobs.clone());
    let registry = Registry::over(stores.awakeables.clone(), journal.clone());
    let pending = registry.suspend("x", "callback", super::T0, 60).unwrap();
    let payload = vec![b'a'; contextful_core::run::journal::INLINE_CUTOFF_BYTES + 1];
    journal
        .record(
            &contextful_engine::awake::resume_key("x", &pending.token),
            &payload,
        )
        .unwrap();
    drop((registry, journal, stores));
    let stores = PgRunStores::connect(&server.conninfo).unwrap();
    let registry = Registry::over(
        stores.awakeables.clone(),
        Journal::over(stores.journal, stores.blobs),
    );
    assert!(matches!(
        registry.resolve(&pending.token, b"different", super::at(super::T0)),
        Err(contextful_engine::awake::AwakeError::Refused(
            contextful_core::run::RunError::AwakeableAlreadyResolved(_)
        ))
    ));
    assert_eq!(
        registry
            .resolve(&pending.token, &payload, super::at(super::T0))
            .unwrap(),
        payload
    );
}
