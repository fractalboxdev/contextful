//! `machine.sqlite` behind the `Catalog` port: lease rows and fences, the cursor
//! compare-and-swap, the pending owner and run rows.

use crate::{at, owner, run_row, SetClock, T0};
use contextful_core::coordinate::{Cas, Catalog, CursorRow, LeaseKey};
use contextful_core::run::own::OwnerScope;
use contextful_core::run::record::RunStatus;
use contextful_core::run::RunError;
use contextful_core::store::catalog::MACHINE_CATALOG_FILE;
use contextful_core::store::StoreError;
use contextful_context::encrypt::AesGcmFileCipher;
use contextful_sqlite::{ExportLedger, ExportPublication, MachineCatalog};
use contextful_core::export::{change_events, change_state, parse_exports, Change};
use serde_json::json;
use std::sync::Arc;

fn catalog(dir: &tempfile::TempDir, clock: &SetClock) -> MachineCatalog {
    MachineCatalog::open(&dir.path().join(MACHINE_CATALOG_FILE), Arc::new(clock.clone())).unwrap()
}

fn sealed_catalog(dir: &tempfile::TempDir, clock: &SetClock, key: u8) -> MachineCatalog {
    MachineCatalog::open_sealed(
        &dir.path().join(MACHINE_CATALOG_FILE),
        Arc::new(clock.clone()),
        Arc::new(AesGcmFileCipher::new([key; 32], 1)),
    )
    .unwrap()
}

// spec: store.encrypt.machine-catalog-sealing@9040e746
#[test]
fn sealed_machine_catalog_persists_lease_cursor_and_run_without_plaintext() {
    let dir = tempfile::tempdir().unwrap();
    let clock = SetClock::new();
    let canary = "machine-catalog-secret-canary-4017";
    {
        let c = sealed_catalog(&dir, &clock, 7);
        c.acquire(&LeaseKey::Pipeline(canary.into()), canary, 30).unwrap().unwrap();
        c.cursor_cas("feed", "filings", 0, cursor(canary), None).unwrap();
        c.put_run(&run_row(canary, canary, RunStatus::Success)).unwrap();
    }
    let c = sealed_catalog(&dir, &clock, 7);
    assert_eq!(c.lease_row(&LeaseKey::Pipeline(canary.into())).unwrap().fence, 1);
    assert_eq!(c.cursor("feed", "filings").unwrap().position, Some(json!(canary)));
    assert_eq!(c.run(canary).unwrap().unwrap().pipeline_id, canary);
    for entry in std::fs::read_dir(dir.path()).unwrap() {
        let path = entry.unwrap().path();
        let bytes = std::fs::read(&path).unwrap();
        if bytes.is_empty() {
            continue;
        }
        assert!(!bytes.windows(canary.len()).any(|window| window == canary.as_bytes()), "{} exposes the canary", path.display());
        assert!(!bytes.starts_with(b"SQLite format 3"), "{} exposes a SQLite page", path.display());
    }
}

#[test]
fn sealed_machine_catalog_refuses_plaintext_and_an_unbound_key() {
    let dir = tempfile::tempdir().unwrap();
    let clock = SetClock::new();
    let path = dir.path().join(MACHINE_CATALOG_FILE);
    catalog(&dir, &clock).put_run(&run_row("plain", "feed", RunStatus::Success)).unwrap();
    let before = std::fs::read(&path).unwrap();
    assert!(MachineCatalog::open_sealed(&path, Arc::new(clock.clone()), Arc::new(AesGcmFileCipher::new([7; 32], 1))).is_err());
    assert_eq!(std::fs::read(&path).unwrap(), before);

    std::fs::remove_file(&path).unwrap();
    sealed_catalog(&dir, &clock, 7).put_run(&run_row("sealed", "feed", RunStatus::Success)).unwrap();
    assert!(MachineCatalog::open_sealed(&path, Arc::new(clock), Arc::new(AesGcmFileCipher::new([8; 32], 1))).is_err());
}

