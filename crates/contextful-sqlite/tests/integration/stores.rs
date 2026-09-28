//! `run.journal.sqlite-stores`: the journal, blob and awakeable stores over one SQLite
//! file pass every store conformance suite, and hold their claims across connections.

use contextful_core::coordinate::Catalog;
use contextful_core::run::journal::{sha256_hex, EntryKey, Row, Stored, INLINE_CUTOFF_BYTES};
use contextful_core::run::ports::{BlobStore, JournalStore};
use contextful_core::store::catalog::MACHINE_CATALOG_FILE;
use contextful_engine::conformance;
use contextful_sqlite::{MachineCatalog, SqliteRunStores};
use std::path::PathBuf;
use std::sync::Arc;
use tempfile::TempDir;

/// A fresh `machine.sqlite` under a directory `dirs` keeps alive for the test.
fn fresh(dirs: &mut Vec<TempDir>) -> SqliteRunStores {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join(MACHINE_CATALOG_FILE);
    dirs.push(dir);
    SqliteRunStores::open(&path).unwrap()
}

fn key(execution_id: &str) -> EntryKey {
    EntryKey::new(execution_id, "pull-0", b"null")
}

type Suite = (&'static str, Box<dyn Fn()>);

/// The three suites over one SQLite file per fresh store set.
fn suites() -> Vec<Suite> {
    vec![
        ("sqlite blob", Box::new(|| {
            let mut dirs = Vec::new();
            conformance::blob_store("sqlite", &mut || fresh(&mut dirs).blobs);
        })),
        ("sqlite journal", Box::new(|| {
            let mut dirs = Vec::new();
            conformance::journal_store("sqlite", &mut || {
                let s = fresh(&mut dirs);
                (s.journal, s.blobs)
            });
        })),
        ("sqlite awakeable", Box::new(|| {
            let mut dirs = Vec::new();
            conformance::awakeable_store("sqlite", &mut || {
                let s = fresh(&mut dirs);
                (s.awakeables, s.journal, s.blobs)
            });
        })),
    ]
}

/// The SQLite adapter passes the journal, blob and awakeable suites the file tree passes.
// spec: run.journal.sqlite-stores@c28b4db5
#[test]
fn the_sqlite_adapters_pass_every_store_conformance_suite() {
    let suites = suites();
    let failed: Vec<&str> = suites.iter().filter(|(_, run)| std::panic::catch_unwind(std::panic::AssertUnwindSafe(run)).is_err()).map(|(name, _)| *name).collect();
    contextful_eval::record::emit("sqlite-journal-conformance", failed.len() as f64, suites.len() as u64, 0);
    assert!(failed.is_empty(), "failed conformance suites: {failed:?}");
}

#[test]
fn the_file_runs_in_write_ahead_log_mode_and_holds_blobs_as_rows() {
    let mut dirs = Vec::new();
    let s = fresh(&mut dirs);
    let value = vec![b'b'; INLINE_CUTOFF_BYTES + 1];
    let sha = sha256_hex(&value);
    s.blobs.put(&sha, &value).unwrap();

    let conn = rusqlite::Connection::open(s.path()).unwrap();
    let mode: String = conn.query_row("PRAGMA journal_mode", [], |r| r.get(0)).unwrap();
    assert_eq!(mode, "wal");
    let held: Vec<u8> = conn.query_row("SELECT bytes FROM journal_blob WHERE sha256 = ?1", [&sha], |r| r.get(0)).unwrap();
    assert_eq!(held, value, "the blob is one row keyed by its sha256");
    let dir: PathBuf = s.path().parent().unwrap().to_path_buf();
    let names: Vec<String> = std::fs::read_dir(&dir).unwrap().map(|e| e.unwrap().file_name().to_string_lossy().into_owned()).collect();
    assert!(names.iter().all(|n| n.starts_with(MACHINE_CATALOG_FILE)), "no blob file lands beside the database: {names:?}");
}

#[test]
fn a_claim_is_exclusive_across_connections_to_one_file() {
    let mut dirs = Vec::new();
    let first = fresh(&mut dirs);
    let path = first.path().to_path_buf();
    let others: Vec<SqliteRunStores> = (0..4).map(|_| SqliteRunStores::open(&path).unwrap()).collect();
    let claimed: Vec<bool> = std::thread::scope(|s| {
        let handles: Vec<_> = std::iter::once(&first)
            .chain(others.iter())
            .enumerate()
            .map(|(i, stores)| s.spawn(move || stores.journal.create_pending(&key("x-1"), &format!("run-{i}")).unwrap()))
            .collect();
        handles.into_iter().map(|h| h.join().unwrap()).collect()
    });
    assert_eq!(claimed.iter().filter(|c| **c).count(), 1, "one connection claims the key: {claimed:?}");

    let Some(Row::Pending { run_id: holder, .. }) = others[0].journal.read(&key("x-1")).unwrap() else { panic!("the claim is readable") };
    assert!(others[1].journal.replace_if_pending(&key("x-1"), &holder, "run-z").unwrap(), "another connection takes the claim over");
    assert_eq!(first.journal.record(&key("x-1"), &Stored::place(b"v")).unwrap(), None);
    assert_eq!(others[2].journal.record(&key("x-1"), &Stored::place(b"w")).unwrap(), Some(Stored::place(b"v")), "the first record stands on every connection");
}

#[test]
fn a_reopened_file_holds_every_row_and_blob() {
    let mut dirs = Vec::new();
    let s = fresh(&mut dirs);
    let path = s.path().to_path_buf();
    let binary = Stored::place(&[0xff, 0x00, 0x7f]);
    s.journal.create_pending(&key("x-1"), "run-a").unwrap();
    s.journal.create_pending(&key("x-2"), "run-a").unwrap();
    s.journal.record(&key("x-2"), &binary).unwrap();
    s.blobs.put("aa", b"bytes").unwrap();
    s.blobs.mark_swept(42).unwrap();
    drop(s);

    let reopened = SqliteRunStores::open(&path).unwrap();
    assert_eq!(reopened.journal.read(&key("x-1")).unwrap(), Some(Row::Pending { key: key("x-1"), run_id: "run-a".into() }));
    assert_eq!(reopened.journal.read(&key("x-2")).unwrap(), Some(Row::Recorded { key: key("x-2"), value: binary }), "non-UTF-8 inline bytes round-trip");
    assert_eq!(reopened.blobs.get("aa").unwrap().as_deref(), Some(&b"bytes"[..]));
    assert_eq!(reopened.blobs.swept_at().unwrap(), Some(42));
    assert_eq!(reopened.journal.executions().unwrap(), ["x-1", "x-2"]);
}

#[test]
fn the_run_stores_share_machine_sqlite_with_the_catalog() {
    let mut dirs = Vec::new();
    let s = fresh(&mut dirs);
    let catalog = MachineCatalog::open(s.path(), Arc::new(crate::SetClock::new())).unwrap();
    catalog.put_run(&crate::run_row("run-1", "feed", contextful_core::run::record::RunStatus::Running)).unwrap();
    s.journal.create_pending(&key("x-1"), "run-1").unwrap();
    assert!(catalog.run("run-1").unwrap().is_some(), "the catalog's rows stand beside the journal's");
    assert!(s.journal.read(&key("x-1")).unwrap().is_some());
}
