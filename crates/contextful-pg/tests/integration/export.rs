//! A lost sink acknowledgement leaves the identical events in the remote outbox.

use contextful_core::export::{change_events, change_state, parse_exports, ExportPublication};
use contextful_pg::PgExportLedger;
use serde_json::json;

#[test]
fn replacement_reoffers_identical_event_ids_and_acknowledges_only_its_prefix() {
    let Some(server) = super::server::start() else {
        return;
    };
    let export = parse_exports("[[export]]\nname='sink'\ntable='items'\nendpoint='https://sink.example/events'\nformat='changes-v1'\nkey=['id']\nschedule='every 30s'\n").unwrap().remove(0);
    let state = change_state(&export, &[json!({"id":"a", "value":7})]).unwrap();
    let events = change_events(&export, "publication-1", 0, &Default::default(), &state).unwrap();
    let mut first = PgExportLedger::connect(&server.conninfo).unwrap();
    assert!(first
        .stage(
            "sink",
            ExportPublication {
                source: "source-1",
                id: "publication-1",
                identity: "identity-1",
                destination: "destination-1"
            },
            &events,
            &state
        )
        .unwrap());
    let sent = first.pending("sink", 500).unwrap();
    assert_eq!(sent, events);
    first.offer("sink", sent.last().unwrap().sequence).unwrap();
    // The sink commits this batch; its acknowledgement never reaches this process.
    drop(first);
    let mut replacement = PgExportLedger::connect(&server.conninfo).unwrap();
    assert!(replacement
        .acknowledge("sink", sent.last().unwrap().sequence)
        .is_err());
    let retried = replacement.pending("sink", 500).unwrap();
    assert_eq!(
        serde_json::to_vec(&retried).unwrap(),
        serde_json::to_vec(&sent).unwrap()
    );
    let through = retried.last().unwrap().sequence;
    replacement.offer("sink", through).unwrap();
    replacement.acknowledge("sink", through).unwrap();
    assert_eq!(replacement.state("sink").unwrap(), state);
    assert_eq!(
        replacement
            .position("sink")
            .unwrap()
            .source_publication
            .as_deref(),
        Some("source-1")
    );
    assert!(replacement.pending("sink", 500).unwrap().is_empty());
}

#[test]
fn a_second_writer_cannot_overwrite_a_pending_publication() {
    let Some(server) = super::server::start() else {
        return;
    };
    let export = parse_exports("[[export]]\nname='sink'\ntable='items'\nendpoint='https://sink.example/events'\nformat='changes-v1'\nkey=['id']\nschedule='every 30s'\n").unwrap().remove(0);
    let state = change_state(&export, &[json!({"id":"a"})]).unwrap();
    let events = change_events(&export, "p", 0, &Default::default(), &state).unwrap();
    let p = ExportPublication {
        source: "s",
        id: "p",
        identity: "i",
        destination: "d",
    };
    let mut first = PgExportLedger::connect(&server.conninfo).unwrap();
    let mut second = PgExportLedger::connect(&server.conninfo).unwrap();
    assert!(first.stage("sink", p, &events, &state).unwrap());
    assert!(!second
        .stage(
            "sink",
            ExportPublication { id: "other", ..p },
            &events,
            &state
        )
        .unwrap());
    assert_eq!(second.pending("sink", 500).unwrap(), events);
    first.pending("sink", 500).unwrap();
    second.acknowledge("sink", events[0].sequence).unwrap_err();
    first.offer("sink", events[0].sequence).unwrap();
    first.acknowledge("sink", events[0].sequence).unwrap();
    assert!(second
        .acknowledge("sink", events.last().unwrap().sequence)
        .is_err());
}

#[test]
fn a_paused_export_writer_is_fenced_before_dispatch_after_takeover() {
    let Some(server) = super::server::start() else {
        return;
    };
    let mut old = PgExportLedger::connect(&server.conninfo).unwrap();
    old.claim("sink", "old-container", 90).unwrap();
    let calls = std::sync::atomic::AtomicUsize::new(0);
    old.dispatch("sink", || {
        calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    })
    .unwrap();
    assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 1);
    let mut admin = postgres::Client::connect(&server.conninfo, postgres::NoTls).unwrap();
    admin
        .execute("UPDATE contextful_lease SET expires_us = 0", &[])
        .unwrap();
    let mut replacement = PgExportLedger::connect(&server.conninfo).unwrap();
    replacement.claim("sink", "replacement", 90).unwrap();
    let error = old
        .dispatch("sink", || {
            calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        })
        .unwrap_err();
    assert!(error.message.contains("ExecutionWriterFenced"), "{error}");
    assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 1);
    replacement
        .dispatch("sink", || {
            calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        })
        .unwrap();
    assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 2);
}

#[test]
fn log_checkpoints_survive_replacement_and_refuse_backwards_progress() {
    let Some(server) = super::server::start() else {
        return;
    };
    let cursor = contextful_core::export::ExportCursor {
        commit_seq: 4,
        row_seq: 8,
    };
    let mut ledger = PgExportLedger::connect(&server.conninfo).unwrap();
    ledger.advance_checkpoint("logs", cursor).unwrap();
    drop(ledger);
    let mut replacement = PgExportLedger::connect(&server.conninfo).unwrap();
    assert_eq!(replacement.checkpoint("logs").unwrap(), cursor);
    assert!(replacement
        .advance_checkpoint("logs", Default::default())
        .is_err());
    assert_eq!(replacement.checkpoint("logs").unwrap(), cursor);
}

// spec: run.export.writer-fenced@7ebbf9ff
#[test]
fn ownership_transfer_waits_for_an_admitted_outbound_effect() {
    let Some(server) = super::server::start() else {
        return;
    };
    let mut writer = PgExportLedger::connect(&server.conninfo).unwrap();
    writer.claim("sink", "writer", 90).unwrap();
    let (entered, inside) = std::sync::mpsc::channel();
    let (release, finish) = std::sync::mpsc::channel();
    let (updated, update_done) = std::sync::mpsc::channel();
    std::thread::scope(|scope| {
        let writer = &writer;
        let dispatch = scope.spawn(move || {
            writer
                .dispatch("sink", || {
                    entered.send(()).unwrap();
                    finish.recv().unwrap();
                })
                .unwrap()
        });
        inside
            .recv_timeout(std::time::Duration::from_secs(5))
            .unwrap();
        let update = scope.spawn(|| {
            let mut admin = postgres::Client::connect(&server.conninfo, postgres::NoTls).unwrap();
            let changed = admin
                .execute("UPDATE contextful_lease SET expires_us = 0", &[])
                .unwrap();
            updated.send(changed).unwrap();
        });
        assert!(matches!(
            update_done.recv_timeout(std::time::Duration::from_millis(50)),
            Err(std::sync::mpsc::RecvTimeoutError::Timeout)
        ));
        release.send(()).unwrap();
        dispatch.join().unwrap();
        update.join().unwrap();
        assert_eq!(update_done.recv().unwrap(), 1);
    });
    let mut replacement = PgExportLedger::connect(&server.conninfo).unwrap();
    replacement.claim("sink", "replacement", 90).unwrap();
    assert!(writer
        .dispatch("sink", || panic!("old effect entered after transfer"))
        .is_err());
}
