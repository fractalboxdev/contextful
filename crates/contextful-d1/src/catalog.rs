//! Conditional D1 lease writes and a durable callback guard.

use crate::{Statement, D1};
use contextful_core::coordinate::{
    Cas, Catalog, CursorRow, ExecutionAdmission, ExecutionGuard, Lease, LeaseKey, LeaseRow,
};
use contextful_core::run::own::{ExecutionOwner, OwnerScope};
use contextful_core::run::record::{select_history, HistoryPage, RunRow, Window};
use contextful_core::run::{Failure, FailureTag, RunError};
use contextful_core::store::StoreError;
use contextful_core::time::Instant;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::sync::atomic::{AtomicU64, Ordering};

const NOW_US: &str = "CAST((julianday('now') - 2440587.5) * 86400000000 AS INTEGER)";
static NEXT_GUARD: AtomicU64 = AtomicU64::new(0);

pub struct D1Catalog {
    db: D1,
}

fn storage(error: impl std::fmt::Display) -> Failure {
    Failure::new(FailureTag::Storage, format!("D1 catalog: {error}"))
}

fn number(row: &Value, key: &str) -> Result<i64, Failure> {
    row.get(key)
        .and_then(Value::as_i64)
        .ok_or_else(|| storage(format!("missing {key}")))
}

fn instant(us: i64) -> Result<Instant, Failure> {
    Instant::from_unix_nanos(i128::from(us) * 1000).map_err(storage)
}

fn ttl_us(seconds: u64) -> i64 {
    i64::try_from(seconds)
        .unwrap_or(i64::MAX / 1_000_000)
        .saturating_mul(1_000_000)
}

struct Guard {
    endpoint: String,
    token: String,
}
impl ExecutionGuard for Guard {}
impl Drop for Guard {
    fn drop(&mut self) {
        if let Ok(db) = D1::connect(&self.endpoint) {
            let _ = db.query(
                "DELETE FROM contextful_guard WHERE scope = 'global' AND token = ?",
                vec![json!(self.token)],
            );
        }
    }
}

impl D1Catalog {
    pub fn connect(endpoint: &str) -> Result<Self, Failure> {
        let db = D1::connect(endpoint).map_err(storage)?;
        db.health().map_err(storage)?;
        Ok(Self { db })
    }

    /// Acquire `key` whatever its current holder or expiry, advancing the fence so every
    /// write predicated on the prior holder's lease stops matching.
    pub fn take_over(&self, key: &LeaseKey, holder: &str, ttl_secs: u64) -> Result<Lease, Failure> {
        let key = key.spelling();
        self.assert_mutable()?;
        self.db.query("INSERT OR IGNORE INTO contextful_lease (key, fence) SELECT ?, 0 WHERE NOT EXISTS (SELECT 1 FROM contextful_guard WHERE scope = 'global')", vec![json!(key)]).map_err(storage)?;
        let sql = format!("UPDATE contextful_lease SET holder = ?, expires_us = {NOW_US} + ?, fence = fence + 1 WHERE key = ? AND NOT EXISTS (SELECT 1 FROM contextful_guard WHERE scope = 'global') RETURNING fence, expires_us");
        let result = self
            .db
            .query(&sql, vec![json!(holder), json!(ttl_us(ttl_secs)), json!(key)])
            .map_err(storage)?;
        let row = result.rows.first().ok_or_else(|| {
            Failure::new(FailureTag::Transient, "execution callback guard blocks D1 lease or owner transfer")
        })?;
        Ok(Lease {
            key,
            holder: holder.into(),
            fence: number(row, "fence")? as u64,
            expires_at: instant(number(row, "expires_us")?)?,
        })
    }

    /// An orphaned callback guard remains visible until its external effect is resolved.
    pub fn pending_guard(&self) -> Result<Option<String>, Failure> {
        let rows = self
            .db
            .query(
                "SELECT token FROM contextful_guard WHERE scope = 'global'",
                vec![],
            )
            .map_err(storage)?
            .rows;
        rows.first()
            .map(|row| {
                row.get("token")
                    .and_then(Value::as_str)
                    .map(str::to_owned)
                    .ok_or_else(|| storage("missing guard token"))
            })
            .transpose()
    }

