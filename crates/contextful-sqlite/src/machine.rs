//! `machine.sqlite` behind the `Catalog` port: lease rows, each table's cursor row and
//! pending owner, and run rows. Every write runs in an immediate transaction, so SQLite's
//! one write lock serializes it against every other connection to the file and each
//! conditional update is linearizable on the machine (`topology.coordinate.backends`).

use crate::{open, storage};
use contextful_core::coordinate::{Cas, Catalog, CursorRow, Lease, LeaseKey, LeaseRow};
use contextful_core::ports::Clock;
use contextful_core::run::own::ExecutionOwner;
use contextful_core::run::record::RunRow;
use contextful_core::run::{Failure, RunError};
use contextful_core::store::StoreError;
use contextful_core::time::Instant;
use rusqlite::{params, Connection, OptionalExtension, Transaction, TransactionBehavior};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS lease (
    key        TEXT PRIMARY KEY,
    holder     TEXT,
    expires_at TEXT,
    fence      INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS scope (
    pipeline_id TEXT NOT NULL,
    tbl         TEXT NOT NULL,
    version     INTEGER NOT NULL,
    cursor      TEXT NOT NULL,
    owner       TEXT,
    PRIMARY KEY (pipeline_id, tbl)
);
CREATE TABLE IF NOT EXISTS run (
    run_id      TEXT PRIMARY KEY,
    pipeline_id TEXT NOT NULL,
    row         TEXT NOT NULL
);
";

/// The machine-local catalog in one SQLite file.
pub struct MachineCatalog {
    path: PathBuf,
    conn: Mutex<Connection>,
    clock: Arc<dyn Clock + Send + Sync>,
}

impl MachineCatalog {
    /// Open or create the catalog at `path`, reading `clock` as its own
    /// (`topology.coordinate.catalog-clock`).
    pub fn open(path: &Path, clock: Arc<dyn Clock + Send + Sync>) -> Result<MachineCatalog, Failure> {
        let conn = open(path, SCHEMA)?;
        Ok(MachineCatalog { path: path.to_path_buf(), conn: Mutex::new(conn), clock })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    fn fail(&self, e: impl std::fmt::Display) -> Failure {
        storage(&self.path, e)
    }

    /// Run `f` in one transaction; `write` takes the database's write lock at the start,
    /// so the read and the conditional update inside `f` see no interleaved writer.
    fn with<T>(&self, write: bool, f: impl FnOnce(&Transaction, &dyn Fn(&dyn std::fmt::Display) -> Failure) -> Result<T, Failure>) -> Result<T, Failure> {
        let mut conn = self.conn.lock().unwrap_or_else(|p| p.into_inner());
        let behavior = if write { TransactionBehavior::Immediate } else { TransactionBehavior::Deferred };
        let tx = conn.transaction_with_behavior(behavior).map_err(|e| self.fail(e))?;
        let out = f(&tx, &|e| self.fail(e))?;
        tx.commit().map_err(|e| self.fail(e))?;
        Ok(out)
    }
}

type Fail<'a> = &'a dyn Fn(&dyn std::fmt::Display) -> Failure;

fn to_text<T: serde::Serialize>(v: &T, fail: Fail) -> Result<String, Failure> {
    serde_json::to_string(v).map_err(|e| fail(&e))
}

fn from_text<T: serde::de::DeserializeOwned>(s: &str, fail: Fail) -> Result<T, Failure> {
    serde_json::from_str(s).map_err(|e| fail(&e))
}

fn lease_row(tx: &Transaction, key: &str, fail: Fail) -> Result<LeaseRow, Failure> {
    let row = tx
        .query_row("SELECT holder, expires_at, fence FROM lease WHERE key = ?1", params![key], |r| {
            Ok((r.get::<_, Option<String>>(0)?, r.get::<_, Option<String>>(1)?, r.get::<_, i64>(2)?))
        })
        .optional()
        .map_err(|e| fail(&e))?;
    let Some((holder, expires_at, fence)) = row else { return Ok(LeaseRow::default()) };
    let expires_at = expires_at.map(|s| from_text::<Instant>(&s, fail)).transpose()?;
    Ok(LeaseRow { holder, expires_at, fence: fence as u64 })
}

fn put_lease_row(tx: &Transaction, key: &str, row: &LeaseRow, fail: Fail) -> Result<(), Failure> {
    let expires_at = row.expires_at.map(|e| to_text(&e, fail)).transpose()?;
    tx.execute(
        "INSERT INTO lease (key, holder, expires_at, fence) VALUES (?1, ?2, ?3, ?4)
         ON CONFLICT (key) DO UPDATE SET holder = excluded.holder, expires_at = excluded.expires_at, fence = excluded.fence",
        params![key, row.holder, expires_at, row.fence as i64],
    )
    .map_err(|e| fail(&e))?;
    Ok(())
}

/// A table's cursor row, its version read from the column a compare-and-swap predicates
/// on, and its pending owner.
fn scope(tx: &Transaction, pipeline_id: &str, table: &str, fail: Fail) -> Result<(CursorRow, Option<ExecutionOwner>), Failure> {
    let row = tx
        .query_row("SELECT version, cursor, owner FROM scope WHERE pipeline_id = ?1 AND tbl = ?2", params![pipeline_id, table], |r| {
            Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?, r.get::<_, Option<String>>(2)?))
        })
        .optional()
        .map_err(|e| fail(&e))?;
    let Some((version, cursor, owner)) = row else { return Ok((CursorRow::default(), None)) };
    let cursor = CursorRow { version: version as u64, ..from_text(&cursor, fail)? };
    let owner = owner.map(|o| from_text(&o, fail)).transpose()?;
    Ok((cursor, owner))
}

