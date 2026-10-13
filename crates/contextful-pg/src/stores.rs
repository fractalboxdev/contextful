//! Remote execution stores share the catalog database and survive disposable disks.

use crate::{from_text, to_text, Fail, PgCatalog};
use contextful_core::run::journal::{awakeable_key, sha256_hex, sweepable, EntryKey, Row, Stored};
use contextful_core::run::ports::{AwakeableStore, BlobStore, JournalStore};
use contextful_core::run::suspend::{Awakeable, AwakeableState};
use contextful_core::run::Failure;
use postgres::Transaction;
use serde::{de::DeserializeOwned, Serialize};
use std::sync::Arc;

const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS contextful_execution_schema (id BOOLEAN PRIMARY KEY CHECK (id), version BIGINT NOT NULL);
INSERT INTO contextful_execution_schema (id, version) VALUES (TRUE, 1) ON CONFLICT DO NOTHING;
CREATE TABLE IF NOT EXISTS contextful_journal (
    execution_id TEXT NOT NULL, step_label TEXT NOT NULL, input_hash TEXT NOT NULL, body TEXT NOT NULL,
    PRIMARY KEY (execution_id, step_label, input_hash)
);
CREATE TABLE IF NOT EXISTS contextful_blob (sha256 TEXT PRIMARY KEY, bytes BYTEA NOT NULL, put_at BIGINT NOT NULL);
CREATE TABLE IF NOT EXISTS contextful_sweep (id BOOLEAN PRIMARY KEY CHECK (id), swept_at BIGINT NOT NULL);
CREATE TABLE IF NOT EXISTS contextful_awakeable (token TEXT PRIMARY KEY, body TEXT NOT NULL);
CREATE TABLE IF NOT EXISTS contextful_recovery (id BOOLEAN PRIMARY KEY CHECK (id), project TEXT NOT NULL, generation TEXT NOT NULL);
";

/// Every execution adapter checks this version before it admits work.
pub const EXECUTION_SCHEMA_VERSION: i64 = 1;

pub(crate) fn schema(db: &PgCatalog) -> Result<(), Failure> {
    db.with(|tx, fail| {
        tx.execute("SELECT pg_advisory_xact_lock($1)", &[&crate::SCHEMA_LOCK]).map_err(|e| fail(&e))?;
        tx.batch_execute(SCHEMA).map_err(|e| fail(&e))?;
        let version: i64 = tx.query_one("SELECT version FROM contextful_execution_schema WHERE id = TRUE", &[]).map_err(|e| fail(&e))?.get(0);
        if version != EXECUTION_SCHEMA_VERSION {
            return Err(fail(&format!("ExecutionSchemaIncompatible: database version {version}, executable version {EXECUTION_SCHEMA_VERSION}")));
        }
        Ok(())
    })
}

/// Journal rows, values and callbacks in the same remote database as owners and cadence.
#[derive(Clone)]
pub struct PgRunStores {
    pub journal: PgJournalStore,
    pub blobs: PgBlobStore,
    pub awakeables: PgAwakeableStore,
}

impl PgRunStores {
    pub fn connect(conninfo: &str) -> Result<Self, Failure> {
        // A callback transaction can record through the journal port; distinct connections
        // prevent a recursive connection lock while its callback row stays exclusive.
        let journal = Arc::new(PgCatalog::connect(conninfo)?);
        schema(&journal)?;
        let blobs = Arc::new(PgCatalog::connect(conninfo)?);
        let awakeables = Arc::new(PgCatalog::connect(conninfo)?);
        Ok(Self {
            journal: PgJournalStore { db: journal },
            blobs: PgBlobStore { db: blobs },
            awakeables: PgAwakeableStore { db: awakeables },
        })
    }