#[test]
fn sealed_machine_catalog_serializes_cross_connection_cursor_cas() {
    let dir = tempfile::tempdir().unwrap();
    let clock = SetClock::new();
    sealed_catalog(&dir, &clock, 7);
    const WRITERS: u64 = 4;
    const EACH: u64 = 10;
    std::thread::scope(|scope| {
        for writer in 0..WRITERS {
            let (dir, clock) = (&dir, &clock);
            scope.spawn(move || {
                let c = sealed_catalog(dir, clock, 7);
                let mut applied = 0;
                while applied < EACH {
                    let seen = c.cursor("feed", "filings").unwrap().version;
                    if c.cursor_cas("feed", "filings", seen, cursor(&format!("writer-{writer}")), None).unwrap() == Cas::Applied {
                        applied += 1;
                    }
                }
            });
        }
    });
    assert_eq!(sealed_catalog(&dir, &clock, 7).cursor("feed", "filings").unwrap().version, WRITERS * EACH);
}

fn pipeline() -> LeaseKey {
    LeaseKey::Pipeline("feed/filings".into())
}

fn cursor(p: &str) -> CursorRow {
    CursorRow { position: Some(json!(p)), marker_run_id: Some("run-1".into()), marker_committed_at: Some(at(T0)), ..CursorRow::default() }
}

fn publication<'a>(source: &'a str, id: &'a str) -> ExportPublication<'a> {
    ExportPublication { source, id, identity: "view-1", destination: "target-1" }
}

#[test]
fn typed_export_replays_unacknowledged_events_and_commits_state_with_its_marker() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join(MACHINE_CATALOG_FILE);
    let export = parse_exports("[[export]]\nname = \"copy\"\ntable = \"items\"\nendpoint = \"https://receiver.example.com/events\"\nformat = \"changes-v1\"\nkey = [\"id\"]\nschedule = \"every 30s\"\n").unwrap().remove(0);
    let state = change_state(&export, &[json!({"id":"a","value":1}), json!({"id":"b","value":2})]).unwrap();
    let events = change_events(&export, "publication-1", 0, &Default::default(), &state).unwrap();
    let mut ledger = ExportLedger::open(&path).unwrap();
    assert!(ledger.stage("copy", publication("source-1", "publication-1"), &events, &state).unwrap());
    assert!(!ledger.stage("copy", publication("source-2", "publication-2"), &events, &state).unwrap());
    assert_eq!(ledger.pending("copy", 2).unwrap(), events[..2]);
    assert!(ledger.state("copy").unwrap().is_empty());
    drop(ledger);

    let mut reopened = ExportLedger::open(&path).unwrap();
    assert_eq!(reopened.pending("copy", 2).unwrap(), events[..2]);
    assert!(reopened.acknowledge("copy", 9).is_err());
    // A completion marker cannot pass changes outside the offered prefix.
    assert!(reopened.acknowledge("copy", 2).is_err(), "the completion marker cannot skip unacknowledged changes");
    assert_eq!(reopened.position("copy").unwrap().ack_sequence, None);
    reopened.acknowledge("copy", 1).unwrap();
    assert_eq!(reopened.position("copy").unwrap().ack_sequence, Some(1));
    assert!(reopened.state("copy").unwrap().is_empty());
    assert_eq!(reopened.pending("copy", 2).unwrap(), events[2..]);
    assert_eq!(events[2].change, Change::PublicationComplete { changes: 2 });
    reopened.acknowledge("copy", 2).unwrap();
    assert_eq!(reopened.state("copy").unwrap(), state);
    assert_eq!(reopened.position("copy").unwrap().source_publication.as_deref(), Some("source-1"));
    assert_eq!(reopened.position("copy").unwrap().identity.as_deref(), Some("view-1"));
    assert!(reopened.pending("copy", 2).unwrap().is_empty());
    let repeated = change_events(&export, "publication-1", 3, &state, &state).unwrap();
    assert!(!reopened.stage("copy", publication("source-1", "publication-1"), &repeated, &state).unwrap());
}