/// Insert the zero-version row a first compare-and-swap predicates on.
fn seed_scope(tx: &Transaction, pipeline_id: &str, table: &str, fail: Fail) -> Result<(), Failure> {
    let empty = to_text(&CursorRow::default(), fail)?;
    tx.execute("INSERT OR IGNORE INTO scope (pipeline_id, tbl, version, cursor) VALUES (?1, ?2, 0, ?3)", params![pipeline_id, table, empty])
        .map_err(|e| fail(&e))?;
    Ok(())
}

/// One conditional update: the new cursor lands only while the stored version still
/// equals `expected`, and the affected-row count says whether it did
/// (`topology.coordinate.cursor-cas`).
fn swap_cursor(tx: &Transaction, pipeline_id: &str, table: &str, expected: u64, next: CursorRow, fail: Fail) -> Result<Cas, Failure> {
    seed_scope(tx, pipeline_id, table, fail)?;
    let stored = to_text(&CursorRow { version: expected + 1, ..next }, fail)?;
    let n = tx
        .execute(
            "UPDATE scope SET cursor = ?3, version = version + 1 WHERE pipeline_id = ?1 AND tbl = ?2 AND version = ?4",
            params![pipeline_id, table, stored, expected as i64],
        )
        .map_err(|e| fail(&e))?;
    Ok(if n == 1 { Cas::Applied } else { Cas::VersionMoved })
}

/// The refusal a commit carrying `fence` meets once a later acquisition moved the lease
/// past it; `None` when the fence is current or the commit carries none.
fn fenced(tx: &Transaction, pipeline_id: &str, table: &str, fence: Option<&Lease>, fail: Fail) -> Result<Option<Cas>, Failure> {
    let Some(lease) = fence else { return Ok(None) };
    let row = lease_row(tx, &lease.key, fail)?;
    Ok((!row.admits(lease.fence)).then(|| {
        Cas::Fenced(StoreError::LeaseFenced(format!(
            "cursor commit for `{pipeline_id}`/`{table}` carries fence {} and lease `{}` is at fence {}",
            lease.fence, lease.key, row.fence
        )))
    }))
}

fn run_row(tx: &Transaction, run_id: &str, fail: Fail) -> Result<Option<RunRow>, Failure> {
    let text = tx
        .query_row("SELECT row FROM run WHERE run_id = ?1", params![run_id], |r| r.get::<_, String>(0))
        .optional()
        .map_err(|e| fail(&e))?;
    text.map(|t| from_text(&t, fail)).transpose()
}

fn put_run_row(tx: &Transaction, row: &RunRow, fail: Fail) -> Result<(), Failure> {
    let text = to_text(row, fail)?;
    tx.execute(
        "INSERT INTO run (run_id, pipeline_id, row) VALUES (?1, ?2, ?3)
         ON CONFLICT (run_id) DO UPDATE SET pipeline_id = excluded.pipeline_id, row = excluded.row",
        params![row.run_id, row.pipeline_id, text],
    )
    .map_err(|e| fail(&e))?;
    Ok(())
}

impl Catalog for MachineCatalog {
    fn now(&self) -> Result<Instant, Failure> {
        Ok(self.clock.now())
    }

    fn acquire(&self, key: &LeaseKey, holder: &str, ttl_secs: u64) -> Result<Option<Lease>, Failure> {
        let spelling = key.spelling();
        let now = self.clock.now();
        self.with(true, |tx, fail| {
            let mut row = lease_row(tx, &spelling, fail)?;
            let lease = row.acquire(&spelling, holder, now, ttl_secs);
            if lease.is_some() {
                put_lease_row(tx, &spelling, &row, fail)?;
            }
            Ok(lease)
        })
    }

    fn release(&self, lease: &Lease) -> Result<(), Failure> {
        self.with(true, |tx, fail| {
            let mut row = lease_row(tx, &lease.key, fail)?;
            row.release(lease);
            put_lease_row(tx, &lease.key, &row, fail)
        })
    }

