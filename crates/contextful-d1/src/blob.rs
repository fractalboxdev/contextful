//! R2 blob content follows its D1 reference, with a durable per-digest write guard.

use crate::D1;
use contextful_core::run::journal::sweepable;
use contextful_core::run::ports::BlobStore;
use contextful_core::run::{Failure, FailureTag};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::sync::atomic::{AtomicU64, Ordering};

static NEXT: AtomicU64 = AtomicU64::new(0);

pub struct D1BlobStore {
    db: D1,
}

fn failure(error: impl std::fmt::Display) -> Failure {
    Failure::new(FailureTag::Storage, format!("D1/R2 blob: {error}"))
}

struct BlobGuard {
    endpoint: String,
    scope: String,
    token: String,
}
impl Drop for BlobGuard {
    fn drop(&mut self) {
        if let Ok(db) = D1::connect(&self.endpoint) {
            let _ = db.query(
                "DELETE FROM contextful_guard WHERE scope = ? AND token = ?",
                vec![json!(self.scope), json!(self.token)],
            );
        }
    }
}

impl D1BlobStore {
    pub fn connect(endpoint: &str) -> Result<Self, Failure> {
        let db = D1::connect(endpoint).map_err(failure)?;
        db.health().map_err(failure)?;
        Ok(Self { db })
    }

    fn lock(&self, sha256: &str) -> Result<BlobGuard, Failure> {
        let scope = format!("blob:{sha256}");
        let nonce = NEXT.fetch_add(1, Ordering::SeqCst);
        let token = format!(
            "{:x}",
            Sha256::digest(format!("{}:{}:{}", std::process::id(), nonce, scope))
        );
        let inserted = self.db.query("INSERT OR IGNORE INTO contextful_guard (scope, token, admitted_at) VALUES (?, ?, CAST(strftime('%s','now') AS INTEGER))", vec![json!(scope), json!(token)]).map_err(failure)?;
        if inserted.changes != 1 {
            return Err(Failure::new(
                FailureTag::Transient,
                "D1 blob is being written or swept",
            ));
        }
        Ok(BlobGuard {
            endpoint: self.db.endpoint.clone(),
            scope,
            token,
        })
    }

    fn referenced(&self, sha256: &str) -> Result<bool, Failure> {
        for sql in [
            "SELECT 1 FROM contextful_journal WHERE kind = 'recorded' AND json_extract(body, '$.value.sha256') = ? LIMIT 1",
            "SELECT 1 FROM contextful_awakeable WHERE json_extract(body, '$.payload.sha256') = ? LIMIT 1",
        ] {
            if !self.db.query(sql, vec![json!(sha256)]).map_err(failure)?.rows.is_empty() { return Ok(true) }
        }
        Ok(false)
    }
}

impl BlobStore for D1BlobStore {
    fn put(&self, sha256: &str, bytes: &[u8]) -> Result<(), Failure> {
        let _guard = self.lock(sha256)?;
        self.db.put_blob(sha256, bytes).map_err(failure)?;
        self.db.query("INSERT INTO contextful_blob (sha256, put_at) VALUES (?, CAST(strftime('%s','now') AS INTEGER)) ON CONFLICT(sha256) DO UPDATE SET put_at = excluded.put_at", vec![json!(sha256)]).map_err(failure)?;
        Ok(())
    }

    fn get(&self, sha256: &str) -> Result<Option<Vec<u8>>, Failure> {
        let found = self
            .db
            .query(
                "SELECT 1 FROM contextful_blob WHERE sha256 = ?",
                vec![json!(sha256)],
            )
            .map_err(failure)?;
        if found.rows.is_empty() {
            return Ok(None);
        }
        self.db
            .get_blob(sha256)
            .map_err(failure)?
            .ok_or_else(|| failure("ExecutionRecoveryMismatch: D1 blob reference has no R2 object"))
            .map(Some)
    }

    fn sweep(&self, referenced: &[String], now_unix: i64) -> Result<Vec<String>, Failure> {
        let rows = self
            .db
            .query(
                "SELECT sha256, put_at FROM contextful_blob ORDER BY sha256",
                vec![],
            )
            .map_err(failure)?
            .rows;
        let mut deleted = Vec::new();
        for row in rows {
            let sha = row
                .get("sha256")
                .and_then(Value::as_str)
                .ok_or_else(|| failure("missing blob digest"))?;
            let at = row
                .get("put_at")
                .and_then(Value::as_i64)
                .ok_or_else(|| failure("missing blob put time"))?;
            let age = u64::try_from(now_unix.saturating_sub(at)).unwrap_or(0);
            if !sweepable(referenced.iter().any(|r| r == sha), age) {
                continue;
            }
            let _guard = self.lock(sha)?;
            let current = self
                .db
                .query(
                    "SELECT put_at FROM contextful_blob WHERE sha256 = ?",
                    vec![json!(sha)],
                )
                .map_err(failure)?;
            let Some(current_at) = current
                .rows
                .first()
                .and_then(|r| r.get("put_at"))
                .and_then(Value::as_i64)
            else {
                continue;
            };
            if !sweepable(
                referenced.iter().any(|r| r == sha),
                u64::try_from(now_unix.saturating_sub(current_at)).unwrap_or(0),
            ) || self.referenced(sha)?
            {
                continue;
            }
            let removed = self
                .db
                .query(
                    "DELETE FROM contextful_blob WHERE sha256 = ? AND put_at = ?",
                    vec![json!(sha), json!(current_at)],
                )
                .map_err(failure)?;
            if removed.changes == 1 {
                self.db.delete_blob(sha).map_err(failure)?;
                deleted.push(sha.to_owned());
            }
        }
        Ok(deleted)
    }

    fn swept_at(&self) -> Result<Option<i64>, Failure> {
        Ok(self
            .db
            .query("SELECT swept_at FROM contextful_sweep WHERE id = 1", vec![])
            .map_err(failure)?
            .rows
            .first()
            .and_then(|r| r.get("swept_at"))
            .and_then(Value::as_i64))
    }

    fn mark_swept(&self, at: i64) -> Result<(), Failure> {
        self.db.query("INSERT INTO contextful_sweep (id, swept_at) VALUES (1, ?) ON CONFLICT(id) DO UPDATE SET swept_at = excluded.swept_at", vec![json!(at)]).map_err(failure)?;
        Ok(())
    }
}