#[test]
fn typed_export_acknowledges_only_the_offered_name_after_restart() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join(MACHINE_CATALOG_FILE);
    let export = parse_exports("[[export]]\nname = \"copy\"\ntable = \"items\"\nendpoint = \"https://receiver.example.com/events\"\nformat = \"changes-v1\"\nkey = [\"id\"]\nschedule = \"every 30s\"\n").unwrap().remove(0);
    let state = change_state(&export, &[json!({"id":"a","value":1})]).unwrap();
    let events = change_events(&export, "publication-1", 0, &Default::default(), &state).unwrap();
    let mut ledger = ExportLedger::open(&path).unwrap();
    ledger.stage("copy", publication("source-1", "publication-1"), &events, &state).unwrap();
    ledger.stage("other", publication("source-1", "publication-1"), &events, &state).unwrap();
    assert_eq!(ledger.pending("copy", 2).unwrap(), events);
    // Offers are scoped to one export name.
    assert!(ledger.acknowledge("other", 1).is_err());
    drop(ledger);

    let mut reopened = ExportLedger::open(&path).unwrap();
    // The durable outbox must be offered again after restart.
    assert!(reopened.acknowledge("copy", 1).is_err());
    assert_eq!(reopened.position("copy").unwrap().ack_sequence, None);
    assert_eq!(reopened.pending("copy", 2).unwrap(), events);
    reopened.acknowledge("copy", 1).unwrap();
    assert_eq!(reopened.position("copy").unwrap().ack_sequence, Some(1));
    assert!(reopened.acknowledge("copy", 1).is_err());
    assert_eq!(reopened.position("other").unwrap().ack_sequence, None);
}

#[test]
fn typed_export_acknowledgement_stops_at_the_narrowed_offer() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join(MACHINE_CATALOG_FILE);
    let export = parse_exports("[[export]]\nname = \"copy\"\ntable = \"items\"\nendpoint = \"https://receiver.example.com/events\"\nformat = \"changes-v1\"\nkey = [\"id\"]\nschedule = \"every 30s\"\n").unwrap().remove(0);
    let state = change_state(&export, &[json!({"id":"a","value":"x".repeat(40000)}), json!({"id":"b","value":"y".repeat(40000)})]).unwrap();
    let events = change_events(&export, "publication-1", 0, &Default::default(), &state).unwrap();
    let mut ledger = ExportLedger::open(&path).unwrap();
    ledger.stage("copy", publication("source-1", "publication-1"), &events, &state).unwrap();
    // The outbound byte bound narrows the offered row-count batch.
    assert_eq!(ledger.pending("copy", 3).unwrap().len(), 3);
    ledger.offer("copy", 0).unwrap();
    assert!(ledger.acknowledge("copy", 1).is_err());
    ledger.acknowledge("copy", 0).unwrap();
    assert_eq!(ledger.pending("copy", 3).unwrap().len(), 2);
}

#[test]
fn typed_export_refuses_corrupt_counters_and_sequence_exhaustion() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join(MACHINE_CATALOG_FILE);
    let mut ledger = ExportLedger::open(&path).unwrap();
    let db = rusqlite::Connection::open(&path).unwrap();
    db.execute("INSERT INTO typed_export (name, ack_sequence, next_sequence) VALUES ('copy', -2, 0)", []).unwrap();
    assert!(ledger.position("copy").unwrap_err().to_string().contains("corrupt sequence counters"));
    db.execute("UPDATE typed_export SET ack_sequence = -1, next_sequence = -1 WHERE name = 'copy'", []).unwrap();
    assert!(ledger.position("copy").is_err());
    db.execute("UPDATE typed_export SET ack_sequence = -1, next_sequence = 9223372036854775807 WHERE name = 'copy'", []).unwrap();
    assert_eq!(ledger.position("copy").unwrap().next_sequence, i64::MAX as u64);
    let marker = contextful_core::export::ChangeEvent {
        version: 1, id: "copy:final".into(), publication: "final".into(), sequence: i64::MAX as u64,
        table: "items".into(), change: Change::PublicationComplete { changes: 0 },
    };
    assert!(ledger.stage("copy", publication("source-1", "final"), &[marker], &Default::default()).unwrap_err().to_string().contains("cursor range"));
    assert!(ledger.position("copy").unwrap().pending_publication.is_none());
}

