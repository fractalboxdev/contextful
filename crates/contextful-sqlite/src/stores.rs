//! The run path's journal, blob and awakeable stores in one SQLite file
//! (`run.journal.sqlite-stores`), by default the store root's `machine.sqlite` beside the
//! catalog's tables.
//!
//! ```text
//! journal        one row per entry key: a pending claim's holder, or the recorded value
//!                inline as bytes or as a blob reference
//! journal_blob   one row per value above the inline cutoff, keyed by its sha256
//! journal_sweep  the instant the last sweep pass ran
//! awakeable      one registry row per token
//! ```
//!
//! The file runs in write-ahead-log mode, so a reader never waits on a writer. Every write
//! runs in an immediate transaction, which takes SQLite's one write lock up front and so
//! serializes it against every other connection to the file, in this process or another.
//! The three stores of one [`SqliteRunStores`] share one connection: an awakeable update
//! records its resume payload through the journal from inside its own transaction, and a
//! second connection would wait on the write lock that transaction holds.

use crate::storage;
use contextful_core::run::journal::{sweepable, EntryKey, Row, Stored};
use contextful_core::run::ports::{AwakeableStore, BlobStore, JournalStore};
use contextful_core::run::suspend::Awakeable;
use contextful_core::run::Failure;
use parking_lot::ReentrantMutex;
use rusqlite::{params, Connection, OptionalExtension};
use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::sync::Arc;

const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS journal (
    execution_id TEXT NOT NULL,
    step_label   TEXT NOT NULL,
    input_hash   TEXT NOT NULL,
    holder       TEXT,
    inline_value BLOB,
    blob_sha256  TEXT,
    blob_bytes   INTEGER,
    PRIMARY KEY (execution_id, step_label, input_hash),
    CHECK ((holder IS NOT NULL AND inline_value IS NULL AND blob_sha256 IS NULL)
        OR (holder IS NULL AND (inline_value IS NULL) <> (blob_sha256 IS NULL)))
) WITHOUT ROWID;
CREATE TABLE IF NOT EXISTS journal_blob (
    sha256 TEXT PRIMARY KEY,
    bytes  BLOB NOT NULL,
    put_at INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS journal_sweep (
    id       INTEGER PRIMARY KEY CHECK (id = 0),
    swept_at INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS awakeable (
    token        TEXT PRIMARY KEY,
    execution_id TEXT NOT NULL,
    row          TEXT NOT NULL
);
";

fn now_unix() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0)
}

/// The connection and the depth of the transactions open on it.
struct Held {
    conn: Connection,
    depth: u32,
}

/// One connection to the file. A call from the thread already inside a transaction joins
/// it as a savepoint; every other thread waits for the outermost transaction to end.
struct Db {
    path: PathBuf,
    held: ReentrantMutex<RefCell<Held>>,
}

/// A transaction or savepoint open on a [`Db`], rolled back unless committed, so a panic
/// or an early return leaves the connection outside it.
struct Open<'a> {
    db: &'a Db,
    held: &'a RefCell<Held>,
    depth: u32,
    done: bool,
}

impl Open<'_> {
    /// Run one statement batch on the connection, borrowing it only for the call.
    fn run<T>(&self, f: impl FnOnce(&Connection) -> rusqlite::Result<T>) -> Result<T, Failure> {
        f(&self.held.borrow().conn).map_err(|e| self.db.fail(e))
    }

    fn finish(mut self) -> Result<(), Failure> {
        self.done = true;
        let end = if self.depth == 0 { "COMMIT".to_string() } else { format!("RELEASE nest{}", self.depth) };
        let out = self.run(|c| c.execute_batch(&end));
        if out.is_err() {
            self.abandon();
        }
        self.held.borrow_mut().depth = self.depth;
        out
    }

    fn abandon(&self) {
        let undo = if self.depth == 0 { "ROLLBACK".to_string() } else { format!("ROLLBACK TO nest{d}; RELEASE nest{d}", d = self.depth) };
        // A failed rollback leaves SQLite to roll the transaction back when the
        // connection closes; the caller already holds the error that got here.
        let _ = self.held.borrow().conn.execute_batch(&undo);
    }
}

impl Drop for Open<'_> {
    fn drop(&mut self) {
        if !self.done {
            self.abandon();
            self.held.borrow_mut().depth = self.depth;
        }
    }
}

