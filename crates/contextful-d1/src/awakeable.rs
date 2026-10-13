//! D1 awakeable rows use a version predicate for concurrent resolutions.

use crate::D1;
use contextful_core::run::journal::{awakeable_key, sha256_hex, EntryKey, Row, Stored};
use contextful_core::run::ports::AwakeableStore;
use contextful_core::run::suspend::{Awakeable, AwakeableState};
use contextful_core::run::{Failure, FailureTag};
use serde_json::{json, Value};

pub struct D1AwakeableStore {
    db: D1,
}

fn failure(error: impl std::fmt::Display) -> Failure {
    Failure::new(FailureTag::Storage, format!("D1 awakeable: {error}"))
}

fn parse(row: &Value) -> Result<(i64, Awakeable), Failure> {
    let version = row
        .get("version")
        .and_then(Value::as_i64)
        .ok_or_else(|| failure("missing awakeable version"))?;
    let body = row
        .get("body")
        .and_then(Value::as_str)
        .ok_or_else(|| failure("missing awakeable body"))?;
    Ok((version, serde_json::from_str(body).map_err(failure)?))
}

impl D1AwakeableStore {
    pub fn connect(endpoint: &str) -> Result<Self, Failure> {
        let db = D1::connect(endpoint).map_err(failure)?;
        db.health().map_err(failure)?;
        Ok(Self { db })
    }

    fn raw(&self, token: &str) -> Result<Option<(i64, Awakeable)>, Failure> {
        self.db
            .query(
                "SELECT version, body FROM contextful_awakeable WHERE token = ?",
                vec![json!(token)],
            )
            .map_err(failure)?
            .rows
            .first()
            .map(parse)
            .transpose()
    }

    fn recovered(&self, mut row: Awakeable) -> Result<Awakeable, Failure> {
        if row.state != AwakeableState::Pending {
            return Ok(row);
        }
        let key = EntryKey {
            execution_id: row.execution_id.clone(),
            step_label: format!("awakeable:{}", row.token),
            input_hash: awakeable_key(&row.token),
        };
        let found = self.db.query("SELECT body FROM contextful_journal WHERE execution_id = ? AND step_label = ? AND input_hash = ?", vec![json!(key.execution_id), json!(key.step_label), json!(key.input_hash)]).map_err(failure)?;
        let Some(body) = found
            .rows
            .first()
            .and_then(|r| r.get("body"))
            .and_then(Value::as_str)
        else {
            return Ok(row);
        };
        if let Row::Recorded { value, .. } = serde_json::from_str::<Row>(body).map_err(failure)? {
            let digest = match &value {
                Stored::Blob { sha256, .. } => sha256.clone(),
                Stored::Inline { .. } => sha256_hex(
                    &value
                        .inline_bytes()
                        .ok_or_else(|| failure("malformed inline callback"))?,
                ),
            };
            row.state = AwakeableState::Resolved;
            row.payload_sha256 = Some(digest);
            row.payload = Some(value);
        }
        Ok(row)
    }
}

impl AwakeableStore for D1AwakeableStore {
    fn insert(&self, row: &Awakeable) -> Result<(), Failure> {
        let body = serde_json::to_string(row).map_err(failure)?;
        self.db.query("INSERT INTO contextful_awakeable (token, version, body) VALUES (?, 0, ?) ON CONFLICT(token) DO UPDATE SET version = version + 1, body = excluded.body", vec![json!(row.token), json!(body)]).map_err(failure)?;
        Ok(())
    }

    fn get(&self, token: &str) -> Result<Option<Awakeable>, Failure> {
        self.raw(token)?
            .map(|(_, row)| self.recovered(row))
            .transpose()
    }

    fn update(
        &self,
        token: &str,
        edit: &mut dyn FnMut(&mut Awakeable) -> Result<bool, Failure>,
    ) -> Result<Option<Awakeable>, Failure> {
        for _ in 0..16 {
            let Some((version, raw)) = self.raw(token)? else {
                return Ok(None);
            };
            let current = self.recovered(raw.clone())?;
            let mut row = current.clone();
            let changed = edit(&mut row)?;
            if !changed && current == raw {
                return Ok(Some(current));
            }
            if !changed {
                row = current
            }
            let body = serde_json::to_string(&row).map_err(failure)?;
            let updated = self.db.query("UPDATE contextful_awakeable SET version = version + 1, body = ? WHERE token = ? AND version = ?", vec![json!(body), json!(token), json!(version)]).map_err(failure)?;
            if updated.changes == 1 {
                return Ok(Some(row));
            }
        }
        Err(Failure::new(
            FailureTag::Transient,
            "D1 awakeable changed repeatedly",
        ))
    }

    fn rows(&self) -> Result<Vec<Awakeable>, Failure> {
        self.db
            .query(
                "SELECT version, body FROM contextful_awakeable ORDER BY token",
                vec![],
            )
            .map_err(failure)?
            .rows
            .iter()
            .map(|row| self.recovered(parse(row)?.1))
            .collect()
    }
}