#[test]
fn a_lease_is_taken_once_per_holder_and_expires_on_the_catalogs_clock() {
    let dir = tempfile::tempdir().unwrap();
    let clock = SetClock::new();
    let c = catalog(&dir, &clock);
    let a = c.acquire(&pipeline(), "holder-a", 30).unwrap().unwrap();
    assert_eq!((a.holder.as_str(), a.fence, a.expires_at), ("holder-a", 1, at(T0).plus_secs(30)));
    assert!(c.acquire(&pipeline(), "holder-b", 30).unwrap().is_none(), "a live holder keeps the row");
    assert!(c.lease_holds(&a).unwrap());

    clock.advance(30);
    assert!(!c.lease_holds(&a).unwrap(), "expiry is read on the catalog's clock");
    let b = c.acquire(&pipeline(), "holder-b", 30).unwrap().unwrap();
    assert_eq!(b.fence, 2);
    assert!(c.renew(&a, 30).unwrap().is_none(), "a superseded lease renews nothing");
    let renewed = c.renew(&b, 60).unwrap().unwrap();
    assert_eq!((renewed.fence, renewed.expires_at), (2, at(T0).plus_secs(90)));

    c.release(&b).unwrap();
    let row = c.lease_row(&pipeline()).unwrap();
    assert_eq!((row.holder, row.expires_at, row.fence), (None, None, 2), "release clears the holder and keeps the fence");
    assert_eq!(c.acquire(&pipeline(), "holder-c", 30).unwrap().unwrap().fence, 3);
}

#[test]
fn a_cursor_update_matches_only_the_stored_version_and_the_current_fence() {
    let dir = tempfile::tempdir().unwrap();
    let clock = SetClock::new();
    let c = catalog(&dir, &clock);
    assert_eq!(c.cursor("feed", "filings").unwrap(), CursorRow::default());
    assert_eq!(c.cursor_cas("feed", "filings", 0, cursor("p1"), None).unwrap(), Cas::Applied);
    assert_eq!(c.cursor_cas("feed", "filings", 0, cursor("p2"), None).unwrap(), Cas::VersionMoved);
    let stored = c.cursor("feed", "filings").unwrap();
    assert_eq!((stored.version, stored.position), (1, Some(json!("p1"))));
    assert_eq!(stored.marker_run_id.as_deref(), Some("run-1"));

    let stale = c.acquire(&pipeline(), "a", 30).unwrap().unwrap();
    clock.advance(31);
    let current = c.acquire(&pipeline(), "b", 30).unwrap().unwrap();
    match c.cursor_cas("feed", "filings", 1, cursor("p2"), Some(&stale)).unwrap() {
        Cas::Fenced(StoreError::LeaseFenced(m)) => assert!(m.contains("fence 1") && m.contains("at fence 2"), "{m}"),
        other => panic!("{other:?}"),
    }
    assert_eq!(c.cursor("feed", "filings").unwrap().version, 1, "the fenced commit applied nothing");
    assert_eq!(c.cursor_cas("feed", "filings", 1, cursor("p2"), Some(&current)).unwrap(), Cas::Applied);
    assert_eq!(c.cursor("feed", "filings").unwrap().position, Some(json!("p2")));
}

