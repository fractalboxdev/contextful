//! `run.journal.sqlite-stores`: the journal, blob and awakeable stores over one SQLite
//! file pass every store conformance suite, commit an awakeable update with the journal
//! writes inside it, and hold their claims across connections.

use contextful_core::coordinate::Catalog;
use contextful_core::run::journal::{sha256_hex, EntryKey, Row, Stored, INLINE_CUTOFF_BYTES};
use contextful_core::run::ports::{AwakeableStore, BlobStore, JournalStore};
use contextful_core::run::suspend::{Awakeable, AwakeableState};
use contextful_core::run::{Failure, FailureTag};
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

/// The three conformance suites, the restart check and the update atomicity cases, over one SQLite file per
/// fresh store set.
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
        ("sqlite awakeable restart", Box::new(|| {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join(MACHINE_CATALOG_FILE);
            conformance::awakeable_restart("sqlite", &mut || {
                let s = SqliteRunStores::open(&path).unwrap();
                (s.awakeables, s.journal, s.blobs)
            });
        })),
        ("sqlite update commits its journal write", Box::new(an_awakeable_update_commits_its_journal_write_with_the_row)),
        ("sqlite failed update rolls back", Box::new(a_failed_awakeable_update_rolls_its_journal_write_back)),
        ("sqlite failed nested update rolls back", Box::new(a_failed_nested_update_rolls_back_alone)),
    ]
}

