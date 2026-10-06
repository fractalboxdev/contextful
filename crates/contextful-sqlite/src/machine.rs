//! `machine.sqlite` behind the `Catalog` port: lease rows, each scope's cursor row and
//! pending owner, and run rows. Plain catalogs use SQLite's write lock; sealed catalogs
//! lock the snapshot across in-memory transactions. Either lock serializes conditional
//! updates across connections on the machine (`topology.coordinate.backends`).

use crate::{open, sealed::SealedFile, storage};
use contextful_core::coordinate::{Cas, Catalog, CursorRow, Lease, LeaseKey, LeaseRow};
use contextful_core::ports::Clock;
use contextful_core::run::own::{ExecutionOwner, OwnerScope};
use contextful_core::run::record::RunRow;
use contextful_core::run::{Failure, RunError};
use contextful_core::store::StoreError;
use contextful_core::store::encrypt::FileCipher;
use contextful_core::time::Instant;
use rusqlite::{params, Connection, OptionalExtension, Transaction, TransactionBehavior};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

/// One row per owner scope: a table keys on `kind = 'table'` and an empty part, a backfill
/// chunk on `kind = 'chunk'` and its chunk, a host scope on `kind = 'host'` and its id.
const SCOPE_TABLE: &str = "
CREATE TABLE scope (
    pipeline_id TEXT NOT NULL,
    tbl         TEXT NOT NULL,
    kind        TEXT NOT NULL,
    part        TEXT NOT NULL,
    version     INTEGER NOT NULL,
    cursor      TEXT NOT NULL,
    owner       TEXT,
    PRIMARY KEY (pipeline_id, tbl, kind, part)
);
";