    /// A reconciled provider receipt or stable idempotency key admits a later owner.
    pub fn reconcile_guard(&self, token: &str, evidence: &str) -> Result<(), Failure> {
        if token.len() != 64
            || !token.bytes().all(|b| b.is_ascii_hexdigit())
            || evidence.len() > 512
            || !(evidence.starts_with("idempotency:") || evidence.starts_with("receipt:"))
            || evidence
                .split_once(':')
                .is_none_or(|(_, value)| value.trim().is_empty())
        {
            return Err(storage("ExecutionReconciliationEvidenceInvalid"));
        }
        let insert = format!("INSERT INTO contextful_reconciliation (token, evidence, reconciled_at) SELECT token, ?, {NOW_US} FROM contextful_guard WHERE scope = 'global' AND token = ?");
        let statements = [
            Statement {
                sql: &insert,
                params: vec![json!(evidence), json!(token)],
            },
            Statement {
                sql: "DELETE FROM contextful_guard WHERE scope = 'global' AND token = ?",
                params: vec![json!(token)],
            },
        ];
        let changed = self.db.batch(&statements).map_err(storage)?;
        if changed.len() != 2 || changed[0].changes != 1 || changed[1].changes != 1 {
            return Err(storage("ExecutionReconciliationGuardMoved"));
        }
        Ok(())
    }

    fn guard_held(&self) -> Result<bool, Failure> {
        Ok(!self
            .db
            .query(
                "SELECT 1 FROM contextful_guard WHERE scope = 'global'",
                vec![],
            )
            .map_err(storage)?
            .rows
            .is_empty())
    }

    fn assert_mutable(&self) -> Result<(), Failure> {
        if self.guard_held()? {
            Err(Failure::new(
                FailureTag::Transient,
                "execution callback guard blocks D1 lease or owner transfer",
            ))
        } else {
            Ok(())
        }
    }

    fn scope_key(scope: &OwnerScope) -> Result<String, Failure> {
        serde_json::to_string(scope).map_err(storage)
    }

    fn seed_scope(&self, scope: &OwnerScope) -> Result<(), Failure> {
        let key = Self::scope_key(scope)?;
        let cursor = serde_json::to_string(&CursorRow::default()).map_err(storage)?;
        self.db.query("INSERT OR IGNORE INTO contextful_scope (scope, version, cursor) SELECT ?, 0, ? WHERE NOT EXISTS (SELECT 1 FROM contextful_guard WHERE scope = 'global')", vec![json!(key), json!(cursor)]).map_err(storage)?;
        self.assert_mutable()
    }

    fn scope_row(&self, scope: &OwnerScope) -> Result<Option<Value>, Failure> {
        Ok(self
            .db
            .query(
                "SELECT version, cursor, owner, retired FROM contextful_scope WHERE scope = ?",
                vec![json!(Self::scope_key(scope)?)],
            )
            .map_err(storage)?
            .rows
            .into_iter()
            .next())
    }

    fn fence(&self, lease: Option<&Lease>) -> Result<Option<Cas>, Failure> {
        let Some(lease) = lease else { return Ok(None) };
        let row = self
            .db
            .query(
                "SELECT fence FROM contextful_lease WHERE key = ?",
                vec![json!(lease.key)],
            )
            .map_err(storage)?;
        let current = row
            .rows
            .first()
            .map(|r| number(r, "fence"))
            .transpose()?
            .unwrap_or(0) as u64;
        Ok((current != lease.fence).then(|| {
            Cas::Fenced(StoreError::LeaseFenced(format!(
                "D1 cursor commit carries fence {} and lease `{}` is at fence {current}",
                lease.fence, lease.key
            )))
        }))
    }