    fn renew(&self, lease: &Lease, ttl_secs: u64) -> Result<Option<Lease>, Failure> {
        let now = self.clock.now();
        self.with(true, |tx, fail| {
            let mut row = lease_row(tx, &lease.key, fail)?;
            let renewed = row.renew(lease, now, ttl_secs);
            if renewed.is_some() {
                put_lease_row(tx, &lease.key, &row, fail)?;
            }
            Ok(renewed)
        })
    }

    fn lease_holds(&self, lease: &Lease) -> Result<bool, Failure> {
        let now = self.clock.now();
        self.with(false, |tx, fail| Ok(lease_row(tx, &lease.key, fail)?.held_by(lease, now)))
    }

    fn lease_row(&self, key: &LeaseKey) -> Result<LeaseRow, Failure> {
        self.with(false, |tx, fail| lease_row(tx, &key.spelling(), fail))
    }

    fn cursor(&self, pipeline_id: &str, table: &str) -> Result<CursorRow, Failure> {
        self.with(false, |tx, fail| Ok(scope(tx, pipeline_id, table, fail)?.0))
    }

    fn cursor_cas(&self, pipeline_id: &str, table: &str, expected_version: u64, next: CursorRow, fence: Option<&Lease>) -> Result<Cas, Failure> {
        self.with(true, |tx, fail| {
            if scope(tx, pipeline_id, table, fail)?.0.version != expected_version {
                return Ok(Cas::VersionMoved);
            }
            if let Some(refused) = fenced(tx, pipeline_id, table, fence, fail)? {
                return Ok(refused);
            }
            swap_cursor(tx, pipeline_id, table, expected_version, next, fail)
        })
    }

    fn owner(&self, pipeline_id: &str, table: &str) -> Result<Option<ExecutionOwner>, Failure> {
        self.with(false, |tx, fail| Ok(scope(tx, pipeline_id, table, fail)?.1))
    }

    fn put_owner(&self, owner: &ExecutionOwner) -> Result<(), Failure> {
        self.with(true, |tx, fail| {
            seed_scope(tx, &owner.pipeline_id, &owner.table, fail)?;
            let text = to_text(owner, fail)?;
            tx.execute("UPDATE scope SET owner = ?3 WHERE pipeline_id = ?1 AND tbl = ?2", params![owner.pipeline_id, owner.table, text])
                .map_err(|e| fail(&e))?;
            Ok(())
        })
    }

    fn retire(&self, pipeline_id: &str, table: &str, execution_id: &str, cursor: Option<(CursorRow, u64)>, fence: Option<&Lease>) -> Result<Cas, Failure> {
        self.with(true, |tx, fail| {
            if let Some(refused) = fenced(tx, pipeline_id, table, fence, fail)? {
                return Ok(refused);
            }
            if let Some((next, expected)) = cursor {
                // A moved version applies nothing, the owner's retirement included.
                if swap_cursor(tx, pipeline_id, table, expected, next, fail)? == Cas::VersionMoved {
                    return Ok(Cas::VersionMoved);
                }
            }
            if scope(tx, pipeline_id, table, fail)?.1.is_some_and(|o| o.execution_id == execution_id) {
                tx.execute("UPDATE scope SET owner = NULL WHERE pipeline_id = ?1 AND tbl = ?2", params![pipeline_id, table])
                    .map_err(|e| fail(&e))?;
            }
            Ok(Cas::Applied)
        })
    }

    fn put_run(&self, row: &RunRow) -> Result<(), Failure> {
        self.with(true, |tx, fail| put_run_row(tx, row, fail))
    }

    fn run(&self, run_id: &str) -> Result<Option<RunRow>, Failure> {
        self.with(false, |tx, fail| run_row(tx, run_id, fail))
    }

    fn runs(&self, pipeline_id: Option<&str>) -> Result<Vec<RunRow>, Failure> {
        self.with(false, |tx, fail| {
            let mut stmt = tx
                .prepare("SELECT row FROM run WHERE ?1 IS NULL OR pipeline_id = ?1 ORDER BY run_id")
                .map_err(|e| fail(&e))?;
            let texts = stmt
                .query_map(params![pipeline_id], |r| r.get::<_, String>(0))
                .map_err(|e| fail(&e))?
                .collect::<Result<Vec<_>, _>>()
                .map_err(|e| fail(&e))?;
            texts.iter().map(|t| from_text(t, fail)).collect()
        })
    }

    fn update_run(&self, run_id: &str, f: &mut dyn FnMut(&mut RunRow) -> Result<(), RunError>) -> Result<Option<Result<RunRow, RunError>>, Failure> {
        self.with(true, |tx, fail| {
            let Some(mut row) = run_row(tx, run_id, fail)? else { return Ok(None) };
            if let Err(e) = f(&mut row) {
                return Ok(Some(Err(e)));
            }
            put_run_row(tx, &row, fail)?;
            Ok(Some(Ok(row)))
        })
    }
}