const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS lease (
    key        TEXT PRIMARY KEY,
    holder     TEXT,
    expires_at TEXT,
    fence      INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS run (
    run_id      TEXT PRIMARY KEY,
    pipeline_id TEXT NOT NULL,
    row         TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS run_by_pipeline ON run (pipeline_id);
";

/// The machine-local catalog in one SQLite file.
pub struct MachineCatalog {
    path: PathBuf,
    backing: Backing,
    clock: Arc<dyn Clock + Send + Sync>,
}

enum Backing {
    Plain(Mutex<Connection>),
    Sealed(SealedFile),
}

impl MachineCatalog {
    /// Open or create the catalog at `path`, reading `clock` as its own
    /// (`topology.coordinate.catalog-clock`).
    pub fn open(path: &Path, clock: Arc<dyn Clock + Send + Sync>) -> Result<MachineCatalog, Failure> {
        let conn = open(path, SCHEMA)?;
        let catalog = MachineCatalog { path: path.to_path_buf(), backing: Backing::Plain(Mutex::new(conn)), clock };
        catalog.with(true, migrate_scope_keys)?;
        Ok(catalog)
    }

    /// Open an authenticated SQLite snapshot. Every transaction reloads under a
    /// machine-local file lock and writes only sealed bytes to `path`.
    pub fn open_sealed(path: &Path, clock: Arc<dyn Clock + Send + Sync>, cipher: Arc<dyn FileCipher>) -> Result<MachineCatalog, Failure> {
        let catalog = MachineCatalog {
            path: path.to_path_buf(),
            backing: Backing::Sealed(SealedFile::new(path, cipher)),
            clock,
        };
        catalog.with_sealed(true, true, |tx, fail| {
            tx.execute_batch(SCHEMA).map_err(|e| fail(&e))?;
            migrate_scope_keys(tx, fail)
        })?;
        Ok(catalog)
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    fn fail(&self, e: impl std::fmt::Display) -> Failure {
        storage(&self.path, e)
    }

    /// Run `f` in one transaction under the backing's machine-local write lock, so
    /// the read and conditional update see no interleaved writer.
    fn with<T>(&self, write: bool, f: impl FnOnce(&Transaction, &dyn Fn(&dyn std::fmt::Display) -> Failure) -> Result<T, Failure>) -> Result<T, Failure> {
        match &self.backing {
            Backing::Plain(conn) => {
                let mut conn = conn.lock().unwrap_or_else(|p| p.into_inner());
                let behavior = if write { TransactionBehavior::Immediate } else { TransactionBehavior::Deferred };
                let tx = conn.transaction_with_behavior(behavior).map_err(|e| self.fail(e))?;
                let out = f(&tx, &|e| self.fail(e))?;
                tx.commit().map_err(|e| self.fail(e))?;
                Ok(out)
            }
            Backing::Sealed(_) => self.with_sealed(write, false, f),
        }
    }

    fn with_sealed<T>(
        &self,
        write: bool,
        allow_create: bool,
        f: impl FnOnce(&Transaction, &dyn Fn(&dyn std::fmt::Display) -> Failure) -> Result<T, Failure>,
    ) -> Result<T, Failure> {
        let Backing::Sealed(file) = &self.backing else { unreachable!("sealed catalog has sealed backing") };
        let _lock = file.lock()?;
        let mut conn = file.load(allow_create, SCHEMA)?;
        let behavior = if write { TransactionBehavior::Immediate } else { TransactionBehavior::Deferred };
        let tx = conn.transaction_with_behavior(behavior).map_err(|e| self.fail(e))?;
        let out = f(&tx, &|e| self.fail(e))?;
        tx.commit().map_err(|e| self.fail(e))?;
        if write { file.save(&conn)?; }
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

/// Create the scope-keyed `scope` table, or rebuild one keyed on `(pipeline_id, tbl)`
/// alone into it. Every stored row becomes a table scope with its pipeline, table, cursor
/// and owner text unchanged, so a pending table owner resumes as it was.
fn migrate_scope_keys(tx: &Transaction, fail: Fail) -> Result<(), Failure> {
    let columns: Vec<String> = tx
        .prepare("SELECT name FROM pragma_table_info('scope')")
        .and_then(|mut q| q.query_map([], |r| r.get::<_, String>(0))?.collect())
        .map_err(|e| fail(&e))?;
    if columns.iter().any(|c| c == "kind") {
        return Ok(());
    }
    if columns.is_empty() {
        return tx.execute_batch(SCOPE_TABLE).map_err(|e| fail(&e));
    }
    tx.execute_batch(&format!(
        "ALTER TABLE scope RENAME TO scope_by_table;
         {SCOPE_TABLE}
         INSERT INTO scope (pipeline_id, tbl, kind, part, version, cursor, owner)
             SELECT pipeline_id, tbl, 'table', '', version, cursor, owner FROM scope_by_table;
         DROP TABLE scope_by_table;"
    ))
    .map_err(|e| fail(&e))
}

/// The row key of a scope: pipeline, table, kind and part. A table keeps the pipeline and
/// table columns it was always keyed on.
fn key(scope: &OwnerScope) -> [&str; 4] {
    match scope {
        OwnerScope::Table { pipeline_id, table } => [pipeline_id, table, "table", ""],
        OwnerScope::Chunk { pipeline_id, table, chunk } => [pipeline_id, table, "chunk", chunk],
        OwnerScope::Host { host_scope } => ["", "", "host", host_scope],
    }
}

const WHERE_KEY: &str = "pipeline_id = ?1 AND tbl = ?2 AND kind = ?3 AND part = ?4";

/// A scope's cursor row, its version read from the column a compare-and-swap predicates
/// on, and its pending owner.
fn scope(tx: &Transaction, at: &OwnerScope, fail: Fail) -> Result<(CursorRow, Option<ExecutionOwner>), Failure> {
    let [p, t, k, c] = key(at);
    let row = tx
        .query_row(&format!("SELECT version, cursor, owner FROM scope WHERE {WHERE_KEY}"), params![p, t, k, c], |r| {
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
fn seed_scope(tx: &Transaction, at: &OwnerScope, fail: Fail) -> Result<(), Failure> {
    let [p, t, k, c] = key(at);
    let empty = to_text(&CursorRow::default(), fail)?;
    tx.execute("INSERT OR IGNORE INTO scope (pipeline_id, tbl, kind, part, version, cursor) VALUES (?1, ?2, ?3, ?4, 0, ?5)", params![p, t, k, c, empty])
        .map_err(|e| fail(&e))?;
    Ok(())
}

/// One conditional update: the new cursor lands only while the stored version still
/// equals `expected`, and the affected-row count says whether it did
/// (`topology.coordinate.cursor-cas`).
fn swap_cursor(tx: &Transaction, at: &OwnerScope, expected: u64, next: CursorRow, fail: Fail) -> Result<Cas, Failure> {
    seed_scope(tx, at, fail)?;
    let [p, t, k, c] = key(at);
    let stored = to_text(&CursorRow { version: expected + 1, ..next }, fail)?;
    let n = tx
        .execute(&format!("UPDATE scope SET cursor = ?5, version = version + 1 WHERE {WHERE_KEY} AND version = ?6"), params![p, t, k, c, stored, expected as i64])
        .map_err(|e| fail(&e))?;
    Ok(if n == 1 { Cas::Applied } else { Cas::VersionMoved })
}

/// The refusal a commit carrying `fence` meets once a later acquisition moved the lease
/// past it; `None` when the fence is current or the commit carries none.
fn fenced(tx: &Transaction, scope: &OwnerScope, fence: Option<&Lease>, fail: Fail) -> Result<Option<Cas>, Failure> {
    let Some(lease) = fence else { return Ok(None) };
    let row = lease_row(tx, &lease.key, fail)?;
    let at = match scope.pipeline_table() {
        Some((p, t)) => format!("`{p}`/`{t}`"),
        None => scope.to_string(),
    };
    Ok((!row.admits(lease.fence)).then(|| {
        Cas::Fenced(StoreError::LeaseFenced(format!("cursor commit for {at} carries fence {} and lease `{}` is at fence {}", lease.fence, lease.key, row.fence)))
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

    fn cursor_at(&self, at: &OwnerScope) -> Result<CursorRow, Failure> {
        self.with(false, |tx, fail| Ok(scope(tx, at, fail)?.0))
    }

    fn cursor_cas_at(&self, at: &OwnerScope, expected_version: u64, next: CursorRow, fence: Option<&Lease>) -> Result<Cas, Failure> {
        self.with(true, |tx, fail| {
            if scope(tx, at, fail)?.0.version != expected_version {
                return Ok(Cas::VersionMoved);
            }
            if let Some(refused) = fenced(tx, at, fence, fail)? {
                return Ok(refused);
            }
            swap_cursor(tx, at, expected_version, next, fail)
        })
    }

    fn owner_at(&self, at: &OwnerScope) -> Result<Option<ExecutionOwner>, Failure> {
        self.with(false, |tx, fail| Ok(scope(tx, at, fail)?.1))
    }

    fn put_owner(&self, owner: &ExecutionOwner) -> Result<(), Failure> {
        self.with(true, |tx, fail| {
            seed_scope(tx, &owner.scope, fail)?;
            let [p, t, k, c] = key(&owner.scope);
            let text = to_text(owner, fail)?;
            tx.execute(&format!("UPDATE scope SET owner = ?5 WHERE {WHERE_KEY}"), params![p, t, k, c, text]).map_err(|e| fail(&e))?;
            Ok(())
        })
    }

    fn retire_at(&self, at: &OwnerScope, execution_id: &str, cursor: Option<(CursorRow, u64)>, fence: Option<&Lease>) -> Result<Cas, Failure> {
        self.with(true, |tx, fail| {
            if let Some(refused) = fenced(tx, at, fence, fail)? {
                return Ok(refused);
            }
            if let Some((next, expected)) = cursor {
                // A moved version applies nothing, the owner's retirement included.
                if swap_cursor(tx, at, expected, next, fail)? == Cas::VersionMoved {
                    return Ok(Cas::VersionMoved);
                }
            }
            if scope(tx, at, fail)?.1.is_some_and(|o| o.execution_id == execution_id) {
                let [p, t, k, c] = key(at);
                tx.execute(&format!("UPDATE scope SET owner = NULL WHERE {WHERE_KEY}"), params![p, t, k, c]).map_err(|e| fail(&e))?;
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
            // One statement per shape: `?1 IS NULL OR pipeline_id = ?1` rules out the index.
            let sql = match pipeline_id {
                Some(_) => "SELECT row FROM run WHERE pipeline_id = ?1 ORDER BY run_id",
                None => "SELECT row FROM run ORDER BY run_id",
            };
            let mut stmt = tx.prepare(sql).map_err(|e| fail(&e))?;
            let text = |r: &rusqlite::Row| r.get::<_, String>(0);
            let texts = match pipeline_id {
                Some(p) => stmt.query_map(params![p], text),
                None => stmt.query_map([], text),
            }
            .map_err(|e| fail(&e))?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| fail(&e))?;
            texts.iter().map(|t| from_text(t, fail)).collect()
        })
    }

    fn last_run_start(&self, pipeline_id: &str) -> Result<Option<Instant>, Failure> {
        self.with(false, |tx, fail| {
            // Starts compare as instants, not as text: RFC 3339 spells a whole second shorter
            // than a fractional one, so a string maximum misorders the two.
            let mut stmt = tx.prepare("SELECT json_extract(row, '$.started_at') FROM run WHERE pipeline_id = ?1").map_err(|e| fail(&e))?;
            let starts = stmt
                .query_map(params![pipeline_id], |r| r.get::<_, String>(0))
                .map_err(|e| fail(&e))?
                .collect::<Result<Vec<_>, _>>()
                .map_err(|e| fail(&e))?;
            let mut newest: Option<Instant> = None;
            for s in starts {
                let at = Instant::parse(&s).map_err(|e| fail(&e))?;
                newest = Some(newest.map_or(at, |n| n.max(at)));
            }
            Ok(newest)
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