    fn fence_condition(sql: &mut String, args: &mut Vec<Value>, lease: Option<&Lease>) {
        if let Some(lease) = lease {
            sql.push_str(
                " AND EXISTS (SELECT 1 FROM contextful_lease WHERE key = ? AND fence = ?)",
            );
            args.extend([json!(lease.key), json!(lease.fence)]);
        }
    }

    fn parse_run(row: &Value) -> Result<RunRow, Failure> {
        serde_json::from_str(
            row.get("row")
                .and_then(Value::as_str)
                .ok_or_else(|| storage("missing run body"))?,
        )
        .map_err(storage)
    }

    fn write_run(&self, row: &RunRow) -> Result<(), Failure> {
        let body = serde_json::to_string(row).map_err(storage)?;
        let started_ns = i64::try_from(row.started_at.unix_nanos()).map_err(storage)?;
        self.db.query("INSERT INTO contextful_run (run_id, pipeline_id, host_scope, started_ns, row) VALUES (?, ?, ?, ?, ?) ON CONFLICT(run_id) DO UPDATE SET pipeline_id = excluded.pipeline_id, host_scope = excluded.host_scope, started_ns = excluded.started_ns, row = excluded.row", vec![json!(row.run_id), json!(row.pipeline_id), json!(row.host_scope), json!(started_ns.to_string()), json!(body)]).map_err(storage)?;
        Ok(())
    }
}

impl Catalog for D1Catalog {
    fn supports_strict_execution_guards(&self) -> bool {
        true
    }

    fn execution_guard(
        &self,
        admission: &ExecutionAdmission,
    ) -> Result<Box<dyn ExecutionGuard>, Failure> {
        let mut sql = format!("INSERT INTO contextful_guard (scope, token, admitted_at) SELECT 'global', ?, {NOW_US} WHERE NOT EXISTS (SELECT 1 FROM contextful_guard WHERE scope = 'global')");
        let nonce = NEXT_GUARD.fetch_add(1, Ordering::SeqCst);
        let token = format!(
            "{:x}",
            Sha256::digest(format!(
                "{}:{}:{}",
                std::process::id(),
                nonce,
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map_err(storage)?
                    .as_nanos()
            ))
        );
        let mut args = vec![json!(token)];
        for expected in admission.owners() {
            let scope = Self::scope_key(&expected.scope)?;
            let owner_query = self
                .db
                .query(
                    "SELECT owner FROM contextful_scope WHERE scope = ?",
                    vec![json!(scope)],
                )
                .map_err(storage)?;
            let owner_text = owner_query
                .rows
                .first()
                .and_then(|r| r.get("owner"))
                .and_then(Value::as_str)
                .ok_or_else(|| {
                    Failure::new(FailureTag::Transient, "execution admission was superseded")
                })?;
            let owner: ExecutionOwner = serde_json::from_str(owner_text).map_err(storage)?;
            if owner.execution_id != expected.execution_id
                || owner.pins != expected.pins
                || (expected.scope.host_id().is_some()
                    && owner.attempts.last() != Some(&expected.run_id))
            {
                return Err(Failure::new(
                    FailureTag::Transient,
                    "execution admission was superseded",
                ));
            }
            let run_query = self
                .db
                .query(
                    "SELECT row FROM contextful_run WHERE run_id = ?",
                    vec![json!(expected.run_id)],
                )
                .map_err(storage)?;
            let run_text = run_query
                .rows
                .first()
                .and_then(|r| r.get("row"))
                .and_then(Value::as_str)
                .ok_or_else(|| {
                    Failure::new(
                        FailureTag::Transient,
                        "execution attempt no longer admits the callback",
                    )
                })?;
            let run: RunRow = serde_json::from_str(run_text).map_err(storage)?;
            let now = self.now()?;
            if run.execution_id != expected.execution_id
                || !run.status.is_in_flight()
                || !run.owner.as_ref().is_some_and(|o| !o.expired(now))
                || (!admission.publication && run.stop.is_some())
            {
                return Err(Failure::new(
                    FailureTag::Transient,
                    "execution attempt no longer admits the callback",
                ));
            }
            let owner_expiry_us = i64::try_from(
                run.owner
                    .as_ref()
                    .expect("validated owner")
                    .lease_expires_at
                    .unix_nanos()
                    / 1000,
            )
            .map_err(storage)?;
            sql.push_str(
                " AND EXISTS (SELECT 1 FROM contextful_scope WHERE scope = ? AND owner = ?)",
            );
            args.extend([json!(scope), json!(owner_text)]);
            sql.push_str(&format!(" AND EXISTS (SELECT 1 FROM contextful_run WHERE run_id = ? AND row = ?) AND ? > {NOW_US}"));
            args.extend([
                json!(expected.run_id),
                json!(run_text),
                json!(owner_expiry_us),
            ]);
        }
        for lease in admission.leases() {
            sql.push_str(&format!(" AND EXISTS (SELECT 1 FROM contextful_lease WHERE key = ? AND holder = ? AND fence = ? AND expires_us > {NOW_US})"));
            args.extend([json!(lease.key), json!(lease.holder), json!(lease.fence)]);
        }
        if self.db.query(&sql, args).map_err(storage)?.changes != 1 {
            return Err(Failure::new(
                FailureTag::Transient,
                "execution admission was superseded or another callback holds D1 guard",
            ));
        }
        Ok(Box::new(Guard {
            endpoint: self.db.endpoint.clone(),
            token,
        }))
    }

