//! The per-unit disclosure budget behind the `BudgetLedger` port
//! (`disclosure.release.budget-reservation`): one row per contributing unit holding its
//! lifetime cap and reserved spend, and one row per unit a release charged.
//!
//! A reservation reads the named units' balances, applies `reserve_all` and writes the
//! result inside one `BEGIN IMMEDIATE` transaction, so a second connection's reservation
//! waits on the write lock and reads the balances the first one committed.

use crate::{open, storage};
use contextful_core::disclosure::noise::Spend;
use contextful_core::disclosure::release::{reserve_all, BudgetLedger, ReleaseError, UnitBudget};
use contextful_core::run::Failure;
use rusqlite::{params, Connection, OptionalExtension, TransactionBehavior};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS disclosure_budget (
    unit  TEXT PRIMARY KEY,
    cap   INTEGER NOT NULL,
    spent INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS disclosure_reservation (
    release_id TEXT NOT NULL,
    unit       TEXT NOT NULL,
    spend      INTEGER NOT NULL,
    PRIMARY KEY (release_id, unit)
);
";

/// The budget ledger in one SQLite file.
pub struct SqliteBudgetLedger {
    path: PathBuf,
    conn: Mutex<Connection>,
}

/// Micro-epsilon as stored: SQLite integers are signed 64-bit.
fn stored(v: u64) -> i64 {
    i64::try_from(v).unwrap_or(i64::MAX)
}

impl SqliteBudgetLedger {
    /// Open or create the ledger at `path`.
    pub fn open(path: &Path) -> Result<SqliteBudgetLedger, Failure> {
        Ok(SqliteBudgetLedger { path: path.to_path_buf(), conn: Mutex::new(open(path, SCHEMA)?) })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    fn fail(&self, e: impl std::fmt::Display) -> Failure {
        storage(&self.path, e)
    }

    /// Sets `unit`'s lifetime cap to `cap` micro-epsilon, keeping its reserved spend.
    pub fn grant(&self, unit: &str, cap: u64) -> Result<(), Failure> {
        let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        conn.execute(
            "INSERT INTO disclosure_budget (unit, cap, spent) VALUES (?1, ?2, 0)
             ON CONFLICT(unit) DO UPDATE SET cap = excluded.cap",
            params![unit, stored(cap)],
        )
        .map_err(|e| self.fail(e))?;
        Ok(())
    }

    /// `unit`'s cap and reserved spend; `None` for a unit never granted.
    pub fn budget(&self, unit: &str) -> Result<Option<UnitBudget>, Failure> {
        let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        conn.query_row("SELECT cap, spent FROM disclosure_budget WHERE unit = ?1", params![unit], |r| {
            Ok(UnitBudget { cap: r.get::<_, i64>(0)?.max(0) as u64, spent: r.get::<_, i64>(1)?.max(0) as u64 })
        })
        .optional()
        .map_err(|e| self.fail(e))
    }
}

impl BudgetLedger for SqliteBudgetLedger {
    fn reserve(&self, release_id: &str, units: &BTreeSet<String>, spend: Spend) -> Result<(), ReleaseError> {
        let mut conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        let ledger = |e: rusqlite::Error| ReleaseError::Ledger(self.fail(e));
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate).map_err(ledger)?;
        // A release id already holding a reservation is a retry: it charges nothing.
        let seen: Option<i64> =
            tx.query_row("SELECT 1 FROM disclosure_reservation WHERE release_id = ?1 LIMIT 1", params![release_id], |r| r.get(0)).optional().map_err(ledger)?;
        if seen.is_some() {
            return tx.commit().map_err(ledger);
        }
        let mut balances = BTreeMap::new();
        {
            let mut read = tx.prepare("SELECT cap, spent FROM disclosure_budget WHERE unit = ?1").map_err(ledger)?;
            for unit in units {
                let row = read
                    .query_row(params![unit], |r| Ok(UnitBudget { cap: r.get::<_, i64>(0)?.max(0) as u64, spent: r.get::<_, i64>(1)?.max(0) as u64 }))
                    .optional()
                    .map_err(ledger)?;
                if let Some(b) = row {
                    balances.insert(unit.clone(), b);
                }
            }
        }
        // A refusal drops the transaction, which rolls it back.
        reserve_all(&mut balances, units, spend)?;
        {
            let mut write = tx.prepare("UPDATE disclosure_budget SET spent = ?2 WHERE unit = ?1").map_err(ledger)?;
            let mut record = tx.prepare("INSERT INTO disclosure_reservation (release_id, unit, spend) VALUES (?1, ?2, ?3)").map_err(ledger)?;
            for unit in units {
                write.execute(params![unit, stored(balances[unit].spent)]).map_err(ledger)?;
                record.execute(params![release_id, unit, stored(spend.micros())]).map_err(ledger)?;
            }
        }
        tx.commit().map_err(ledger)
    }
}