    /// Explicitly provision a fresh execution database; normal opens never create this binding.
    pub fn initialize_recovery(
        conninfo: &str,
        project: &str,
        generation: &str,
    ) -> Result<(), Failure> {
        let existing = PgCatalog::connect_schema(conninfo, false)?;
        let bound = existing.with(|tx, fail| {
            let recovery_table: Option<String> = tx.query_one("SELECT to_regclass('contextful_recovery')::TEXT", &[]).map_err(|e| fail(&e))?.get(0);
            if recovery_table.is_some() && tx.query_opt("SELECT id FROM contextful_recovery WHERE id = TRUE", &[]).map_err(|e| fail(&e))?.is_some() { return Ok(true) }
            for table in ["contextful_lease", "contextful_scope", "contextful_run", "contextful_journal", "contextful_blob", "contextful_awakeable", "contextful_export"] {
                let exists: Option<String> = tx.query_one("SELECT to_regclass($1)::TEXT", &[&table]).map_err(|e| fail(&e))?.get(0);
                if exists.is_some() && tx.query_one(&format!("SELECT EXISTS (SELECT 1 FROM {table})"), &[]).map_err(|e| fail(&e))?.get::<_, bool>(0) {
                    return Err(fail(&"ExecutionRecoveryMismatch: existing execution state has no recovery identity; initialize only a fresh deployment"));
                }
            }
            Ok(false)
        })?;
        if bound {
            return Self::verify_recovery(conninfo, project, generation);
        }
        drop(existing);
        let db = PgCatalog::connect(conninfo)?;
        schema(&db)?;
        db.with(|tx, fail| {
            tx.execute("SELECT pg_advisory_xact_lock($1)", &[&crate::SCHEMA_LOCK]).map_err(|e| fail(&e))?;
            tx.execute("INSERT INTO contextful_recovery (id, project, generation) VALUES (TRUE, $1, $2) ON CONFLICT DO NOTHING", &[&project, &generation]).map_err(|e| fail(&e))?;
            recovery(tx, project, generation, fail)
        })?;
        drop(crate::PgExportLedger::connect(conninfo)?);
        Ok(())
    }

    /// A corpus-only restore into an empty execution database refuses this check.
    pub fn verify_recovery(conninfo: &str, project: &str, generation: &str) -> Result<(), Failure> {
        let db = PgCatalog::connect_schema(conninfo, false)?;
        db.with(|tx, fail| {
            for table in ["contextful_lease", "contextful_scope", "contextful_run", "contextful_execution_schema", "contextful_journal", "contextful_blob", "contextful_awakeable", "contextful_sweep", "contextful_export", "contextful_recovery"] {
                let exists: Option<String> = tx.query_one("SELECT to_regclass($1)::TEXT", &[&table]).map_err(|e| fail(&e))?.get(0);
                if exists.is_none() { return Err(fail(&format!("ExecutionRecoveryMismatch: complete execution backup is required; missing `{table}`"))) }
            }
            let version: i64 = tx.query_one("SELECT version FROM contextful_execution_schema WHERE id = TRUE", &[]).map_err(|e| fail(&e))?.get(0);
            if version != EXECUTION_SCHEMA_VERSION { return Err(fail(&format!("ExecutionSchemaIncompatible: database version {version}, executable version {EXECUTION_SCHEMA_VERSION}"))) }
            recovery(tx, project, generation, fail)
        })
    }
}

fn recovery(
    tx: &mut Transaction,
    project: &str,
    generation: &str,
    fail: Fail,
) -> Result<(), Failure> {
    let row = tx
        .query_opt(
            "SELECT project, generation FROM contextful_recovery WHERE id = TRUE",
            &[],
        )
        .map_err(|e| fail(&e))?;
    if row.is_none_or(|r| r.get::<_, &str>(0) != project || r.get::<_, &str>(1) != generation) {
        return Err(fail(&"ExecutionRecoveryMismatch: the complete execution backup and its project recovery identity are required; a corpus-only restore admits no work"));
    }
    Ok(())
}

#[derive(Clone)]
pub struct PgJournalStore {
    db: Arc<PgCatalog>,
}