impl Db {
    fn open(path: &Path) -> Result<Db, Failure> {
        let conn = crate::connect(path)?;
        let mode: String = conn.query_row("PRAGMA journal_mode = WAL", [], |r| r.get(0)).map_err(|e| storage(path, e))?;
        if !mode.eq_ignore_ascii_case("wal") {
            return Err(storage(path, format!("the file refuses write-ahead-log mode and stays in `{mode}`")));
        }
        conn.execute_batch(SCHEMA).map_err(|e| storage(path, e))?;
        Ok(Db { path: path.to_path_buf(), held: ReentrantMutex::new(RefCell::new(Held { conn, depth: 0 })) })
    }

    fn fail(&self, e: impl std::fmt::Display) -> Failure {
        storage(&self.path, e)
    }

    /// Run `f` in one transaction, immediate when `write`, or in a savepoint when this
    /// thread is already inside one.
    fn with<T>(&self, write: bool, f: impl FnOnce(&Open) -> Result<T, Failure>) -> Result<T, Failure> {
        let guard = self.held.lock();
        let depth = guard.borrow().depth;
        let begin = match (depth, write) {
            (0, true) => "BEGIN IMMEDIATE".to_string(),
            (0, false) => "BEGIN DEFERRED".to_string(),
            (d, _) => format!("SAVEPOINT nest{d}"),
        };
        guard.borrow().conn.execute_batch(&begin).map_err(|e| self.fail(e))?;
        guard.borrow_mut().depth = depth + 1;
        let open = Open { db: self, held: &guard, depth, done: false };
        let out = f(&open)?;
        open.finish()?;
        Ok(out)
    }
}

/// The journal, blob and awakeable stores over one SQLite file and one connection.
#[derive(Clone)]
pub struct SqliteRunStores {
    pub journal: SqliteJournalStore,
    pub blobs: SqliteBlobStore,
    pub awakeables: SqliteAwakeableStore,
}

impl SqliteRunStores {
    /// Open or create the stores at `path`, switching the file to write-ahead-log mode.
    pub fn open(path: &Path) -> Result<SqliteRunStores, Failure> {
        let db = Arc::new(Db::open(path)?);
        Ok(SqliteRunStores {
            journal: SqliteJournalStore { db: db.clone() },
            blobs: SqliteBlobStore { db: db.clone() },
            awakeables: SqliteAwakeableStore { db },
        })
    }

    /// The file the stores live in.
    pub fn path(&self) -> &Path {
        &self.journal.db.path
    }
}

/// Journal rows in the `journal` table.
#[derive(Clone)]
pub struct SqliteJournalStore {
    db: Arc<Db>,
}

/// Inline bytes as the value they hold: UTF-8 text when they are one, else hex, as
/// [`Stored::place`] writes them.
fn inline(bytes: Vec<u8>) -> Stored {
    match String::from_utf8(bytes) {
        Ok(text) => Stored::Inline { text: Some(text), hex: None },
        Err(e) => Stored::Inline { text: None, hex: Some(e.into_bytes().iter().map(|b| format!("{b:02x}")).collect()) },
    }
}

type Columns = (String, String, String, Option<String>, Option<Vec<u8>>, Option<String>, Option<i64>);

const COLUMNS: &str = "execution_id, step_label, input_hash, holder, inline_value, blob_sha256, blob_bytes";

fn columns(r: &rusqlite::Row) -> rusqlite::Result<Columns> {
    Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?, r.get(6)?))
}

fn row(db: &Db, (execution_id, step_label, input_hash, holder, inline_value, blob_sha256, blob_bytes): Columns) -> Result<Row, Failure> {
    let key = EntryKey { execution_id, step_label, input_hash };
    match (holder, inline_value, blob_sha256) {
        (Some(run_id), _, _) => Ok(Row::Pending { key, run_id }),
        (None, _, Some(sha256)) => Ok(Row::Recorded { key, value: Stored::Blob { sha256, bytes: blob_bytes.unwrap_or_default() as u64 } }),
        (None, Some(bytes), None) => Ok(Row::Recorded { key, value: inline(bytes) }),
        (None, None, None) => Err(db.fail(format!("journal row for step `{}` holds neither a claim nor a value", key.step_label))),
    }
}

impl SqliteJournalStore {
    fn read_in(&self, tx: &Open, key: &EntryKey) -> Result<Option<Row>, Failure> {
        let found = tx.run(|c| {
            c.query_row(
                &format!("SELECT {COLUMNS} FROM journal WHERE execution_id = ?1 AND step_label = ?2 AND input_hash = ?3"),
                params![key.execution_id, key.step_label, key.input_hash],
                columns,
            )
            .optional()
        })?;
        found.map(|c| row(&self.db, c)).transpose()
    }
}