#[test]
fn retiring_an_owner_caches_its_position_in_the_same_transaction() {
    let dir = tempfile::tempdir().unwrap();
    let clock = SetClock::new();
    let c = catalog(&dir, &clock);
    c.put_owner(&owner("x-1")).unwrap();
    assert_eq!(c.owner("feed", "filings").unwrap(), Some(owner("x-1")));

    // A moved version applies nothing: the owner stays and the cursor keeps its row.
    assert_eq!(c.retire("feed", "filings", "x-1", Some((cursor("p1"), 4)), None).unwrap(), Cas::VersionMoved);
    assert_eq!(c.owner("feed", "filings").unwrap(), Some(owner("x-1")));
    assert_eq!(c.cursor("feed", "filings").unwrap().version, 0);

    // Another execution's retirement leaves this owner in place.
    assert_eq!(c.retire("feed", "filings", "x-0", None, None).unwrap(), Cas::Applied);
    assert!(c.owner("feed", "filings").unwrap().is_some());

    assert_eq!(c.retire("feed", "filings", "x-1", Some((cursor("p1"), 0)), None).unwrap(), Cas::Applied);
    assert_eq!(c.owner("feed", "filings").unwrap(), None);
    assert_eq!(c.cursor("feed", "filings").unwrap().position, Some(json!("p1")));
}

#[test]
fn run_rows_list_by_pipeline_and_a_refused_update_writes_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let clock = SetClock::new();
    let c = catalog(&dir, &clock);
    c.put_run(&run_row("run-2", "feed", RunStatus::Running)).unwrap();
    c.put_run(&run_row("run-1", "feed", RunStatus::Pending)).unwrap();
    c.put_run(&run_row("run-3", "other", RunStatus::Pending)).unwrap();
    assert_eq!(c.run("run-1").unwrap().unwrap().status, RunStatus::Pending);
    assert!(c.run("run-9").unwrap().is_none());
    let feed: Vec<String> = c.runs(Some("feed")).unwrap().into_iter().map(|r| r.run_id).collect();
    assert_eq!(feed, ["run-1", "run-2"]);
    assert_eq!(c.runs(None).unwrap().len(), 3);

    let refused = c.update_run("run-1", &mut |row| {
        row.status = RunStatus::Success;
        Err(RunError::CancelTargetNotInFlight("refused".into()))
    });
    assert!(matches!(refused.unwrap(), Some(Err(_))));
    assert_eq!(c.run("run-1").unwrap().unwrap().status, RunStatus::Pending, "a refused update writes nothing");
    let updated = c.update_run("run-1", &mut |row| {
        row.status = RunStatus::Running;
        Ok(())
    });
    assert_eq!(updated.unwrap().unwrap().unwrap().status, RunStatus::Running);
    assert!(c.update_run("run-9", &mut |_| Ok(())).unwrap().is_none());
}

/// The newest run start of one pipeline reads through an index on `pipeline_id`, so a
/// scheduler beat costs the pipeline's rows, not the whole run history.
#[test]
fn the_newest_run_start_reads_one_pipeline_through_an_index() {
    let dir = tempfile::tempdir().unwrap();
    let clock = SetClock::new();
    let c = catalog(&dir, &clock);
    assert_eq!(c.last_run_start("feed").unwrap(), None);
    for (id, pipeline, secs) in [("run-1", "feed", 0), ("run-2", "feed", 7200), ("run-3", "feed", 3600), ("run-4", "other", 9000)] {
        let mut row = run_row(id, pipeline, RunStatus::Success);
        row.started_at = at(T0).plus_secs(secs);
        c.put_run(&row).unwrap();
    }
    // A sub-second start sorts after the whole second before it.
    let mut row = run_row("run-5", "feed", RunStatus::Success);
    row.started_at = contextful_core::time::Instant::parse("2030-01-01T02:00:00.5Z").unwrap();
    c.put_run(&row).unwrap();
    assert_eq!(c.last_run_start("feed").unwrap(), Some(row.started_at));
    assert_eq!(c.last_run_start("other").unwrap(), Some(at(T0).plus_secs(9000)));
    assert_eq!(c.last_run_start("none").unwrap(), None);

    let conn = rusqlite::Connection::open(dir.path().join(MACHINE_CATALOG_FILE)).unwrap();
    let plan: Vec<String> = conn
        .prepare("EXPLAIN QUERY PLAN SELECT json_extract(row, '$.started_at') FROM run WHERE pipeline_id = 'feed'")
        .unwrap()
        .query_map([], |r| r.get::<_, String>(3))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    assert!(plan.iter().any(|d| d.contains("USING INDEX run_by_pipeline")), "{plan:?}");
}