    fn now(&self) -> Result<Instant, Failure> {
        let result = self
            .db
            .query(&format!("SELECT {NOW_US} AS now_us"), vec![])
            .map_err(storage)?;
        instant(number(
            result
                .rows
                .first()
                .ok_or_else(|| storage("missing clock"))?,
            "now_us",
        )?)
    }

    fn acquire(
        &self,
        key: &LeaseKey,
        holder: &str,
        ttl_secs: u64,
    ) -> Result<Option<Lease>, Failure> {
        let key = key.spelling();
        self.assert_mutable()?;
        self.db.query("INSERT OR IGNORE INTO contextful_lease (key, fence) SELECT ?, 0 WHERE NOT EXISTS (SELECT 1 FROM contextful_guard WHERE scope = 'global')", vec![json!(key)]).map_err(storage)?;
        let sql = format!("UPDATE contextful_lease SET holder = ?, expires_us = {NOW_US} + ?, fence = fence + 1 WHERE key = ? AND (holder IS NULL OR expires_us IS NULL OR expires_us <= {NOW_US}) AND NOT EXISTS (SELECT 1 FROM contextful_guard WHERE scope = 'global') RETURNING fence, expires_us");
        let result = self
            .db
            .query(
                &sql,
                vec![json!(holder), json!(ttl_us(ttl_secs)), json!(key)],
            )
            .map_err(storage)?;
        if let Some(row) = result.rows.first() {
            return Ok(Some(Lease {
                key,
                holder: holder.into(),
                fence: number(row, "fence")? as u64,
                expires_at: instant(number(row, "expires_us")?)?,
            }));
        }
        self.assert_mutable()?;
        Ok(None)
    }

    fn release(&self, lease: &Lease) -> Result<(), Failure> {
        let result = self.db.query("UPDATE contextful_lease SET holder = NULL, expires_us = NULL WHERE key = ? AND fence = ? AND holder = ? AND NOT EXISTS (SELECT 1 FROM contextful_guard WHERE scope = 'global')", vec![json!(lease.key), json!(lease.fence), json!(lease.holder)]).map_err(storage)?;
        if result.changes == 0 {
            self.assert_mutable()?
        }
        Ok(())
    }