fn lock(tx: &mut Transaction, namespace: &str, key: &str, fail: Fail) -> Result<(), Failure> {
    let key = format!("{namespace}:{key}");
    tx.execute(
        "SELECT pg_advisory_xact_lock(hashtextextended($1, 0))",
        &[&key],
    )
    .map_err(|e| fail(&e))?;
    Ok(())
}

fn journal_row(tx: &mut Transaction, key: &EntryKey, fail: Fail) -> Result<Option<Row>, Failure> {
    tx.query_opt("SELECT body FROM contextful_journal WHERE execution_id = $1 AND step_label = $2 AND input_hash = $3", &[&key.execution_id, &key.step_label, &key.input_hash])
        .map_err(|e| fail(&e))?.map(|r| from_text(r.get(0), fail)).transpose()
}

fn put_row(tx: &mut Transaction, key: &EntryKey, row: &Row, fail: Fail) -> Result<(), Failure> {
    let body = to_text(row, fail)?;
    tx.execute("INSERT INTO contextful_journal (execution_id, step_label, input_hash, body) VALUES ($1, $2, $3, $4)
        ON CONFLICT (execution_id, step_label, input_hash) DO UPDATE SET body = excluded.body", &[&key.execution_id, &key.step_label, &key.input_hash, &body]).map_err(|e| fail(&e))?;
    Ok(())
}

impl PgJournalStore {
    fn edit<T>(
        &self,
        key: &EntryKey,
        f: impl FnOnce(&mut Transaction, Option<Row>, Fail) -> Result<T, Failure>,
    ) -> Result<T, Failure> {
        self.db.with(|tx, fail| {
            lock(tx, "journal", &key.digest(), fail)?;
            let row = journal_row(tx, key, fail)?;
            f(tx, row, fail)
        })
    }
}

impl JournalStore for PgJournalStore {
    fn create_pending(&self, key: &EntryKey, run_id: &str) -> Result<bool, Failure> {
        self.edit(key, |tx, row, fail| {
            if row.is_some() {
                return Ok(false);
            }
            put_row(tx, key, &Row::pending(key, run_id), fail)?;
            Ok(true)
        })
    }
    fn read(&self, key: &EntryKey) -> Result<Option<Row>, Failure> {
        self.db.with(|tx, fail| journal_row(tx, key, fail))
    }
    fn replace_if_pending(
        &self,
        key: &EntryKey,
        holder: &str,
        run_id: &str,
    ) -> Result<bool, Failure> {
        self.edit(key, |tx, row, fail| {
            if let Some(Row::Pending {
                run_id: current,
                attempts,
                ..
            }) = row
            {
                if current == holder {
                    put_row(
                        tx,
                        key,
                        &Row::Pending {
                            key: key.clone(),
                            run_id: run_id.into(),
                            attempts,
                        },
                        fail,
                    )?;
                    return Ok(true);
                }
            }
            Ok(false)
        })
    }
    fn record(&self, key: &EntryKey, value: &Stored) -> Result<Option<Stored>, Failure> {
        self.edit(key, |tx, row, fail| {
            if let Some(Row::Recorded { value, .. }) = row {
                return Ok(Some(value));
            }
            put_row(
                tx,
                key,
                &Row::Recorded {
                    key: key.clone(),
                    value: value.clone(),
                },
                fail,
            )?;
            Ok(None)
        })
    }
    fn release(&self, key: &EntryKey, run_id: &str) -> Result<(), Failure> {
        self.edit(key, |tx, row, fail| {
            if matches!(row, Some(Row::Pending { run_id: holder, .. }) if holder == run_id) {
                tx.execute("DELETE FROM contextful_journal WHERE execution_id = $1 AND step_label = $2 AND input_hash = $3", &[&key.execution_id, &key.step_label, &key.input_hash]).map_err(|e| fail(&e))?;
            }
            Ok(())
        })
    }
    fn note_attempts(&self, key: &EntryKey, run_id: &str, attempts: u32) -> Result<(), Failure> {
        self.edit(key, |tx, row, fail| {
            if row.is_none()
                || matches!(row, Some(Row::Pending { run_id: holder, .. }) if holder == run_id)
            {
                put_row(
                    tx,
                    key,
                    &Row::Pending {
                        key: key.clone(),
                        run_id: run_id.into(),
                        attempts,
                    },
                    fail,
                )?;
            }
            Ok(())
        })
    }
    fn rows(&self, execution_id: &str) -> Result<Vec<Row>, Failure> {
        self.db.with(|tx, fail| tx.query("SELECT body FROM contextful_journal WHERE execution_id = $1 ORDER BY step_label, input_hash", &[&execution_id]).map_err(|e| fail(&e))?.iter().map(|r| from_text(r.get(0), fail)).collect())
    }
    fn retire(&self, execution_id: &str) -> Result<(), Failure> {
        self.db.with(|tx, fail| {
            tx.execute(
                "DELETE FROM contextful_journal WHERE execution_id = $1",
                &[&execution_id],
            )
            .map_err(|e| fail(&e))?;
            Ok(())
        })
    }
    fn executions(&self) -> Result<Vec<String>, Failure> {
        self.db.with(|tx, fail| {
            Ok(tx
                .query(
                    "SELECT DISTINCT execution_id FROM contextful_journal ORDER BY execution_id",
                    &[],
                )
                .map_err(|e| fail(&e))?
                .iter()
                .map(|r| r.get(0))
                .collect())
        })
    }
}

#[derive(Clone)]
pub struct PgBlobStore {
    db: Arc<PgCatalog>,
}

impl BlobStore for PgBlobStore {
    fn put(&self, sha256: &str, bytes: &[u8]) -> Result<(), Failure> {
        self.db.with(|tx, fail| {
            tx.execute("INSERT INTO contextful_blob (sha256, bytes, put_at) VALUES ($1, $2, EXTRACT(EPOCH FROM clock_timestamp())::BIGINT)
                ON CONFLICT (sha256) DO UPDATE SET put_at = excluded.put_at", &[&sha256, &bytes]).map_err(|e| fail(&e))?;
            Ok(())
        })
    }
    fn get(&self, sha256: &str) -> Result<Option<Vec<u8>>, Failure> {
        self.db.with(|tx, fail| {
            Ok(tx
                .query_opt(
                    "SELECT bytes FROM contextful_blob WHERE sha256 = $1",
                    &[&sha256],
                )
                .map_err(|e| fail(&e))?
                .map(|r| r.get(0)))
        })
    }
    fn sweep(&self, referenced: &[String], now_unix: i64) -> Result<Vec<String>, Failure> {
        self.db.with(|tx, fail| {
            let rows = tx
                .query("SELECT sha256, put_at FROM contextful_blob FOR UPDATE", &[])
                .map_err(|e| fail(&e))?;
            let doomed: Vec<String> = rows
                .iter()
                .filter_map(|r| {
                    let sha: String = r.get(0);
                    let at: i64 = r.get(1);
                    sweepable(
                        referenced.contains(&sha),
                        u64::try_from(now_unix.saturating_sub(at)).unwrap_or(0),
                    )
                    .then_some(sha)
                })
                .collect();
            for sha in &doomed {
                tx.execute("DELETE FROM contextful_blob WHERE sha256 = $1", &[sha])
                    .map_err(|e| fail(&e))?;
            }
            Ok(doomed)
        })
    }
    fn swept_at(&self) -> Result<Option<i64>, Failure> {
        self.db.with(|tx, fail| {
            Ok(tx
                .query_opt("SELECT swept_at FROM contextful_sweep WHERE id = TRUE", &[])
                .map_err(|e| fail(&e))?
                .map(|r| r.get(0)))
        })
    }
    fn mark_swept(&self, at: i64) -> Result<(), Failure> {
        self.db.with(|tx, fail| { tx.execute("INSERT INTO contextful_sweep (id, swept_at) VALUES (TRUE, $1) ON CONFLICT (id) DO UPDATE SET swept_at = excluded.swept_at", &[&at]).map_err(|e| fail(&e))?; Ok(()) })
    }
}

#[derive(Clone)]
pub struct PgAwakeableStore {
    db: Arc<PgCatalog>,
}

fn read_json<T: DeserializeOwned>(
    tx: &mut Transaction,
    token: &str,
    fail: Fail,
) -> Result<Option<T>, Failure> {
    tx.query_opt(
        "SELECT body FROM contextful_awakeable WHERE token = $1",
        &[&token],
    )
    .map_err(|e| fail(&e))?
    .map(|r| from_text(r.get(0), fail))
    .transpose()
}
fn put_json<T: Serialize>(
    tx: &mut Transaction,
    token: &str,
    row: &T,
    fail: Fail,
) -> Result<(), Failure> {
    let body = to_text(row, fail)?;
    tx.execute("INSERT INTO contextful_awakeable (token, body) VALUES ($1, $2) ON CONFLICT (token) DO UPDATE SET body = excluded.body", &[&token, &body]).map_err(|e| fail(&e))?;
    Ok(())
}

fn recovered_callback(
    tx: &mut Transaction,
    mut row: Awakeable,
    fail: Fail,
) -> Result<Awakeable, Failure> {
    if row.state == AwakeableState::Pending {
        // The journal's committed resume value survives a crash before the callback row.
        let key = EntryKey {
            execution_id: row.execution_id.clone(),
            step_label: format!("awakeable:{}", row.token),
            input_hash: awakeable_key(&row.token),
        };
        if let Some(Row::Recorded { value, .. }) = journal_row(tx, &key, fail)? {
            let digest = match &value {
                Stored::Blob { sha256, .. } => sha256.clone(),
                Stored::Inline { .. } => sha256_hex(
                    &value
                        .inline_bytes()
                        .ok_or_else(|| fail(&"callback journal holds malformed inline bytes"))?,
                ),
            };
            row.state = AwakeableState::Resolved;
            row.payload_sha256 = Some(digest);
            row.payload = Some(value);
        }
    }
    Ok(row)
}

impl AwakeableStore for PgAwakeableStore {
    fn insert(&self, row: &Awakeable) -> Result<(), Failure> {
        self.db.with(|tx, fail| {
            lock(tx, "awakeable", &row.token, fail)?;
            put_json(tx, &row.token, row, fail)
        })
    }
    fn get(&self, token: &str) -> Result<Option<Awakeable>, Failure> {
        self.db.with(|tx, fail| {
            read_json(tx, token, fail)?
                .map(|row| recovered_callback(tx, row, fail))
                .transpose()
        })
    }
    fn update(
        &self,
        token: &str,
        edit: &mut dyn FnMut(&mut Awakeable) -> Result<bool, Failure>,
    ) -> Result<Option<Awakeable>, Failure> {
        self.db.with(|tx, fail| {
            lock(tx, "awakeable", token, fail)?;
            let Some(raw) = read_json::<Awakeable>(tx, token, fail)? else {
                return Ok(None);
            };
            let current = recovered_callback(tx, raw.clone(), fail)?;
            let mut row = current.clone();
            if edit(&mut row)? {
                put_json(tx, token, &row, fail)?;
                Ok(Some(row))
            } else {
                if current != raw {
                    put_json(tx, token, &current, fail)?;
                }
                Ok(Some(current))
            }
        })
    }
    fn rows(&self) -> Result<Vec<Awakeable>, Failure> {
        self.db.with(|tx, fail| {
            let rows = tx
                .query("SELECT body FROM contextful_awakeable ORDER BY token", &[])
                .map_err(|e| fail(&e))?;
            rows.iter()
                .map(|r| recovered_callback(tx, from_text(r.get(0), fail)?, fail))
                .collect()
        })
    }
}