impl JournalStore for SqliteJournalStore {
    fn create_pending(&self, key: &EntryKey, run_id: &str) -> Result<bool, Failure> {
        self.db.with(true, |tx| {
            let n = tx.run(|c| {
                c.execute(
                    "INSERT INTO journal (execution_id, step_label, input_hash, holder) VALUES (?1, ?2, ?3, ?4)
                     ON CONFLICT (execution_id, step_label, input_hash) DO NOTHING",
                    params![key.execution_id, key.step_label, key.input_hash, run_id],
                )
            })?;
            Ok(n == 1)
        })
    }

    fn read(&self, key: &EntryKey) -> Result<Option<Row>, Failure> {
        self.db.with(false, |tx| self.read_in(tx, key))
    }

    fn replace_if_pending(&self, key: &EntryKey, holder: &str, run_id: &str) -> Result<bool, Failure> {
        self.db.with(true, |tx| {
            let n = tx.run(|c| {
                c.execute(
                    "UPDATE journal SET holder = ?5 WHERE execution_id = ?1 AND step_label = ?2 AND input_hash = ?3 AND holder = ?4",
                    params![key.execution_id, key.step_label, key.input_hash, holder, run_id],
                )
            })?;
            Ok(n == 1)
        })
    }

    fn record(&self, key: &EntryKey, value: &Stored) -> Result<Option<Stored>, Failure> {
        self.db.with(true, |tx| {
            if let Some(Row::Recorded { value: standing, .. }) = self.read_in(tx, key)? {
                return Ok(Some(standing));
            }
            let (inline_value, blob_sha256, blob_bytes) = match value {
                Stored::Blob { sha256, bytes } => (None, Some(sha256.as_str()), Some(*bytes as i64)),
                Stored::Inline { .. } => (value.inline_bytes(), None, None),
            };
            tx.run(|c| {
                c.execute(
                    "INSERT INTO journal (execution_id, step_label, input_hash, holder, inline_value, blob_sha256, blob_bytes)
                     VALUES (?1, ?2, ?3, NULL, ?4, ?5, ?6)
                     ON CONFLICT (execution_id, step_label, input_hash) DO UPDATE SET holder = NULL, inline_value = excluded.inline_value,
                        blob_sha256 = excluded.blob_sha256, blob_bytes = excluded.blob_bytes",
                    params![key.execution_id, key.step_label, key.input_hash, inline_value, blob_sha256, blob_bytes],
                )
            })?;
            Ok(None)
        })
    }

    fn release(&self, key: &EntryKey, run_id: &str) -> Result<(), Failure> {
        self.db.with(true, |tx| {
            tx.run(|c| {
                c.execute(
                    "DELETE FROM journal WHERE execution_id = ?1 AND step_label = ?2 AND input_hash = ?3 AND holder = ?4",
                    params![key.execution_id, key.step_label, key.input_hash, run_id],
                )
            })?;
            Ok(())
        })
    }

    fn rows(&self, execution_id: &str) -> Result<Vec<Row>, Failure> {
        self.db.with(false, |tx| {
            let found = tx.run(|c| {
                let mut stmt = c.prepare(&format!("SELECT {COLUMNS} FROM journal WHERE execution_id = ?1"))?;
                let rows = stmt.query_map(params![execution_id], columns)?.collect::<rusqlite::Result<Vec<_>>>();
                rows
            })?;
            found.into_iter().map(|c| row(&self.db, c)).collect()
        })
    }

    fn retire(&self, execution_id: &str) -> Result<(), Failure> {
        self.db.with(true, |tx| {
            tx.run(|c| c.execute("DELETE FROM journal WHERE execution_id = ?1", params![execution_id]))?;
            Ok(())
        })
    }

    fn executions(&self) -> Result<Vec<String>, Failure> {
        self.db.with(false, |tx| {
            tx.run(|c| {
                let mut stmt = c.prepare("SELECT DISTINCT execution_id FROM journal ORDER BY execution_id")?;
                let ids = stmt.query_map([], |r| r.get(0))?.collect::<rusqlite::Result<Vec<String>>>();
                ids
            })
        })
    }
}

/// Values above the inline cutoff in the `journal_blob` table, one row per sha256.
#[derive(Clone)]
pub struct SqliteBlobStore {
    db: Arc<Db>,
}

impl BlobStore for SqliteBlobStore {
    fn put(&self, sha256: &str, bytes: &[u8]) -> Result<(), Failure> {
        // A second put of one hash holds the same bytes, so it renews the age alone.
        self.db.with(true, |tx| {
            tx.run(|c| {
                c.execute(
                    "INSERT INTO journal_blob (sha256, bytes, put_at) VALUES (?1, ?2, ?3)
                     ON CONFLICT (sha256) DO UPDATE SET put_at = excluded.put_at",
                    params![sha256, bytes, now_unix()],
                )
            })?;
            Ok(())
        })
    }