/// The SQLite adapter passes the journal, blob and awakeable suites the file tree passes,
/// and an awakeable update commits or rolls back with the journal writes inside it.
// spec: run.journal.sqlite-stores@c28b4db5
#[test]
fn the_sqlite_stores_pass_every_conformance_suite_and_commit_updates_atomically() {
    let suites = suites();
    let failed: Vec<&str> = suites.iter().filter(|(_, run)| std::panic::catch_unwind(std::panic::AssertUnwindSafe(run)).is_err()).map(|(name, _)| *name).collect();
    contextful_eval::record::emit("sqlite-journal-conformance", failed.len() as f64, suites.len() as u64, 0);
    assert!(failed.is_empty(), "failed cases: {failed:?}");
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

/// Eight connections to one file record one key at once, over 16 keys: each write opens
/// an immediate transaction, so one value stands and no read upgrades into a busy snapshot.
// spec: run.journal.sqlite-write-lock@1ad72e8a
#[test]
fn concurrent_records_of_one_key_across_connections_return_one_value() {
    let mut dirs = Vec::new();
    let first = fresh(&mut dirs);
    let path = first.path().to_path_buf();
    let stores: Vec<SqliteRunStores> = std::iter::once(first).chain((0..7).map(|_| SqliteRunStores::open(&path).unwrap())).collect();
    for round in 0..16 {
        let k = key(&format!("x-{round}"));
        let barrier = std::sync::Barrier::new(stores.len());
        let (k, barrier) = (&k, &barrier);
        let results: Vec<Result<Option<Stored>, Failure>> = std::thread::scope(|s| {
            let handles: Vec<_> = stores
                .iter()
                .enumerate()
                .map(|(i, stores)| {
                    s.spawn(move || {
                        barrier.wait();
                        stores.journal.record(k, &Stored::place(format!("v-{i}").as_bytes()))
                    })
                })
                .collect();
            handles.into_iter().map(|h| h.join().unwrap()).collect()
        });
        let failed: Vec<String> = results.iter().filter_map(|r| r.as_ref().err().map(|e| format!("{e:?}"))).collect();
        assert!(failed.is_empty(), "round {round}: no record fails: {failed:?}");
        let winners: Vec<usize> = results.iter().enumerate().filter(|(_, r)| matches!(r, Ok(None))).map(|(i, _)| i).collect();
        assert_eq!(winners.len(), 1, "round {round}: one record lands: {results:?}");
        let standing = Stored::place(format!("v-{}", winners[0]).as_bytes());
        for (i, r) in results.iter().enumerate().filter(|(i, _)| *i != winners[0]) {
            assert_eq!(r.as_ref().unwrap().as_ref(), Some(&standing), "round {round}: connection {i} returns the standing value");
        }
        assert_eq!(stores[0].journal.read(k).unwrap(), Some(Row::Recorded { key: k.clone(), value: standing }));
    }
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

/// A pending awakeable row under `token` for execution `x-1`.
fn awakeable(token: &str) -> Awakeable {
    Awakeable {
        token: token.into(),
        execution_id: "x-1".into(),
        step_label: "approve".into(),
        created_at: crate::at(crate::T0),
        ttl_secs: 60,
        deadline: crate::at("2030-01-01T00:01:00Z"),
        state: AwakeableState::Pending,
        payload_sha256: None,
        payload: None,
    }
}

/// Journal rows under `execution_id` as another connection to the file reads them.
fn committed(path: &std::path::Path, execution_id: &str) -> i64 {
    let conn = rusqlite::Connection::open(path).unwrap();
    conn.query_row("SELECT COUNT(*) FROM journal WHERE execution_id = ?1", [execution_id], |r| r.get(0)).unwrap()
}

fn an_awakeable_update_commits_its_journal_write_with_the_row() {
    let mut dirs = Vec::new();
    let s = fresh(&mut dirs);
    let path = s.path().to_path_buf();
    s.awakeables.insert(&awakeable("tok-a")).unwrap();

    let updated = s
        .awakeables
        .update("tok-a", &mut |row| {
            assert_eq!(s.journal.record(&key("x-1"), &Stored::place(b"yes")).unwrap(), None);
            assert_eq!(committed(&path, "x-1"), 0, "another connection sees no journal write before the update commits");
            row.state = AwakeableState::Resolved;
            Ok(true)
        })
        .unwrap();
    assert_eq!(updated.map(|r| r.state), Some(AwakeableState::Resolved));
    assert_eq!(committed(&path, "x-1"), 1, "the journal write commits with the row");
}

fn a_failed_awakeable_update_rolls_its_journal_write_back() {
    let mut dirs = Vec::new();
    let s = fresh(&mut dirs);
    let path = s.path().to_path_buf();
    s.awakeables.insert(&awakeable("tok-a")).unwrap();

    let failed = s.awakeables.update("tok-a", &mut |row| {
        s.journal.record(&key("x-1"), &Stored::place(b"yes"))?;
        row.state = AwakeableState::Resolved;
        Err(Failure::new(FailureTag::Storage, "the edit fails after its journal write"))
    });
    assert!(failed.is_err());
    assert_eq!(s.journal.read(&key("x-1")).unwrap(), None, "the journal write rolls back with the update");
    assert_eq!(committed(&path, "x-1"), 0);
    assert_eq!(s.awakeables.get("tok-a").unwrap().map(|r| r.state), Some(AwakeableState::Pending));
}

fn a_failed_nested_update_rolls_back_alone() {
    let mut dirs = Vec::new();
    let s = fresh(&mut dirs);
    let path = s.path().to_path_buf();
    s.awakeables.insert(&awakeable("tok-a")).unwrap();
    s.awakeables.insert(&awakeable("tok-b")).unwrap();

    s.awakeables
        .update("tok-a", &mut |outer| {
            let inner = s.awakeables.update("tok-b", &mut |row| {
                s.journal.record(&key("x-1"), &Stored::place(b"inner"))?;
                row.state = AwakeableState::Resolved;
                Err(Failure::new(FailureTag::Storage, "the inner edit fails after its journal write"))
            });
            assert!(inner.is_err());
            outer.state = AwakeableState::Resolved;
            Ok(true)
        })
        .unwrap();
    assert_eq!(committed(&path, "x-1"), 0, "the inner update's journal write rolls back with it");
    assert_eq!(s.awakeables.get("tok-b").unwrap().map(|r| r.state), Some(AwakeableState::Pending));
    assert_eq!(s.awakeables.get("tok-a").unwrap().map(|r| r.state), Some(AwakeableState::Resolved), "the outer update commits");
}

#[test]
fn concurrent_puts_of_one_hash_across_connections_converge() {
    let mut dirs = Vec::new();
    let first = fresh(&mut dirs);
    let path = first.path().to_path_buf();
    let others: Vec<SqliteRunStores> = (0..4).map(|_| SqliteRunStores::open(&path).unwrap()).collect();
    let value = vec![b'c'; INLINE_CUTOFF_BYTES + 1];
    let sha = sha256_hex(&value);
    let (sha, value) = (&sha, &value);
    std::thread::scope(|s| {
        for stores in std::iter::once(&first).chain(others.iter()) {
            s.spawn(move || stores.blobs.put(sha, value).unwrap());
        }
    });
    let conn = rusqlite::Connection::open(&path).unwrap();
    let held: Vec<Vec<u8>> = conn.prepare("SELECT bytes FROM journal_blob").unwrap().query_map([], |r| r.get(0)).unwrap().map(Result::unwrap).collect();
    assert_eq!(held, [value.clone()], "every writer converges on one whole row");
}