#[test]
fn rows_outlive_the_connection_that_wrote_them() {
    let dir = tempfile::tempdir().unwrap();
    let clock = SetClock::new();
    {
        let c = catalog(&dir, &clock);
        c.acquire(&pipeline(), "a", 30).unwrap().unwrap();
        c.cursor_cas("feed", "filings", 0, cursor("p1"), None).unwrap();
        c.put_run(&run_row("run-1", "feed", RunStatus::Success)).unwrap();
    }
    let c = catalog(&dir, &clock);
    assert_eq!(c.lease_row(&pipeline()).unwrap().fence, 1);
    assert_eq!(c.cursor("feed", "filings").unwrap().version, 1);
    assert_eq!(c.run("run-1").unwrap().unwrap().status, RunStatus::Success);
}

/// Connections from several threads, each its own `MachineCatalog` on one file, race the
/// compare-and-swap; the file's write lock serializes them, so no update is lost and each
/// acquisition takes a distinct fence.
#[test]
fn connections_racing_on_one_file_lose_no_update() {
    let dir = tempfile::tempdir().unwrap();
    let clock = SetClock::new();
    catalog(&dir, &clock);
    const WRITERS: u64 = 6;
    const EACH: u64 = 15;
    let fences = std::sync::Mutex::new(Vec::new());
    std::thread::scope(|s| {
        for w in 0..WRITERS {
            let (dir, clock, fences) = (&dir, &clock, &fences);
            s.spawn(move || {
                let c = catalog(dir, clock);
                let mut applied = 0;
                while applied < EACH {
                    let seen = c.cursor("feed", "filings").unwrap().version;
                    if c.cursor_cas("feed", "filings", seen, cursor(&format!("w{w}")), None).unwrap() == Cas::Applied {
                        applied += 1;
                    }
                }
                let key = LeaseKey::Partition { pipeline: "feed".into(), partition: format!("p{w}") };
                let lease = c.acquire(&LeaseKey::Compaction("filings".into()), &format!("w{w}"), 0).unwrap();
                if let Some(l) = lease {
                    fences.lock().unwrap().push(l.fence);
                }
                c.acquire(&key, "w", 30).unwrap().unwrap();
            });
        }
    });
    let c = catalog(&dir, &clock);
    assert_eq!(c.cursor("feed", "filings").unwrap().version, WRITERS * EACH);
    let mut fences = fences.into_inner().unwrap();
    let n = fences.len();
    fences.sort_unstable();
    fences.dedup();
    assert_eq!(fences.len(), n, "no fence repeats: {fences:?}");
}

