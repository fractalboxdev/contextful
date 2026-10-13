//! D1 journal claims and recorded values use conditional SQL writes.

use crate::D1;
use contextful_core::run::journal::{EntryKey, Row, Stored};
use contextful_core::run::ports::JournalStore;
use contextful_core::run::{Failure, FailureTag};
use serde_json::{json, Value};

pub struct D1JournalStore {
    db: D1,
}

fn failure(error: impl std::fmt::Display) -> Failure {
    Failure::new(FailureTag::Storage, format!("D1 journal: {error}"))
}

fn params(key: &EntryKey) -> Vec<Value> {
    vec![
        json!(key.execution_id),
        json!(key.step_label),
        json!(key.input_hash),
    ]
}

impl D1JournalStore {
    pub fn connect(endpoint: &str) -> Result<Self, Failure> {
        let db = D1::connect(endpoint).map_err(failure)?;
        db.health().map_err(failure)?;
        Ok(Self { db })
    }

    fn read_row(&self, key: &EntryKey) -> Result<Option<Row>, Failure> {
        self.db.query("SELECT body FROM contextful_journal WHERE execution_id = ? AND step_label = ? AND input_hash = ?", params(key))
            .map_err(failure)?.rows.into_iter().next()
            .map(|row| serde_json::from_str(row.get("body").and_then(Value::as_str).ok_or_else(|| failure("missing journal body"))?).map_err(failure))
            .transpose()
    }

    fn insert_pending(&self, key: &EntryKey, run_id: &str, attempts: u32) -> Result<bool, Failure> {
        let row = Row::Pending {
            key: key.clone(),
            run_id: run_id.into(),
            attempts,
        };
        let mut args = params(key);
        args.extend([
            json!(run_id),
            json!(serde_json::to_string(&row).map_err(failure)?),
        ]);
        Ok(self.db.query("INSERT OR IGNORE INTO contextful_journal (execution_id, step_label, input_hash, kind, holder, body) VALUES (?, ?, ?, 'pending', ?, ?)", args).map_err(failure)?.changes == 1)
    }

    fn replace_pending(&self, key: &EntryKey, old: &Row, new: &Row) -> Result<bool, Failure> {
        let old_holder = match old {
            Row::Pending { run_id, .. } => run_id,
            _ => return Ok(false),
        };
        let new_holder = match new {
            Row::Pending { run_id, .. } => run_id,
            _ => return Ok(false),
        };
        let mut args = vec![
            json!(new_holder),
            json!(serde_json::to_string(new).map_err(failure)?),
        ];
        args.extend(params(key));
        args.extend([
            json!(old_holder),
            json!(serde_json::to_string(old).map_err(failure)?),
        ]);
        Ok(self.db.query("UPDATE contextful_journal SET holder = ?, body = ? WHERE execution_id = ? AND step_label = ? AND input_hash = ? AND kind = 'pending' AND holder = ? AND body = ?", args).map_err(failure)?.changes == 1)
    }
}

impl JournalStore for D1JournalStore {
    fn create_pending(&self, key: &EntryKey, run_id: &str) -> Result<bool, Failure> {
        self.insert_pending(key, run_id, 0)
    }

    fn read(&self, key: &EntryKey) -> Result<Option<Row>, Failure> {
        self.read_row(key)
    }

    fn replace_if_pending(
        &self,
        key: &EntryKey,
        holder: &str,
        run_id: &str,
    ) -> Result<bool, Failure> {
        for _ in 0..16 {
            let Some(old) = self.read_row(key)? else {
                return Ok(false);
            };
            let Row::Pending {
                run_id: current,
                attempts,
                ..
            } = &old
            else {
                return Ok(false);
            };
            if current != holder {
                return Ok(false);
            }
            let new = Row::Pending {
                key: key.clone(),
                run_id: run_id.into(),
                attempts: *attempts,
            };
            if self.replace_pending(key, &old, &new)? {
                return Ok(true);
            }
        }
        Err(Failure::new(
            FailureTag::Transient,
            "D1 journal claim changed repeatedly",
        ))
    }

