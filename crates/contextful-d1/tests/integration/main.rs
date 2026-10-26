mod catalog_cost;
mod publication;
mod size;
mod transport;


use contextful_core::coordinate::{
    Cas, Catalog, CursorRow, ExecutionAdmission, ExecutionExpectation, LeaseKey,
};
use contextful_core::export::{change_events, change_state, parse_exports, ExportPublication};
use contextful_core::run::journal::{EntryKey, Row, Stored};
use contextful_core::run::own::{ConnectorPin, ExecutionOwner, OwnerScope, Pins};
use contextful_core::run::ports::AwakeableStore;
use contextful_core::run::ports::{BlobStore, JournalStore};
use contextful_core::run::record::{Owner as RunOwner, Phase, RunRow, RunStatus};
use contextful_core::run::suspend::{Awakeable, AwakeableState};
use contextful_core::time::Instant;
use contextful_d1::{D1, RecoveryObject};
use serde_json::json;
use sha2::{Digest, Sha256};
use std::io::{BufRead, BufReader};
use std::io::{Read, Write};
use std::net::TcpListener;
use std::process::{Child, Command, Stdio};

struct Fixture {
    _root: tempfile::TempDir,
    process: Child,
    endpoint: String,
}

impl Fixture {
    fn start() -> Self {
        Self::start_capped(None)
    }