    fn renew(&self, lease: &Lease, ttl_secs: u64) -> Result<Option<Lease>, Failure> {
        let sql = format!("UPDATE contextful_lease SET expires_us = {NOW_US} + ? WHERE key = ? AND fence = ? AND holder = ? RETURNING expires_us");
        let result = self
            .db
            .query(
                &sql,
                vec![
                    json!(ttl_us(ttl_secs)),
                    json!(lease.key),
                    json!(lease.fence),
                    json!(lease.holder),
                ],
            )
            .map_err(storage)?;
        result
            .rows
            .first()
            .map(|r| {
                Ok(Lease {
                    expires_at: instant(number(r, "expires_us")?)?,
                    ..lease.clone()
                })
            })
            .transpose()
    }

    fn lease_holds(&self, lease: &Lease) -> Result<bool, Failure> {
        let sql = format!("SELECT 1 FROM contextful_lease WHERE key = ? AND fence = ? AND holder = ? AND expires_us > {NOW_US}");
        Ok(!self
            .db
            .query(
                &sql,
                vec![json!(lease.key), json!(lease.fence), json!(lease.holder)],
            )
            .map_err(storage)?
            .rows
            .is_empty())
    }

    fn lease_row(&self, key: &LeaseKey) -> Result<LeaseRow, Failure> {
        let result = self
            .db
            .query(
                "SELECT holder, expires_us, fence FROM contextful_lease WHERE key = ?",
                vec![json!(key.spelling())],
            )
            .map_err(storage)?;
        let Some(row) = result.rows.first() else {
            return Ok(LeaseRow::default());
        };
        Ok(LeaseRow {
            holder: row.get("holder").and_then(Value::as_str).map(str::to_owned),
            expires_at: row
                .get("expires_us")
                .and_then(Value::as_i64)
                .map(instant)
                .transpose()?,
            fence: number(row, "fence")? as u64,
        })
    }

    fn cursor_at(&self, scope: &OwnerScope) -> Result<CursorRow, Failure> {
        let Some(row) = self.scope_row(scope)? else {
            return Ok(CursorRow::default());
        };
        let mut cursor: CursorRow = serde_json::from_str(
            row.get("cursor")
                .and_then(Value::as_str)
                .ok_or_else(|| storage("missing cursor body"))?,
        )
        .map_err(storage)?;
        cursor.version = number(&row, "version")? as u64;
        Ok(cursor)
    }

    fn cursor_cas_at(
        &self,
        scope: &OwnerScope,
        expected_version: u64,
        mut next: CursorRow,
        fence: Option<&Lease>,
    ) -> Result<Cas, Failure> {
        self.seed_scope(scope)?;
        next.version = expected_version
            .checked_add(1)
            .ok_or_else(|| storage("cursor version exhausted"))?;
        let body = serde_json::to_string(&next).map_err(storage)?;
        let mut args = vec![
            json!(body),
            json!(Self::scope_key(scope)?),
            json!(expected_version),
        ];
        let mut sql = "UPDATE contextful_scope SET cursor = ?, version = version + 1 WHERE scope = ? AND version = ? AND NOT EXISTS (SELECT 1 FROM contextful_guard WHERE scope = 'global')".to_string();
        Self::fence_condition(&mut sql, &mut args, fence);
        if self.db.query(&sql, args).map_err(storage)?.changes == 1 {
            return Ok(Cas::Applied);
        }
        self.assert_mutable()?;
        if self.cursor_at(scope)?.version != expected_version {
            return Ok(Cas::VersionMoved);
        }
        if let Some(refused) = self.fence(fence)? {
            return Ok(refused);
        }
        Ok(Cas::VersionMoved)
    }

    fn owner_at(&self, scope: &OwnerScope) -> Result<Option<ExecutionOwner>, Failure> {
        self.scope_row(scope)?
            .and_then(|r| r.get("owner").and_then(Value::as_str).map(str::to_owned))
            .map(|body| serde_json::from_str(&body).map_err(storage))
            .transpose()
    }

    fn put_owner(&self, owner: &ExecutionOwner) -> Result<(), Failure> {
        self.seed_scope(&owner.scope)?;
        let body = serde_json::to_string(owner).map_err(storage)?;
        let result = self.db.query("UPDATE contextful_scope SET owner = ? WHERE scope = ? AND NOT EXISTS (SELECT 1 FROM contextful_guard WHERE scope = 'global')", vec![json!(body), json!(Self::scope_key(&owner.scope)?)]).map_err(storage)?;
        if result.changes != 1 {
            self.assert_mutable()?;
            return Err(storage("owner row missing"));
        }
        Ok(())
    }