    fn record(&self, key: &EntryKey, value: &Stored) -> Result<Option<Stored>, Failure> {
        let row = Row::Recorded {
            key: key.clone(),
            value: value.clone(),
        };
        let body = serde_json::to_string(&row).map_err(failure)?;
        let mut args = params(key);
        args.push(json!(body));
        let mut insert = "INSERT OR IGNORE INTO contextful_journal (execution_id, step_label, input_hash, kind, holder, body) SELECT ?, ?, ?, 'recorded', NULL, ?".to_string();
        if let Stored::Blob { sha256, .. } = value {
            insert.push_str(" WHERE EXISTS (SELECT 1 FROM contextful_blob WHERE sha256 = ?) AND NOT EXISTS (SELECT 1 FROM contextful_guard WHERE scope = ?)");
            args.extend([json!(sha256), json!(format!("blob:{sha256}"))]);
        }
        if self.db.query(&insert, args).map_err(failure)?.changes == 1 {
            return Ok(None);
        }
        let mut args = vec![json!(body)];
        args.extend(params(key));
        let mut update = "UPDATE contextful_journal SET kind = 'recorded', holder = NULL, body = ? WHERE execution_id = ? AND step_label = ? AND input_hash = ? AND kind = 'pending'".to_string();
        if let Stored::Blob { sha256, .. } = value {
            update.push_str(" AND EXISTS (SELECT 1 FROM contextful_blob WHERE sha256 = ?) AND NOT EXISTS (SELECT 1 FROM contextful_guard WHERE scope = ?)");
            args.extend([json!(sha256), json!(format!("blob:{sha256}"))]);
        }
        if self.db.query(&update, args).map_err(failure)?.changes == 1 {
            return Ok(None);
        }
        match self.read_row(key)? {
            Some(Row::Recorded { value, .. }) => Ok(Some(value)),
            _ => Err(Failure::new(
                FailureTag::Transient,
                "D1 journal record changed concurrently or its R2 blob is unavailable",
            )),
        }
    }

    fn release(&self, key: &EntryKey, run_id: &str) -> Result<(), Failure> {
        let mut args = params(key);
        args.push(json!(run_id));
        self.db.query("DELETE FROM contextful_journal WHERE execution_id = ? AND step_label = ? AND input_hash = ? AND kind = 'pending' AND holder = ?", args).map_err(failure)?;
        Ok(())
    }

    fn note_attempts(&self, key: &EntryKey, run_id: &str, attempts: u32) -> Result<(), Failure> {
        if self.insert_pending(key, run_id, attempts)? {
            return Ok(());
        }
        for _ in 0..16 {
            let Some(old) = self.read_row(key)? else {
                return Ok(());
            };
            let Row::Pending {
                run_id: current, ..
            } = &old
            else {
                return Ok(());
            };
            if current != run_id {
                return Ok(());
            }
            let new = Row::Pending {
                key: key.clone(),
                run_id: run_id.into(),
                attempts,
            };
            if self.replace_pending(key, &old, &new)? {
                return Ok(());
            }
        }
        Err(Failure::new(
            FailureTag::Transient,
            "D1 journal attempts changed repeatedly",
        ))
    }

    fn rows(&self, execution_id: &str) -> Result<Vec<Row>, Failure> {
        self.db.query("SELECT body FROM contextful_journal WHERE execution_id = ? ORDER BY step_label, input_hash", vec![json!(execution_id)]).map_err(failure)?
            .rows.into_iter().map(|row| serde_json::from_str(row.get("body").and_then(Value::as_str).ok_or_else(|| failure("missing journal body"))?).map_err(failure)).collect()
    }

    fn retire(&self, execution_id: &str) -> Result<(), Failure> {
        self.db
            .query(
                "DELETE FROM contextful_journal WHERE execution_id = ?",
                vec![json!(execution_id)],
            )
            .map_err(failure)?;
        Ok(())
    }

    fn executions(&self) -> Result<Vec<String>, Failure> {
        self.db
            .query(
                "SELECT DISTINCT execution_id FROM contextful_journal ORDER BY execution_id",
                vec![],
            )
            .map_err(failure)?
            .rows
            .into_iter()
            .map(|row| {
                row.get("execution_id")
                    .and_then(Value::as_str)
                    .map(str::to_owned)
                    .ok_or_else(|| failure("missing execution id"))
            })
            .collect()
    }
}