    /// A fixture whose statement requests over `statement_bytes` lose their connection.
    fn start_capped(statement_bytes: Option<usize>) -> Self {
        let root = tempfile::tempdir().unwrap();
        let mut process = Command::new("node")
            .arg(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixture.mjs"))
            .arg(root.path())
            .args(statement_bytes.map(|bytes| bytes.to_string()))
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let mut line = String::new();
        BufReader::new(process.stdout.take().unwrap())
            .read_line(&mut line)
            .unwrap();
        let port: u16 = line.trim().parse().unwrap();
        Self {
            _root: root,
            process,
            endpoint: format!("http://127.0.0.1:{port}"),
        }
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = self.process.kill();
        let _ = self.process.wait();
    }
}

#[test]
fn an_external_or_postgres_endpoint_cannot_impersonate_the_private_binding() {
    for endpoint in [
        "postgres://localhost/execution",
        "https://example.com",
        "http://example.com",
    ] {
        assert!(D1::connect(endpoint).is_err(), "{endpoint}");
    }
    assert!(D1::connect("http://contextful.d1").is_ok());
}

#[test]
fn a_corrupt_r2_blob_cannot_be_returned_as_a_recorded_value() {
    let server = TcpListener::bind("127.0.0.1:0").unwrap();
    let endpoint = format!("http://{}", server.local_addr().unwrap());
    let expected = format!("{:x}", Sha256::digest(b"recorded value"));
    let worker = std::thread::spawn(move || {
        let (mut stream, _) = server.accept().unwrap();
        let mut request = [0; 1024];
        let count = stream.read(&mut request).unwrap();
        assert!(String::from_utf8_lossy(&request[..count]).contains("GET /blob/"));
        stream
            .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 7\r\n\r\nchanged")
            .unwrap();
    });
    let client = D1::connect(&endpoint).unwrap();
    assert!(client.get_blob(&expected).is_err());
    worker.join().unwrap();
}

// spec: run.journal.d1-recovery@74ff7a6f
#[test]
fn a_replacement_checks_the_same_d1_schema_and_recovery_identity() {
    let fixture = Fixture::start();
    let first = D1::connect(&fixture.endpoint).unwrap();
    first
        .initialize_recovery("sample", "backup-generation")
        .unwrap();
    let replacement = D1::connect(&fixture.endpoint).unwrap();
    replacement
        .verify_recovery("sample", "backup-generation")
        .unwrap();
    assert!(replacement
        .verify_recovery("sample", "different-generation")
        .is_err());
    assert!(replacement
        .verify_recovery("another-project", "backup-generation")
        .is_err());
    let frontier = replacement
        .query(
            "SELECT frontier_sha256 FROM contextful_recovery WHERE id = 1",
            vec![],
        )
        .unwrap();
    let digest = frontier.rows[0]["frontier_sha256"].as_str().unwrap();
    replacement.delete_blob(digest).unwrap();
    assert!(replacement
        .verify_recovery("sample", "backup-generation")
        .is_err());
}

#[test]
fn a_replacement_refuses_a_missing_recorded_r2_object_before_execution() {
    let fixture = Fixture::start();
    let db = D1::connect(&fixture.endpoint).unwrap();
    db.initialize_recovery("sample", "backup-generation").unwrap();
    let bytes = b"recorded result";
    let digest = format!("{:x}", Sha256::digest(bytes));
    db.put_blob(&digest, bytes).unwrap();
    db.query(
        "INSERT INTO contextful_blob (sha256, put_at) VALUES (?, 1)",
        vec![json!(digest)],
    ).unwrap();
    db.verify_recovery("sample", "backup-generation").unwrap();
    db.delete_blob(&digest).unwrap();
    assert!(db.verify_recovery("sample", "backup-generation").is_err());
}

#[test]
fn a_malformed_capture_keeps_the_recovery_identity_usable() {
    let fixture = Fixture::start();
    let db = D1::connect(&fixture.endpoint).unwrap();
    db.initialize_recovery("sample", "backup-generation").unwrap();
    let before = db.query("SELECT frontier_sha256 FROM contextful_recovery WHERE id = 1", vec![]).unwrap();
    let original = before.rows[0]["frontier_sha256"].as_str().unwrap();
    let invalid = RecoveryObject { key: "corpus/object".into(), sha256: "invalid".into() };
    assert!(db.capture_recovery("sample", "backup-generation", &[invalid]).is_err());
    let after = db.query("SELECT frontier_sha256 FROM contextful_recovery WHERE id = 1", vec![]).unwrap();
    assert_eq!(after.rows[0]["frontier_sha256"], original);
    db.verify_recovery("sample", "backup-generation").unwrap();
}

#[test]
fn d1_run_timestamps_remain_exact_text_for_sql_export() {
    let fixture = Fixture::start();
    let db = D1::connect(&fixture.endpoint).unwrap();
    db.initialize_recovery("sample", "generation").unwrap();
    let timestamp = "1791918353123456789";
    db.query(
        "INSERT INTO contextful_run (run_id, pipeline_id, started_ns, row) VALUES (?, ?, ?, ?)",
        vec![json!("run"), json!("pipeline"), json!(timestamp), json!("{}")],
    )
    .unwrap();
    let result = db
        .query(
            "SELECT typeof(started_ns) AS kind, CAST(started_ns AS TEXT) AS exact FROM contextful_run",
            vec![],
        )
        .unwrap();
    assert_eq!(result.rows[0]["kind"], "text");
    assert_eq!(result.rows[0]["exact"], timestamp);
}

#[test]
fn d1_run_timestamp_text_keeps_numeric_cadence_order() {
    let fixture = Fixture::start();
    let db = D1::connect(&fixture.endpoint).unwrap();
    db.initialize_recovery("sample", "generation").unwrap();
    for (run_id, timestamp) in [("older", "99"), ("newer", "100")] {
        db.query(
            "INSERT INTO contextful_run (run_id, pipeline_id, started_ns, row) VALUES (?, ?, ?, ?)",
            vec![json!(run_id), json!("pipeline"), json!(timestamp), json!("{}")],
        )
        .unwrap();
    }
    let catalog = contextful_d1::D1Catalog::connect(&fixture.endpoint).unwrap();
    assert_eq!(
        catalog.last_run_start("pipeline").unwrap(),
        Some(Instant::from_unix_nanos(100).unwrap())
    );
}

#[test]
fn an_uninitialized_d1_database_refuses_execution() {
    let fixture = Fixture::start();
    let client = D1::connect(&fixture.endpoint).unwrap();
    assert!(client
        .verify_recovery("sample", "backup-generation")
        .is_err());
}

#[test]
fn recorded_steps_survive_replacement_and_refuse_a_second_effect() {
    let fixture = Fixture::start();
    let first = D1::connect(&fixture.endpoint).unwrap();
    first.initialize_recovery("sample", "generation").unwrap();
    let key = EntryKey {
        execution_id: "execution".into(),
        step_label: "paid-call".into(),
        input_hash: "input".into(),
    };
    let journal = contextful_d1::D1JournalStore::connect(&fixture.endpoint).unwrap();
    assert!(journal.create_pending(&key, "run-a").unwrap());
    assert!(!journal.create_pending(&key, "run-b").unwrap());
    assert!(journal.replace_if_pending(&key, "run-a", "run-b").unwrap());
    assert!(!journal.replace_if_pending(&key, "run-a", "run-c").unwrap());
    let value = Stored::place(b"receipt");
    assert_eq!(journal.record(&key, &value).unwrap(), None);
    drop(journal);
    let replacement = contextful_d1::D1JournalStore::connect(&fixture.endpoint).unwrap();
    assert!(matches!(
        replacement.read(&key).unwrap(),
        Some(Row::Recorded { .. })
    ));
    assert_eq!(
        replacement
            .record(&key, &Stored::place(b"second receipt"))
            .unwrap(),
        Some(value)
    );
    assert_eq!(replacement.executions().unwrap(), vec!["execution"]);
}

#[test]
fn a_replacement_recovers_an_awakeable_from_its_recorded_callback() {
    let fixture = Fixture::start();
    D1::connect(&fixture.endpoint)
        .unwrap()
        .initialize_recovery("sample", "generation")
        .unwrap();
    let store = contextful_d1::D1AwakeableStore::connect(&fixture.endpoint).unwrap();
    let row =
        Awakeable::mint("token", "execution", "callback", "2026-01-01T00:00:00Z", 60).unwrap();
    store.insert(&row).unwrap();
    let key = EntryKey {
        execution_id: "execution".into(),
        step_label: "awakeable:token".into(),
        input_hash: contextful_core::run::journal::awakeable_key("token"),
    };
    let journal = contextful_d1::D1JournalStore::connect(&fixture.endpoint).unwrap();
    journal.record(&key, &Stored::place(b"answer")).unwrap();
    drop(store);
    let replacement = contextful_d1::D1AwakeableStore::connect(&fixture.endpoint).unwrap();
    let recovered = replacement.get("token").unwrap().unwrap();
    assert_eq!(recovered.state, AwakeableState::Resolved);
    assert_eq!(recovered.payload, Some(Stored::place(b"answer")));
    assert_eq!(
        replacement
            .update("token", &mut |row| {
                row.ttl_secs = 120;
                Ok(true)
            })
            .unwrap()
            .unwrap()
            .ttl_secs,
        120
    );
}

#[test]
fn a_durable_callback_guard_blocks_lease_transfer_until_release() {
    let fixture = Fixture::start();
    D1::connect(&fixture.endpoint)
        .unwrap()
        .initialize_recovery("sample", "generation")
        .unwrap();
    let catalog = contextful_d1::D1Catalog::connect(&fixture.endpoint).unwrap();
    let lease = catalog
        .acquire(&LeaseKey::Cadence("export".into()), "first", 90)
        .unwrap()
        .unwrap();
    let admission = ExecutionAdmission::new(vec![], vec![lease.clone()], false).unwrap();
    let guard = catalog.execution_guard(&admission).unwrap();
    let replacement = contextful_d1::D1Catalog::connect(&fixture.endpoint).unwrap();
    assert!(replacement.release(&lease).is_err());
    drop(guard);
    replacement.release(&lease).unwrap();
    let next = replacement
        .acquire(&LeaseKey::Cadence("export".into()), "second", 90)
        .unwrap()
        .unwrap();
    assert!(next.fence > lease.fence);
}

// spec: run.journal.d1-guard@c06df09b
#[test]
fn a_crashed_d1_guard_requires_explicit_reconciliation_before_takeover() {
    let fixture = Fixture::start();
    let db = D1::connect(&fixture.endpoint).unwrap();
    db.initialize_recovery("sample", "generation").unwrap();
    let catalog = contextful_d1::D1Catalog::connect(&fixture.endpoint).unwrap();
    let admission = ExecutionAdmission::new(vec![], vec![], true).unwrap();
    let guard = catalog.execution_guard(&admission).unwrap();
    std::mem::forget(guard);
    let replacement = contextful_d1::D1Catalog::connect(&fixture.endpoint).unwrap();
    assert!(replacement.execution_guard(&admission).is_err());
    let token = replacement
        .pending_guard()
        .unwrap()
        .expect("durable guard token");
    assert!(replacement
        .reconcile_guard("wrong-token", "idempotency:key-1")
        .is_err());
    assert!(replacement.reconcile_guard(&token, "").is_err());
    assert!(replacement.execution_guard(&admission).is_err());
    replacement
        .reconcile_guard(&token, "idempotency:key-1")
        .unwrap();
    assert!(replacement.pending_guard().unwrap().is_none());
    assert!(replacement.execution_guard(&admission).is_ok());
    let recorded = db
        .query(
            "SELECT evidence FROM contextful_reconciliation WHERE token = ?",
            vec![json!(token)],
        )
        .unwrap();
    assert_eq!(recorded.rows[0]["evidence"], "idempotency:key-1");
}

fn owner() -> ExecutionOwner {
    ExecutionOwner {
        execution_id: "execution".into(),
        scope: OwnerScope::table("feed", "filings"),
        pins: Pins {
            connector: ConnectorPin {
                id: "source".into(),
                version: "1".into(),
                world: "native".into(),
                hash: "hash".into(),
            },
            content_hash: "plan".into(),
            input_hash: "input".into(),
        }
        .into(),
        attempts: vec!["run-1".into()],
        opened_at: Instant::parse("2026-01-01T00:00:00Z").unwrap(),
        next_attempt_at: None,
    }
}

#[test]
fn a_d1_owner_and_cursor_retire_atomically_after_replacement() {
    let fixture = Fixture::start();
    D1::connect(&fixture.endpoint)
        .unwrap()
        .initialize_recovery("sample", "generation")
        .unwrap();
    let first = contextful_d1::D1Catalog::connect(&fixture.endpoint).unwrap();
    first.put_owner(&owner()).unwrap();
    let scope = OwnerScope::table("feed", "filings");
    let cursor = CursorRow {
        position: Some(json!("page-2")),
        ..CursorRow::default()
    };
    assert_eq!(
        first
            .retire_at(&scope, "execution", Some((cursor.clone(), 4)), None)
            .unwrap(),
        Cas::VersionMoved
    );
    assert_eq!(first.owner_at(&scope).unwrap(), Some(owner()));
    drop(first);
    let replacement = contextful_d1::D1Catalog::connect(&fixture.endpoint).unwrap();
    assert_eq!(
        replacement
            .retire_at(&scope, "execution", Some((cursor, 0)), None)
            .unwrap(),
        Cas::Applied
    );
    assert_eq!(replacement.owner_at(&scope).unwrap(), None);
    assert_eq!(
        replacement.cursor_at(&scope).unwrap().position,
        Some(json!("page-2"))
    );
    assert_eq!(
        replacement.retired_at(&scope).unwrap().as_deref(),
        Some("execution")
    );
}

fn run_row(id: &str) -> RunRow {
    RunRow {
        run_id: id.into(),
        pipeline_id: "feed".into(),
        table: "filings".into(),
        site_id: "site".into(),
        status: RunStatus::Pending,
        owner: None,
        started_at: Instant::parse("2026-01-01T00:00:00Z").unwrap(),
        ended_at: None,
        rows: 0,
        bytes: 0,
        batches: 0,
        skipped: 0,
        fetched: 0,
        kept: 0,
        declined: Default::default(),
        audit: vec![],
        error_kind: None,
        error_message: None,
        connector_id: "source".into(),
        connector_version: "1".into(),
        connector_hash: "hash".into(),
        trace_id: None,
        phase: Phase::Plan,
        pipeline_phase: None,
        execution_id: "execution".into(),
        stop: None,
        host_scope: None,
        plan_ref: None,
        input: None,
        failed_branches: vec![],
        credential_providers: Default::default(),
        bound_hits: Vec::new(),
        next_attempt_at: None,
        unpushed: None,
    }
}

#[test]
fn d1_run_rows_and_updates_survive_replacement() {
    let fixture = Fixture::start();
    D1::connect(&fixture.endpoint)
        .unwrap()
        .initialize_recovery("sample", "generation")
        .unwrap();
    let first = contextful_d1::D1Catalog::connect(&fixture.endpoint).unwrap();
    first.put_run(&run_row("run-b")).unwrap();
    first.put_run(&run_row("run-a")).unwrap();
    drop(first);
    let replacement = contextful_d1::D1Catalog::connect(&fixture.endpoint).unwrap();
    assert_eq!(
        replacement
            .runs(Some("feed"))
            .unwrap()
            .iter()
            .map(|r| r.run_id.as_str())
            .collect::<Vec<_>>(),
        vec!["run-a", "run-b"]
    );
    replacement
        .update_run("run-a", &mut |row| {
            row.status = RunStatus::Running;
            Ok(())
        })
        .unwrap()
        .unwrap()
        .unwrap();
    assert_eq!(
        replacement.run("run-a").unwrap().unwrap().status,
        RunStatus::Running
    );
    let mut precise = run_row("run-precise");
    precise.started_at = Instant::parse("2026-01-01T00:00:00.123456789Z").unwrap();
    replacement.put_run(&precise).unwrap();
    assert_eq!(
        replacement.last_run_start("feed").unwrap(),
        Some(precise.started_at)
    );
}

/// History reads through a window, newest first; the bound and the order read the indexed integer the start
/// text spells, and the ceiling sits in the `LIMIT`.
#[test]
fn d1_history_selects_its_window_over_the_indexed_start() {
    use contextful_core::run::record::Window;
    let fixture = Fixture::start();
    let db = D1::connect(&fixture.endpoint).unwrap();
    db.initialize_recovery("sample", "generation").unwrap();
    let catalog = contextful_d1::D1Catalog::connect(&fixture.endpoint).unwrap();
    for (id, pipeline, start) in [
        ("run-1", "feed", "2026-01-01T00:00:00Z"),
        ("run-2", "feed", "2026-01-02T00:00:00.5Z"),
        ("run-b", "feed", "2026-01-03T00:00:00Z"),
        ("run-B", "feed", "2026-01-03T00:00:00Z"),
        ("run-4", "other", "2026-01-04T00:00:00Z"),
    ] {
        let mut row = run_row(id);
        row.pipeline_id = pipeline.into();
        row.started_at = Instant::parse(start).unwrap();
        catalog.put_run(&row).unwrap();
    }
    let at = |s: &str| Some(Instant::parse(s).unwrap());
    let ids = |page: contextful_core::run::record::HistoryPage| (page.runs.into_iter().map(|r| r.run_id).collect::<Vec<_>>(), page.truncated);
    let page = catalog.history(Some("feed"), &Window { since: at("2026-01-01T00:00:00Z"), ceiling: 2 }).unwrap();
    assert_eq!(ids(page), (vec!["run-b".to_string(), "run-B".into()], true));
    let page = catalog.history(Some("feed"), &Window { since: at("2026-01-02T00:00:00.5Z"), ceiling: 10 }).unwrap();
    assert_eq!(ids(page), (vec!["run-b".to_string(), "run-B".into(), "run-2".into()], false));
    let page = catalog.history(None, &Window { since: at("2026-01-03T00:00:00Z"), ceiling: 10 }).unwrap();
    assert_eq!(ids(page).0, ["run-4", "run-b", "run-B"]);
    let page = catalog.history(Some("feed"), &Window { since: None, ceiling: 1 }).unwrap();
    assert_eq!(ids(page), (vec!["run-b".to_string()], true));
    let indexes = db.query("SELECT name FROM sqlite_master WHERE type = 'index' AND tbl_name = 'contextful_run'", vec![]).unwrap();
    let names: Vec<&str> = indexes.rows.iter().filter_map(|r| r.get("name").and_then(serde_json::Value::as_str)).collect();
    assert!(names.contains(&"contextful_run_by_pipeline_start") && names.contains(&"contextful_run_by_start"), "{names:?}");
}

#[test]
fn a_d1_callback_guard_blocks_owner_transfer_but_allows_run_metadata() {
    let fixture = Fixture::start();
    D1::connect(&fixture.endpoint)
        .unwrap()
        .initialize_recovery("sample", "generation")
        .unwrap();
    let catalog = contextful_d1::D1Catalog::connect(&fixture.endpoint).unwrap();
    let expected = owner();
    catalog.put_owner(&expected).unwrap();
    let mut run = run_row("run-1");
    run.status = RunStatus::Running;
    run.owner = Some(RunOwner::leased(1, "boot", catalog.now().unwrap()));
    catalog.put_run(&run).unwrap();
    let admission = ExecutionAdmission::new(
        vec![ExecutionExpectation {
            scope: expected.scope.clone(),
            execution_id: expected.execution_id.clone(),
            pins: expected.pins.clone(),
            run_id: "run-1".into(),
        }],
        vec![],
        false,
    )
    .unwrap();
    let guard = catalog.execution_guard(&admission).unwrap();
    let replacement = contextful_d1::D1Catalog::connect(&fixture.endpoint).unwrap();
    let mut moved = expected.clone();
    moved.execution_id = "second-execution".into();
    assert!(replacement.put_owner(&moved).is_err());
    replacement
        .update_run("run-1", &mut |row| {
            row.rows = 1;
            Ok(())
        })
        .unwrap()
        .unwrap()
        .unwrap();
    drop(guard);
    replacement.put_owner(&moved).unwrap();
    assert!(catalog.execution_guard(&admission).is_err());
}

// spec: run.journal.d1-blob@0af768ba
#[test]
fn a_recorded_r2_blob_survives_replacement_and_sweep() {
    let fixture = Fixture::start();
    D1::connect(&fixture.endpoint)
        .unwrap()
        .initialize_recovery("sample", "generation")
        .unwrap();
    let sha = format!("{:x}", Sha256::digest(b"large recorded value"));
    let db = D1::connect(&fixture.endpoint).unwrap();
    db.put_blob(&sha, b"large recorded value").unwrap();
    assert!(db
        .query(
            "SELECT sha256 FROM contextful_blob WHERE sha256 = ?",
            vec![json!(sha)]
        )
        .unwrap()
        .rows
        .is_empty());
    assert!(db.get_blob(&sha).unwrap().is_some());
    let blobs = contextful_d1::D1BlobStore::connect(&fixture.endpoint).unwrap();
    blobs.put(&sha, b"large recorded value").unwrap();
    let key = EntryKey::new("execution", "paid-call", b"input");
    let journal = contextful_d1::D1JournalStore::connect(&fixture.endpoint).unwrap();
    journal
        .record(
            &key,
            &Stored::Blob {
                sha256: sha.clone(),
                bytes: 20,
            },
        )
        .unwrap();
    drop(blobs);
    let replacement = contextful_d1::D1BlobStore::connect(&fixture.endpoint).unwrap();
    assert_eq!(
        replacement.get(&sha).unwrap().as_deref(),
        Some(b"large recorded value".as_slice())
    );
    assert!(replacement.sweep(&[], i64::MAX).unwrap().is_empty());
    assert!(replacement.get(&sha).unwrap().is_some());
    journal.retire("execution").unwrap();
    assert_eq!(replacement.sweep(&[], i64::MAX).unwrap(), vec![sha.clone()]);
    assert!(replacement.get(&sha).unwrap().is_none());
}

// spec: run.export.d1-state@da5236d3
#[test]
fn a_lost_sink_acknowledgement_reoffers_the_same_d1_event_identity() {
    let fixture = Fixture::start();
    D1::connect(&fixture.endpoint)
        .unwrap()
        .initialize_recovery("sample", "generation")
        .unwrap();
    let export = parse_exports("[[export]]\nname='sink'\ntable='items'\nendpoint='https://sink.example/events'\nformat='changes-v1'\nkey=['id']\nschedule='every 30s'\n").unwrap().remove(0);
    let state = change_state(&export, &[json!({"id":"a","value":7})]).unwrap();
    let events = change_events(&export, "publication-1", 0, &Default::default(), &state).unwrap();
    let mut first = contextful_d1::D1ExportLedger::connect(&fixture.endpoint).unwrap();
    let checkpoint = contextful_core::export::ExportCursor {
        commit_seq: 4,
        row_seq: 8,
    };
    first.advance_checkpoint("sink", checkpoint).unwrap();
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
    first.offer("sink", sent.last().unwrap().sequence).unwrap();
    drop(first);
    let mut replacement = contextful_d1::D1ExportLedger::connect(&fixture.endpoint).unwrap();
    assert_eq!(replacement.checkpoint("sink").unwrap(), checkpoint);
    assert!(replacement
        .advance_checkpoint("sink", Default::default())
        .is_err());
    assert!(replacement
        .acknowledge("sink", sent.last().unwrap().sequence)
        .is_err());
    let retried = replacement.pending("sink", 500).unwrap();
    assert_eq!(retried, sent);
    let through = retried.last().unwrap().sequence;
    replacement.offer("sink", through).unwrap();
    replacement.acknowledge("sink", through).unwrap();
    assert_eq!(replacement.state("sink").unwrap(), state);
    assert!(replacement.pending("sink", 500).unwrap().is_empty());
}

#[test]
fn a_restored_d1_export_checkpoint_replays_deduplicable_events() {
    let fixture = Fixture::start();
    let db = D1::connect(&fixture.endpoint).unwrap();
    db.initialize_recovery("sample", "generation").unwrap();
    let export = parse_exports("[[export]]\nname='sink'\ntable='items'\nendpoint='https://sink.example/events'\nformat='changes-v1'\nkey=['id']\nschedule='every 30s'\n").unwrap().remove(0);
    let state = change_state(&export, &[json!({"id":"a","value":7})]).unwrap();
    let events = change_events(&export, "publication-1", 0, &Default::default(), &state).unwrap();
    let mut sender = contextful_d1::D1ExportLedger::connect(&fixture.endpoint).unwrap();
    let old_checkpoint = contextful_core::export::ExportCursor { commit_seq: 4, row_seq: 8 };
    let new_checkpoint = contextful_core::export::ExportCursor { commit_seq: 5, row_seq: 9 };
    sender.advance_checkpoint("sink", old_checkpoint).unwrap();
    sender.stage("sink", ExportPublication {
        source: "source-1", id: "publication-1", identity: "identity-1", destination: "destination-1"
    }, &events, &state).unwrap();
    let backup = db.query("SELECT body FROM contextful_export WHERE name = ?", vec![json!("sink")]).unwrap().rows[0]["body"].as_str().unwrap().to_owned();
    sender.advance_checkpoint("sink", new_checkpoint).unwrap();
    let sent = sender.pending("sink", 500).unwrap();
    let mut sink = std::collections::BTreeMap::new();
    for event in &sent {
        assert!(sink.insert(event.id.clone(), event.clone()).is_none());
    }
    let through = sent.last().unwrap().sequence;
    sender.offer("sink", through).unwrap();
    sender.acknowledge("sink", through).unwrap();
    assert_eq!(sender.checkpoint("sink").unwrap(), new_checkpoint);
    drop(sender);

    db.query("UPDATE contextful_export SET version = version + 1, body = ? WHERE name = ?", vec![json!(backup), json!("sink")]).unwrap();
    let mut replacement = contextful_d1::D1ExportLedger::connect(&fixture.endpoint).unwrap();
    assert_eq!(replacement.checkpoint("sink").unwrap(), old_checkpoint);
    let retried = replacement.pending("sink", 500).unwrap();
    assert_eq!(retried, sent);
    for event in &retried {
        assert_eq!(sink.get(&event.id), Some(event));
    }
    assert_eq!(sink.len(), sent.len());
    replacement.offer("sink", through).unwrap();
    replacement.acknowledge("sink", through).unwrap();
    assert_eq!(replacement.state("sink").unwrap(), state);
    assert!(replacement.pending("sink", 500).unwrap().is_empty());
}