    fn retire_at(
        &self,
        scope: &OwnerScope,
        execution_id: &str,
        cursor: Option<(CursorRow, u64)>,
        fence: Option<&Lease>,
    ) -> Result<Cas, Failure> {
        self.seed_scope(scope)?;
        let expected_version = cursor.as_ref().map(|(_, expected)| *expected);
        let mut args = Vec::new();
        let mut sql = "UPDATE contextful_scope SET ".to_string();
        if let Some((mut next, expected)) = cursor {
            next.version = expected
                .checked_add(1)
                .ok_or_else(|| storage("cursor version exhausted"))?;
            sql.push_str("cursor = ?, version = version + 1, ");
            args.push(json!(serde_json::to_string(&next).map_err(storage)?));
            sql.push_str("owner = CASE WHEN json_extract(owner, '$.execution_id') = ? THEN NULL ELSE owner END, retired = CASE WHEN ? = '' THEN retired ELSE ? END WHERE scope = ? AND version = ? AND NOT EXISTS (SELECT 1 FROM contextful_guard WHERE scope = 'global')");
            args.extend([
                json!(execution_id),
                json!(execution_id),
                json!(execution_id),
                json!(Self::scope_key(scope)?),
                json!(expected),
            ]);
        } else {
            sql.push_str("owner = CASE WHEN json_extract(owner, '$.execution_id') = ? THEN NULL ELSE owner END, retired = CASE WHEN ? = '' THEN retired ELSE ? END WHERE scope = ? AND NOT EXISTS (SELECT 1 FROM contextful_guard WHERE scope = 'global')");
            args.extend([
                json!(execution_id),
                json!(execution_id),
                json!(execution_id),
                json!(Self::scope_key(scope)?),
            ]);
        }
        Self::fence_condition(&mut sql, &mut args, fence);
        if self.db.query(&sql, args).map_err(storage)?.changes == 1 {
            return Ok(Cas::Applied);
        }
        self.assert_mutable()?;
        if let Some(expected) = expected_version {
            if self.cursor_at(scope)?.version != expected {
                return Ok(Cas::VersionMoved);
            }
        }
        if let Some(refused) = self.fence(fence)? {
            return Ok(refused);
        }
        Ok(Cas::VersionMoved)
    }

    fn retired_at(&self, scope: &OwnerScope) -> Result<Option<String>, Failure> {
        Ok(self
            .scope_row(scope)?
            .and_then(|r| r.get("retired").and_then(Value::as_str).map(str::to_owned)))
    }

    fn put_run(&self, row: &RunRow) -> Result<(), Failure> {
        self.write_run(row)
    }

    fn run(&self, run_id: &str) -> Result<Option<RunRow>, Failure> {
        self.db
            .query(
                "SELECT row FROM contextful_run WHERE run_id = ?",
                vec![json!(run_id)],
            )
            .map_err(storage)?
            .rows
            .first()
            .map(Self::parse_run)
            .transpose()
    }

    fn runs(&self, pipeline_id: Option<&str>) -> Result<Vec<RunRow>, Failure> {
        let (sql, args) = match pipeline_id {
            Some(id) => ("SELECT row FROM contextful_run WHERE pipeline_id = ? ORDER BY run_id COLLATE BINARY", vec![json!(id)]),
            None => ("SELECT row FROM contextful_run ORDER BY run_id COLLATE BINARY", vec![]),
        };
        self.db
            .query(sql, args)
            .map_err(storage)?
            .rows
            .iter()
            .map(Self::parse_run)
            .collect()
    }