/// A `machine.sqlite` keyed on pipeline and table alone migrates to scope keys at open: each
/// table owner keeps its pipeline, table and stored owner text byte for byte, and chunk and
/// host scopes each hold their own owner and cursor beside it.
#[test]
fn a_table_keyed_file_migrates_to_scope_keys_keeping_each_table_owner() {
    let dir = tempfile::tempdir().unwrap();
    let clock = SetClock::new();
    let path = dir.path().join(MACHINE_CATALOG_FILE);
    let stored = serde_json::to_string(&owner("x-stored")).unwrap();
    let text = r#"{"execution_id":"x-stored","pipeline_id":"feed","table":"filings","pins":{"connector":{"id":"vendor","version":"1","world":"native","hash":"h"},"content_hash":"plan-1","input_hash":"input-1"},"attempts":["run-1"],"opened_at":"2030-01-01T00:00:00Z"}"#;
    assert_eq!(stored, text, "a table owner serializes as a table-keyed catalog stored it");
    {
        let old = rusqlite::Connection::open(&path).unwrap();
        old.execute_batch(
            "CREATE TABLE scope (pipeline_id TEXT NOT NULL, tbl TEXT NOT NULL, version INTEGER NOT NULL, cursor TEXT NOT NULL, owner TEXT, PRIMARY KEY (pipeline_id, tbl));",
        )
        .unwrap();
        let cursor = serde_json::to_string(&cursor("p1")).unwrap();
        old.execute("INSERT INTO scope VALUES ('feed', 'filings', 3, ?1, ?2)", rusqlite::params![cursor, text]).unwrap();
    }

    let c = catalog(&dir, &clock);
    assert_eq!(c.owner("feed", "filings").unwrap(), Some(owner("x-stored")), "the pending table owner resumes");
    assert_eq!(c.cursor("feed", "filings").unwrap().version, 3);
    c.put_owner(&owner("x-stored")).unwrap();

    let chunk = OwnerScope::chunk("feed", "filings", "2030-01");
    let host = OwnerScope::host("index-42");
    for (scope, id) in [(&chunk, "x-chunk"), (&host, "x-host")] {
        let mut o = owner(id);
        o.scope = scope.clone();
        c.put_owner(&o).unwrap();
        assert_eq!(c.cursor_cas_at(scope, 0, cursor(id), None).unwrap(), Cas::Applied);
    }
    assert_eq!(c.owner_at(&chunk).unwrap().unwrap().execution_id, "x-chunk");
    assert_eq!(c.owner_at(&host).unwrap().unwrap().execution_id, "x-host");
    assert_eq!(c.retire_at(&host, "x-host", None, None).unwrap(), Cas::Applied);
    assert!(c.owner_at(&host).unwrap().is_none());
    assert_eq!(c.owner_at(&chunk).unwrap().unwrap().execution_id, "x-chunk", "retiring one scope leaves the others");
    drop(c);

    let raw = rusqlite::Connection::open(&path).unwrap();
    let (pipeline, table, owner_text): (String, String, String) = raw
        .query_row("SELECT pipeline_id, tbl, owner FROM scope WHERE kind = 'table'", [], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
        .unwrap();
    assert_eq!((pipeline.as_str(), table.as_str(), owner_text.as_str()), ("feed", "filings", text), "the table row keeps its key and text");
    assert_eq!(catalog(&dir, &clock).cursor("feed", "filings").unwrap().version, 3, "a second open migrates nothing");
}

#[test]
fn scheduled_host_starts_survive_restart_without_output_runs() {
    let dir = tempfile::tempdir().unwrap();
    let clock = SetClock::new();
    let mut row = run_row("host-attempt", "", RunStatus::Running);
    row.host_scope = Some("job:score-documents".into());
    let started = row.started_at;
    catalog(&dir, &clock).put_run(&row).unwrap();
    let reopened = catalog(&dir, &clock);
    assert_eq!(reopened.last_run_start("job:score-documents").unwrap(), Some(started));
    assert_eq!(reopened.last_run_start("job:other").unwrap(), None);
    let db = rusqlite::Connection::open(dir.path().join(MACHINE_CATALOG_FILE)).unwrap();
    let mut statement = db.prepare("EXPLAIN QUERY PLAN SELECT json_extract(row, '$.started_at') FROM run WHERE pipeline_id = ?1 OR json_extract(row, '$.host_scope') = ?1").unwrap();
    let plan = statement.query_map(["job:score-documents"], |row| row.get::<_, String>(3)).unwrap().collect::<Result<Vec<_>, _>>().unwrap();
    assert!(plan.iter().any(|step| step.contains("MULTI-INDEX OR")), "{plan:?}");
    assert!(!plan.iter().any(|step| step.contains("SCAN")), "{plan:?}");
}
