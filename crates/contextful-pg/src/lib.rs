//! The Postgres adapter behind the `Catalog` port: a self-hosted cluster's daemons share
//! one database, and every single-writer operation is a conditional update the database
//! serializes (`topology.coordinate.backends`). Expiry is evaluated against the database's
//! clock, which every daemon shares (`topology.coordinate.catalog-clock`).
//!
//! A lease acquisition is one conditional `UPDATE` whose predicate reads the stored holder
//! and expiry and whose assignment increments the fence. A cursor commit locks its scope
//! row, takes a shared lock on the lease row its fence names, and updates on the stored
//! version, so an acquisition racing it waits for the commit to settle.

use contextful_core::coordinate::{Cas, Catalog, CursorRow, Lease, LeaseKey, LeaseRow};
use contextful_core::run::own::{ExecutionOwner, OwnerScope};
use contextful_core::run::record::RunRow;
use contextful_core::run::{Failure, FailureTag, RunError};
use contextful_core::store::StoreError;
use contextful_core::time::Instant;
use postgres::{Client, Config, NoTls, Transaction};
use std::sync::Mutex;

/// The catalog's tables, created once under an advisory lock so daemons opening together
/// do not race the DDL. Instants are microseconds since the epoch on the database's clock.
const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS contextful_lease (
    key        TEXT PRIMARY KEY,
    holder     TEXT,
    expires_us BIGINT,
    fence      BIGINT NOT NULL
);
CREATE TABLE IF NOT EXISTS contextful_scope (
    pipeline_id TEXT NOT NULL,
    tbl         TEXT NOT NULL,
    kind        TEXT NOT NULL,
    part        TEXT NOT NULL,
    version     BIGINT NOT NULL,
    cursor      TEXT NOT NULL,
    owner       TEXT,
    retired     TEXT,
    PRIMARY KEY (pipeline_id, tbl, kind, part)
);
ALTER TABLE contextful_scope ADD COLUMN IF NOT EXISTS retired TEXT;
CREATE TABLE IF NOT EXISTS contextful_run (
    run_id      TEXT PRIMARY KEY,
    pipeline_id TEXT NOT NULL,
    host_scope  TEXT,
    started_ns  BIGINT NOT NULL,
    row         TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS contextful_run_by_pipeline ON contextful_run (pipeline_id);
CREATE INDEX IF NOT EXISTS contextful_run_by_host_scope ON contextful_run (host_scope) WHERE host_scope IS NOT NULL;
";

/// The advisory lock key the schema creation holds.
const SCHEMA_LOCK: i64 = 0x636f_6e74_6578_7466;

/// The database's clock in microseconds since the epoch.
const NOW_US: &str = "(EXTRACT(EPOCH FROM clock_timestamp()) * 1000000)::BIGINT";

/// The catalog in one Postgres database, over one connection per daemon.
pub struct PgCatalog {
    config: Config,
    /// `host:port/database`, the name every failure carries; never the password.
    name: String,
    client: Mutex<Client>,
}

type Fail<'a> = &'a dyn Fn(&dyn std::fmt::Display) -> Failure;

fn storage(name: &str, e: impl std::fmt::Display) -> Failure {
    Failure::new(FailureTag::Storage, format!("Postgres catalog {name}: {e}"))
}

/// `host:port/database` of a parsed connection string.
fn describe(config: &Config) -> String {
    let host = config
        .get_hosts()
        .iter()
        .map(|h| match h {
            postgres::config::Host::Tcp(h) => h.clone(),
            #[cfg(unix)]
            postgres::config::Host::Unix(p) => p.display().to_string(),
        })
        .next()
        .unwrap_or_else(|| "localhost".into());
    let port = config.get_ports().first().copied().unwrap_or(5432);
    format!("{host}:{port}/{}", config.get_dbname().unwrap_or(""))
}

impl PgCatalog {
    /// Connect to the database `conninfo` names, a key-value string or a `postgres://`
    /// URL, and create the catalog's tables on first open.
    pub fn connect(conninfo: &str) -> Result<PgCatalog, Failure> {
        // A parse error can quote the string, password included, so it is not passed on.
        let config: Config = conninfo.parse().map_err(|_| storage("", "the connection string does not parse"))?;
        let name = describe(&config);
        let client = config.connect(NoTls).map_err(|e| storage(&name, e))?;
        let catalog = PgCatalog { config, name, client: Mutex::new(client) };
        catalog.with(|tx, fail| {
            tx.execute("SELECT pg_advisory_xact_lock($1)", &[&SCHEMA_LOCK]).map_err(|e| fail(&e))?;
            tx.batch_execute(SCHEMA).map_err(|e| fail(&e))
        })?;
        Ok(catalog)
    }

    /// `host:port/database` of the catalog.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Run `f` in one transaction, reconnecting first when the server closed the
    /// connection.
    fn with<T>(&self, f: impl FnOnce(&mut Transaction, Fail) -> Result<T, Failure>) -> Result<T, Failure> {
        let fail = |e: &dyn std::fmt::Display| storage(&self.name, e);
        let mut client = self.client.lock().unwrap_or_else(|p| p.into_inner());
        if client.is_closed() {
            *client = self.config.connect(NoTls).map_err(|e| fail(&e))?;
        }
        let mut tx = client.transaction().map_err(|e| fail(&e))?;
        let out = f(&mut tx, &fail)?;
        tx.commit().map_err(|e| fail(&e))?;
        Ok(out)
    }
}

fn instant_us(us: i64, fail: Fail) -> Result<Instant, Failure> {
    Instant::from_unix_nanos(i128::from(us) * 1000).map_err(|e| fail(&e))
}

fn ttl_us(ttl_secs: u64) -> i64 {
    i64::try_from(ttl_secs).unwrap_or(i64::MAX / 1_000_000).saturating_mul(1_000_000)
}

fn fence_i64(fence: u64, fail: Fail) -> Result<i64, Failure> {
    i64::try_from(fence).map_err(|e| fail(&e))
}

fn to_text<T: serde::Serialize>(v: &T, fail: Fail) -> Result<String, Failure> {
    serde_json::to_string(v).map_err(|e| fail(&e))
}

fn from_text<T: serde::de::DeserializeOwned>(s: &str, fail: Fail) -> Result<T, Failure> {
    serde_json::from_str(s).map_err(|e| fail(&e))
}

fn lease_of(key: &str, holder: &str, fence: i64, expires_us: i64, fail: Fail) -> Result<Lease, Failure> {
    Ok(Lease { key: key.to_string(), holder: holder.to_string(), fence: fence as u64, expires_at: instant_us(expires_us, fail)? })
}

/// The lease row of `key`; `lock` takes a shared row lock an acquisition waits on.
fn lease_row(tx: &mut Transaction, key: &str, lock: bool, fail: Fail) -> Result<LeaseRow, Failure> {
    let sql = if lock {
        "SELECT holder, expires_us, fence FROM contextful_lease WHERE key = $1 FOR SHARE"
    } else {
        "SELECT holder, expires_us, fence FROM contextful_lease WHERE key = $1"
    };
    let Some(r) = tx.query_opt(sql, &[&key]).map_err(|e| fail(&e))? else { return Ok(LeaseRow::default()) };
    let expires_at = r.get::<_, Option<i64>>(1).map(|us| instant_us(us, fail)).transpose()?;
    Ok(LeaseRow { holder: r.get(0), expires_at, fence: r.get::<_, i64>(2) as u64 })
}

/// The row key of a scope, as the machine catalog spells it.
fn key(scope: &OwnerScope) -> [&str; 4] {
    match scope {
        OwnerScope::Table { pipeline_id, table } => [pipeline_id, table, "table", ""],
        OwnerScope::Chunk { pipeline_id, table, chunk } => [pipeline_id, table, "chunk", chunk],
        OwnerScope::Host { host_scope } => ["", "", "host", host_scope],
    }
}

const WHERE_KEY: &str = "pipeline_id = $1 AND tbl = $2 AND kind = $3 AND part = $4";

/// Insert the zero-version row a first compare-and-swap predicates on.
fn seed_scope(tx: &mut Transaction, at: &OwnerScope, fail: Fail) -> Result<(), Failure> {
    let [p, t, k, c] = key(at);
    let empty = to_text(&CursorRow::default(), fail)?;
    tx.execute(
        "INSERT INTO contextful_scope (pipeline_id, tbl, kind, part, version, cursor) VALUES ($1, $2, $3, $4, 0, $5) ON CONFLICT DO NOTHING",
        &[&p, &t, &k, &c, &empty],
    )
    .map_err(|e| fail(&e))?;
    Ok(())
}

/// A scope's cursor row and pending owner; `lock` seeds the row and holds it until the
/// transaction ends, so a conditional update after it sees no interleaved writer.
fn scope(tx: &mut Transaction, at: &OwnerScope, lock: bool, fail: Fail) -> Result<(CursorRow, Option<ExecutionOwner>), Failure> {
    if lock {
        seed_scope(tx, at, fail)?;
    }
    let [p, t, k, c] = key(at);
    let sql = format!("SELECT version, cursor, owner FROM contextful_scope WHERE {WHERE_KEY}{}", if lock { " FOR UPDATE" } else { "" });
    let Some(r) = tx.query_opt(sql.as_str(), &[&p, &t, &k, &c]).map_err(|e| fail(&e))? else { return Ok((CursorRow::default(), None)) };
    let cursor = CursorRow { version: r.get::<_, i64>(0) as u64, ..from_text(r.get::<_, &str>(1), fail)? };
    let owner = r.get::<_, Option<&str>>(2).map(|o| from_text(o, fail)).transpose()?;
    Ok((cursor, owner))
}

/// One conditional update on the stored version, read back by its affected-row count
/// (`topology.coordinate.cursor-cas`).
fn swap_cursor(tx: &mut Transaction, at: &OwnerScope, expected: u64, next: CursorRow, fail: Fail) -> Result<Cas, Failure> {
    let [p, t, k, c] = key(at);
    let stored = to_text(&CursorRow { version: expected + 1, ..next }, fail)?;
    let expected = i64::try_from(expected).map_err(|e| fail(&e))?;
    let sql = format!("UPDATE contextful_scope SET cursor = $5, version = version + 1 WHERE {WHERE_KEY} AND version = $6");
    let n = tx.execute(sql.as_str(), &[&p, &t, &k, &c, &stored, &expected]).map_err(|e| fail(&e))?;
    Ok(if n == 1 { Cas::Applied } else { Cas::VersionMoved })
}

/// The refusal a commit carrying `fence` meets once a later acquisition moved the lease
/// past it. The shared lock holds the lease row at its fence until the commit settles
/// (`topology.coordinate.fenced-commit`).
fn fenced(tx: &mut Transaction, at: &OwnerScope, fence: Option<&Lease>, fail: Fail) -> Result<Option<Cas>, Failure> {
    let Some(lease) = fence else { return Ok(None) };
    let row = lease_row(tx, &lease.key, true, fail)?;
    let at = match at.pipeline_table() {
        Some((p, t)) => format!("`{p}`/`{t}`"),
        None => at.to_string(),
    };
    Ok((!row.admits(lease.fence)).then(|| {
        Cas::Fenced(StoreError::LeaseFenced(format!("cursor commit for {at} carries fence {} and lease `{}` is at fence {}", lease.fence, lease.key, row.fence)))
    }))
}

fn run_row(tx: &mut Transaction, run_id: &str, lock: bool, fail: Fail) -> Result<Option<RunRow>, Failure> {
    let sql = if lock { "SELECT row FROM contextful_run WHERE run_id = $1 FOR UPDATE" } else { "SELECT row FROM contextful_run WHERE run_id = $1" };
    let row = tx.query_opt(sql, &[&run_id]).map_err(|e| fail(&e))?;
    row.map(|r| from_text(r.get::<_, &str>(0), fail)).transpose()
}

fn put_run_row(tx: &mut Transaction, row: &RunRow, fail: Fail) -> Result<(), Failure> {
    let text = to_text(row, fail)?;
    let started_ns = i64::try_from(row.started_at.unix_nanos()).map_err(|e| fail(&e))?;
    tx.execute(
        "INSERT INTO contextful_run (run_id, pipeline_id, host_scope, started_ns, row) VALUES ($1, $2, $3, $4, $5)
         ON CONFLICT (run_id) DO UPDATE SET pipeline_id = excluded.pipeline_id, host_scope = excluded.host_scope,
             started_ns = excluded.started_ns, row = excluded.row",
        &[&row.run_id, &row.pipeline_id, &row.host_scope, &started_ns, &text],
    )
    .map_err(|e| fail(&e))?;
    Ok(())
}

impl Catalog for PgCatalog {
    fn now(&self) -> Result<Instant, Failure> {
        self.with(|tx, fail| {
            let us: i64 = tx.query_one(format!("SELECT {NOW_US}").as_str(), &[]).map_err(|e| fail(&e))?.get(0);
            instant_us(us, fail)
        })
    }

    /// One conditional update: it matches a row with no holder or an expired one on the
    /// database's clock, and increments the fence (`topology.coordinate.fence-advances`).
    fn acquire(&self, key: &LeaseKey, holder: &str, ttl_secs: u64) -> Result<Option<Lease>, Failure> {
        let spelling = key.spelling();
        self.with(|tx, fail| {
            tx.execute("INSERT INTO contextful_lease (key, fence) VALUES ($1, 0) ON CONFLICT DO NOTHING", &[&spelling]).map_err(|e| fail(&e))?;
            let sql = format!(
                "UPDATE contextful_lease SET holder = $2, expires_us = {NOW_US} + $3, fence = fence + 1
                 WHERE key = $1 AND (holder IS NULL OR expires_us IS NULL OR expires_us <= {NOW_US})
                 RETURNING fence, expires_us"
            );
            let row = tx.query_opt(sql.as_str(), &[&spelling, &holder, &ttl_us(ttl_secs)]).map_err(|e| fail(&e))?;
            row.map(|r| lease_of(&spelling, holder, r.get(0), r.get(1), fail)).transpose()
        })
    }

    fn release(&self, lease: &Lease) -> Result<(), Failure> {
        self.with(|tx, fail| {
            let fence = fence_i64(lease.fence, fail)?;
            tx.execute("UPDATE contextful_lease SET holder = NULL, expires_us = NULL WHERE key = $1 AND fence = $2 AND holder = $3", &[&lease.key, &fence, &lease.holder])
                .map_err(|e| fail(&e))?;
            Ok(())
        })
    }

    fn renew(&self, lease: &Lease, ttl_secs: u64) -> Result<Option<Lease>, Failure> {
        self.with(|tx, fail| {
            let fence = fence_i64(lease.fence, fail)?;
            let sql = format!("UPDATE contextful_lease SET expires_us = {NOW_US} + $4 WHERE key = $1 AND fence = $2 AND holder = $3 RETURNING expires_us");
            let row = tx.query_opt(sql.as_str(), &[&lease.key, &fence, &lease.holder, &ttl_us(ttl_secs)]).map_err(|e| fail(&e))?;
            row.map(|r| Ok(Lease { expires_at: instant_us(r.get(0), fail)?, ..lease.clone() })).transpose()
        })
    }

    fn lease_holds(&self, lease: &Lease) -> Result<bool, Failure> {
        self.with(|tx, fail| {
            let fence = fence_i64(lease.fence, fail)?;
            let sql = format!("SELECT 1 FROM contextful_lease WHERE key = $1 AND fence = $2 AND holder = $3 AND expires_us > {NOW_US}");
            Ok(tx.query_opt(sql.as_str(), &[&lease.key, &fence, &lease.holder]).map_err(|e| fail(&e))?.is_some())
        })
    }

    fn lease_row(&self, key: &LeaseKey) -> Result<LeaseRow, Failure> {
        self.with(|tx, fail| lease_row(tx, &key.spelling(), false, fail))
    }

    fn cursor_at(&self, at: &OwnerScope) -> Result<CursorRow, Failure> {
        self.with(|tx, fail| Ok(scope(tx, at, false, fail)?.0))
    }

    fn cursor_cas_at(&self, at: &OwnerScope, expected_version: u64, next: CursorRow, fence: Option<&Lease>) -> Result<Cas, Failure> {
        self.with(|tx, fail| {
            if scope(tx, at, true, fail)?.0.version != expected_version {
                return Ok(Cas::VersionMoved);
            }
            if let Some(refused) = fenced(tx, at, fence, fail)? {
                return Ok(refused);
            }
            swap_cursor(tx, at, expected_version, next, fail)
        })
    }

    fn owner_at(&self, at: &OwnerScope) -> Result<Option<ExecutionOwner>, Failure> {
        self.with(|tx, fail| Ok(scope(tx, at, false, fail)?.1))
    }

    fn put_owner(&self, owner: &ExecutionOwner) -> Result<(), Failure> {
        self.with(|tx, fail| {
            seed_scope(tx, &owner.scope, fail)?;
            let [p, t, k, c] = key(&owner.scope);
            let text = to_text(owner, fail)?;
            let sql = format!("UPDATE contextful_scope SET owner = $5 WHERE {WHERE_KEY}");
            tx.execute(sql.as_str(), &[&p, &t, &k, &c, &text]).map_err(|e| fail(&e))?;
            Ok(())
        })
    }

    fn retire_at(&self, at: &OwnerScope, execution_id: &str, cursor: Option<(CursorRow, u64)>, fence: Option<&Lease>) -> Result<Cas, Failure> {
        self.with(|tx, fail| {
            let (_, owner) = scope(tx, at, true, fail)?;
            if let Some(refused) = fenced(tx, at, fence, fail)? {
                return Ok(refused);
            }
            if let Some((next, expected)) = cursor {
                // A moved version applies nothing, the owner's retirement included.
                if swap_cursor(tx, at, expected, next, fail)? == Cas::VersionMoved {
                    return Ok(Cas::VersionMoved);
                }
            }
            if owner.is_some_and(|o| o.execution_id == execution_id) {
                let [p, t, k, c] = key(at);
                let sql = format!("UPDATE contextful_scope SET owner = NULL WHERE {WHERE_KEY}");
                tx.execute(sql.as_str(), &[&p, &t, &k, &c]).map_err(|e| fail(&e))?;
            }
            if !execution_id.is_empty() {
                let [p, t, k, c] = key(at);
                let sql = format!("UPDATE contextful_scope SET retired = $5 WHERE {WHERE_KEY}");
                tx.execute(sql.as_str(), &[&p, &t, &k, &c, &execution_id]).map_err(|e| fail(&e))?;
            }
            Ok(Cas::Applied)
        })
    }

    fn retired_at(&self, at: &OwnerScope) -> Result<Option<String>, Failure> {
        self.with(|tx, fail| {
            let [p, t, k, c] = key(at);
            let sql = format!("SELECT retired FROM contextful_scope WHERE {WHERE_KEY}");
            let row = tx.query_opt(sql.as_str(), &[&p, &t, &k, &c]).map_err(|e| fail(&e))?;
            Ok(row.and_then(|r| r.get::<_, Option<String>>(0)))
        })
    }

    fn put_run(&self, row: &RunRow) -> Result<(), Failure> {
        self.with(|tx, fail| put_run_row(tx, row, fail))
    }

    fn run(&self, run_id: &str) -> Result<Option<RunRow>, Failure> {
        self.with(|tx, fail| run_row(tx, run_id, false, fail))
    }

    fn runs(&self, pipeline_id: Option<&str>) -> Result<Vec<RunRow>, Failure> {
        self.with(|tx, fail| {
            // Byte order, as the machine catalog lists them, whatever the database's collation.
            let rows = match pipeline_id {
                Some(p) => tx.query("SELECT row FROM contextful_run WHERE pipeline_id = $1 ORDER BY run_id COLLATE \"C\"", &[&p]),
                None => tx.query("SELECT row FROM contextful_run ORDER BY run_id COLLATE \"C\"", &[]),
            }
            .map_err(|e| fail(&e))?;
            rows.iter().map(|r| from_text(r.get::<_, &str>(0), fail)).collect()
        })
    }

    fn last_run_start(&self, pipeline_id: &str) -> Result<Option<Instant>, Failure> {
        self.with(|tx, fail| {
            let newest: Option<i64> = tx
                .query_one("SELECT MAX(started_ns) FROM contextful_run WHERE pipeline_id = $1 OR host_scope = $1", &[&pipeline_id])
                .map_err(|e| fail(&e))?
                .get(0);
            newest.map(|ns| Instant::from_unix_nanos(i128::from(ns)).map_err(|e| fail(&e))).transpose()
        })
    }

    fn update_run(&self, run_id: &str, f: &mut dyn FnMut(&mut RunRow) -> Result<(), RunError>) -> Result<Option<Result<RunRow, RunError>>, Failure> {
        self.with(|tx, fail| {
            let Some(mut row) = run_row(tx, run_id, true, fail)? else { return Ok(None) };
            if let Err(e) = f(&mut row) {
                return Ok(Some(Err(e)));
            }
            put_run_row(tx, &row, fail)?;
            Ok(Some(Ok(row)))
        })
    }
}