    fn unpushed_runs(&self) -> Result<Vec<RunRow>, Failure> {
        self.db
            .query("SELECT row FROM contextful_run WHERE json_extract(row, '$.unpushed') IS NOT NULL ORDER BY run_id COLLATE BINARY", vec![])
            .map_err(storage)?
            .rows
            .iter()
            .map(Self::parse_run)
            .collect()
    }

    fn history(&self, pipeline_id: Option<&str>, window: &Window) -> Result<HistoryPage, Failure> {
        let since = window.since.map_or(i64::MIN, |s| {
            let ns = s.unix_nanos();
            i64::try_from(ns).unwrap_or(if ns < 0 { i64::MIN } else { i64::MAX })
        });
        // One row past the ceiling tells a truncated window from a full one; the bound and the
        // order read the indexed integer the start text spells (`run.record.history-window`).
        let limit = window.ceiling.saturating_add(1);
        let (sql, args) = match pipeline_id {
            Some(id) => (
                "SELECT row FROM contextful_run WHERE pipeline_id = ? AND CAST(started_ns AS INTEGER) >= CAST(? AS INTEGER) ORDER BY CAST(started_ns AS INTEGER) DESC, run_id COLLATE BINARY DESC LIMIT ?",
                vec![json!(id), json!(since.to_string()), json!(limit)],
            ),
            None => (
                "SELECT row FROM contextful_run WHERE CAST(started_ns AS INTEGER) >= CAST(? AS INTEGER) ORDER BY CAST(started_ns AS INTEGER) DESC, run_id COLLATE BINARY DESC LIMIT ?",
                vec![json!(since.to_string()), json!(limit)],
            ),
        };
        let rows = self.db.query(sql, args).map_err(storage)?.rows.iter().map(Self::parse_run).collect::<Result<Vec<_>, _>>()?;
        Ok(select_history(rows, window))
    }

    fn last_run_start(&self, pipeline_id: &str) -> Result<Option<Instant>, Failure> {
        let result = self.db.query("SELECT CAST(MAX(CAST(started_ns AS INTEGER)) AS TEXT) AS started_ns FROM contextful_run WHERE pipeline_id = ? OR host_scope = ?", vec![json!(pipeline_id), json!(pipeline_id)]).map_err(storage)?;
        result
            .rows
            .first()
            .and_then(|r| r.get("started_ns"))
            .and_then(Value::as_str)
            .map(|ns| {
                ns.parse::<i128>()
                    .map_err(storage)
                    .and_then(|ns| Instant::from_unix_nanos(ns).map_err(storage))
            })
            .transpose()
    }

    fn update_run(
        &self,
        run_id: &str,
        f: &mut dyn FnMut(&mut RunRow) -> Result<(), RunError>,
    ) -> Result<Option<Result<RunRow, RunError>>, Failure> {
        for _ in 0..16 {
            let result = self
                .db
                .query(
                    "SELECT row FROM contextful_run WHERE run_id = ?",
                    vec![json!(run_id)],
                )
                .map_err(storage)?;
            let Some(current) = result.rows.first() else {
                return Ok(None);
            };
            let old = current
                .get("row")
                .and_then(Value::as_str)
                .ok_or_else(|| storage("missing run body"))?;
            let mut row: RunRow = serde_json::from_str(old).map_err(storage)?;
            if let Err(error) = f(&mut row) {
                return Ok(Some(Err(error)));
            }
            let body = serde_json::to_string(&row).map_err(storage)?;
            let started_ns = i64::try_from(row.started_at.unix_nanos()).map_err(storage)?;
            let updated = self.db.query("UPDATE contextful_run SET pipeline_id = ?, host_scope = ?, started_ns = ?, row = ? WHERE run_id = ? AND row = ?", vec![json!(row.pipeline_id), json!(row.host_scope), json!(started_ns.to_string()), json!(body), json!(run_id), json!(old)]).map_err(storage)?;
            if updated.changes == 1 {
                return Ok(Some(Ok(row)));
            }
        }
        Err(Failure::new(
            FailureTag::Transient,
            "D1 run row changed repeatedly",
        ))
    }
}