    fn get(&self, sha256: &str) -> Result<Option<Vec<u8>>, Failure> {
        self.db.with(false, |tx| tx.run(|c| c.query_row("SELECT bytes FROM journal_blob WHERE sha256 = ?1", params![sha256], |r| r.get(0)).optional()))
    }

    fn sweep(&self, referenced: &[String], now_unix: i64) -> Result<Vec<String>, Failure> {
        self.db.with(true, |tx| {
            let held = tx.run(|c| {
                let mut stmt = c.prepare("SELECT sha256, put_at FROM journal_blob")?;
                let held = stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)))?.collect::<rusqlite::Result<Vec<_>>>();
                held
            })?;
            let doomed: Vec<String> = held
                .into_iter()
                .filter(|(sha, put_at)| sweepable(referenced.contains(sha), u64::try_from(now_unix - put_at).unwrap_or(0)))
                .map(|(sha, _)| sha)
                .collect();
            for sha in &doomed {
                tx.run(|c| c.execute("DELETE FROM journal_blob WHERE sha256 = ?1", params![sha]))?;
            }
            Ok(doomed)
        })
    }

    fn swept_at(&self) -> Result<Option<i64>, Failure> {
        self.db.with(false, |tx| tx.run(|c| c.query_row("SELECT swept_at FROM journal_sweep WHERE id = 0", [], |r| r.get(0)).optional()))
    }

    fn mark_swept(&self, at: i64) -> Result<(), Failure> {
        self.db.with(true, |tx| {
            tx.run(|c| c.execute("INSERT INTO journal_sweep (id, swept_at) VALUES (0, ?1) ON CONFLICT (id) DO UPDATE SET swept_at = excluded.swept_at", params![at]))?;
            Ok(())
        })
    }
}

/// Awakeable registry rows in the `awakeable` table, each row as JSON under its token.
#[derive(Clone)]
pub struct SqliteAwakeableStore {
    db: Arc<Db>,
}

impl SqliteAwakeableStore {
    fn get_in(&self, tx: &Open, token: &str) -> Result<Option<Awakeable>, Failure> {
        let text: Option<String> = tx.run(|c| c.query_row("SELECT row FROM awakeable WHERE token = ?1", params![token], |r| r.get(0)).optional())?;
        text.map(|t| serde_json::from_str(&t).map_err(|e| self.db.fail(e))).transpose()
    }

    fn put_in(&self, tx: &Open, row: &Awakeable) -> Result<(), Failure> {
        let text = serde_json::to_string(row).map_err(|e| self.db.fail(e))?;
        tx.run(|c| {
            c.execute(
                "INSERT INTO awakeable (token, execution_id, row) VALUES (?1, ?2, ?3)
                 ON CONFLICT (token) DO UPDATE SET execution_id = excluded.execution_id, row = excluded.row",
                params![row.token, row.execution_id, text],
            )
        })?;
        Ok(())
    }
}

impl AwakeableStore for SqliteAwakeableStore {
    fn insert(&self, row: &Awakeable) -> Result<(), Failure> {
        self.db.with(true, |tx| self.put_in(tx, row))
    }

    fn get(&self, token: &str) -> Result<Option<Awakeable>, Failure> {
        self.db.with(false, |tx| self.get_in(tx, token))
    }

    fn update(&self, token: &str, edit: &mut dyn FnMut(&mut Awakeable) -> Result<bool, Failure>) -> Result<Option<Awakeable>, Failure> {
        // The edit runs inside the write transaction, so a journal write it makes through
        // this connection commits or rolls back with the row.
        self.db.with(true, |tx| {
            let Some(current) = self.get_in(tx, token)? else { return Ok(None) };
            let mut row = current.clone();
            if edit(&mut row)? {
                self.put_in(tx, &row)?;
                return Ok(Some(row));
            }
            Ok(Some(current))
        })
    }

    fn rows(&self) -> Result<Vec<Awakeable>, Failure> {
        self.db.with(false, |tx| {
            let texts = tx.run(|c| {
                let mut stmt = c.prepare("SELECT row FROM awakeable ORDER BY token")?;
                let texts = stmt.query_map([], |r| r.get::<_, String>(0))?.collect::<rusqlite::Result<Vec<_>>>();
                texts
            })?;
            texts.iter().map(|t| serde_json::from_str(t).map_err(|e| self.db.fail(e))).collect()
        })
    }
}
