//! `derived.sqlite` behind the `DerivedCatalog` port: one row per table, per reachable
//! snapshot and per committed run, replaced whole by each rebuild
//! (`store.lay-out.derived-catalog`).

use crate::{open, storage};
use contextful_core::run::Failure;
use contextful_core::store::catalog::{DerivedCatalog, DerivedRows, DerivedRun, DerivedSnapshot, DerivedTable};
use contextful_core::time::Instant;
use rusqlite::{params, Connection, TransactionBehavior};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS tbl (
    name        TEXT PRIMARY KEY,
    schema      TEXT NOT NULL,
    snapshot_id TEXT,
    fence       INTEGER
);
CREATE TABLE IF NOT EXISTS snapshot (
    tbl         TEXT NOT NULL,
    snapshot_id TEXT NOT NULL,
    parent      TEXT,
    created_at  TEXT NOT NULL,
    row_count   INTEGER NOT NULL,
    parts       INTEGER NOT NULL,
    PRIMARY KEY (tbl, snapshot_id)
);
CREATE TABLE IF NOT EXISTS run (
    tbl          TEXT NOT NULL,
    run_id       TEXT NOT NULL,
    node_id      TEXT NOT NULL,
    committed_at TEXT NOT NULL,
    parts        INTEGER NOT NULL,
    pipeline_id  TEXT,
    PRIMARY KEY (tbl, run_id, node_id)
);
";

/// The rebuildable catalog in one SQLite file.
pub struct DerivedSqlite {
    path: PathBuf,
    conn: Mutex<Connection>,
}

impl DerivedSqlite {
    /// Open or create the catalog at `path`.
    pub fn open(path: &Path) -> Result<DerivedSqlite, Failure> {
        Ok(DerivedSqlite { path: path.to_path_buf(), conn: Mutex::new(open(path, SCHEMA)?) })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    fn fail(&self, e: impl std::fmt::Display) -> Failure {
        storage(&self.path, e)
    }
}

fn instant(s: String) -> rusqlite::Result<Instant> {
    Instant::parse(&s).map_err(|e| rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, Box::new(e)))
}

impl DerivedCatalog for DerivedSqlite {
    fn replace(&self, rows: &DerivedRows) -> Result<(), Failure> {
        let mut conn = self.conn.lock().unwrap_or_else(|p| p.into_inner());
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate).map_err(|e| self.fail(e))?;
        tx.execute_batch("DELETE FROM tbl; DELETE FROM snapshot; DELETE FROM run;").map_err(|e| self.fail(e))?;
        for t in &rows.tables {
            tx.execute(
                "INSERT INTO tbl (name, schema, snapshot_id, fence) VALUES (?1, ?2, ?3, ?4)",
                params![t.table, t.schema, t.snapshot_id, t.fence.map(|f| f as i64)],
            )
            .map_err(|e| self.fail(e))?;
        }
        for s in &rows.snapshots {
            tx.execute(
                "INSERT INTO snapshot (tbl, snapshot_id, parent, created_at, row_count, parts) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params![s.table, s.snapshot_id, s.parent, s.created_at.to_rfc3339_nanos(), s.row_count as i64, s.parts as i64],
            )
            .map_err(|e| self.fail(e))?;
        }
        for r in &rows.runs {
            tx.execute(
                "INSERT INTO run (tbl, run_id, node_id, committed_at, parts, pipeline_id) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params![r.table, r.run_id, r.node_id, r.committed_at.to_rfc3339_nanos(), r.parts as i64, r.pipeline_id],
            )
            .map_err(|e| self.fail(e))?;
        }
        tx.commit().map_err(|e| self.fail(e))
    }

    fn rows(&self) -> Result<DerivedRows, Failure> {
        let mut conn = self.conn.lock().unwrap_or_else(|p| p.into_inner());
        let tx = conn.transaction().map_err(|e| self.fail(e))?;
        let tables = {
            let mut stmt = tx.prepare("SELECT name, schema, snapshot_id, fence FROM tbl ORDER BY name").map_err(|e| self.fail(e))?;
            let rows = stmt
                .query_map([], |r| {
                    Ok(DerivedTable {
                        table: r.get(0)?,
                        schema: r.get(1)?,
                        snapshot_id: r.get(2)?,
                        fence: r.get::<_, Option<i64>>(3)?.map(|f| f as u64),
                    })
                })
                .map_err(|e| self.fail(e))?;
            rows.collect::<Result<Vec<_>, _>>().map_err(|e| self.fail(e))?
        };
        let snapshots = {
            let mut stmt = tx
                .prepare("SELECT tbl, snapshot_id, parent, created_at, row_count, parts FROM snapshot ORDER BY tbl, snapshot_id")
                .map_err(|e| self.fail(e))?;
            let rows = stmt
                .query_map([], |r| {
                    Ok(DerivedSnapshot {
                        table: r.get(0)?,
                        snapshot_id: r.get(1)?,
                        parent: r.get(2)?,
                        created_at: instant(r.get(3)?)?,
                        row_count: r.get::<_, i64>(4)? as u64,
                        parts: r.get::<_, i64>(5)? as u64,
                    })
                })
                .map_err(|e| self.fail(e))?;
            rows.collect::<Result<Vec<_>, _>>().map_err(|e| self.fail(e))?
        };
        let runs = {
            let mut stmt = tx
                .prepare("SELECT tbl, run_id, node_id, committed_at, parts, pipeline_id FROM run ORDER BY tbl, run_id, node_id")
                .map_err(|e| self.fail(e))?;
            let rows = stmt
                .query_map([], |r| {
                    Ok(DerivedRun {
                        table: r.get(0)?,
                        run_id: r.get(1)?,
                        node_id: r.get(2)?,
                        committed_at: instant(r.get(3)?)?,
                        parts: r.get::<_, i64>(4)? as u64,
                        pipeline_id: r.get(5)?,
                    })
                })
                .map_err(|e| self.fail(e))?;
            rows.collect::<Result<Vec<_>, _>>().map_err(|e| self.fail(e))?
        };
        tx.commit().map_err(|e| self.fail(e))?;
        Ok(DerivedRows { tables, snapshots, runs })
    }
}
