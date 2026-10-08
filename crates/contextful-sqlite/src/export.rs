//! Machine-local typed export state and its durable delivery outbox.

use crate::{stores::Db, storage};
use contextful_core::export::{ChangeEvent, ChangeState};
use contextful_core::run::Failure;
use contextful_core::store::encrypt::FileCipher;
use rusqlite::{params, Connection, OptionalExtension};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS typed_export (
  name TEXT PRIMARY KEY,
  source_publication TEXT,
  pending_publication TEXT,
  ack_sequence INTEGER NOT NULL DEFAULT -1,
  next_sequence INTEGER NOT NULL DEFAULT 0
);
CREATE TABLE IF NOT EXISTS typed_export_event (
  name TEXT NOT NULL,
  sequence INTEGER NOT NULL,
  body TEXT NOT NULL,
  PRIMARY KEY (name, sequence)
);
CREATE TABLE IF NOT EXISTS typed_export_state (
  name TEXT NOT NULL,
  key TEXT NOT NULL,
  row TEXT NOT NULL,
  PRIMARY KEY (name, key)
);
CREATE TABLE IF NOT EXISTS typed_export_next_state (
  name TEXT NOT NULL,
  key TEXT NOT NULL,
  row TEXT NOT NULL,
  PRIMARY KEY (name, key)
);
CREATE TABLE IF NOT EXISTS typed_export_binding (
  name TEXT PRIMARY KEY,
  identity TEXT NOT NULL,
  destination TEXT NOT NULL,
  pending_source TEXT NOT NULL,
  acknowledged_publication TEXT
);
";

/// The delivered frontier and the current or staged publication of one export.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExportPosition {
    pub source_publication: Option<String>,
    pub pending_publication: Option<String>,
    pub identity: Option<String>,
    pub destination: Option<String>,
    pub ack_sequence: Option<u64>,
    pub next_sequence: u64,
}

/// The source frontier and read/delivery identity of one staged publication.
#[derive(Debug, Clone, Copy)]
pub struct ExportPublication<'a> {
    pub source: &'a str,
    pub id: &'a str,
    pub identity: &'a str,
    pub destination: &'a str,
}

pub struct ExportLedger {
    path: PathBuf,
    db: Db,
    offered: BTreeMap<String, (u64, u64)>,
}

impl ExportLedger {
    pub fn open(path: &Path) -> Result<Self, Failure> {
        Ok(Self { path: path.to_path_buf(), db: Db::open_schema(path, SCHEMA)?, offered: BTreeMap::new() })
    }

    /// Open authenticated export state; every transaction reloads under the shared file lock.
    pub fn open_sealed(path: &Path, cipher: Arc<dyn FileCipher>) -> Result<Self, Failure> {
        Ok(Self { path: path.to_path_buf(), db: Db::open_sealed_schema(path, cipher, SCHEMA)?, offered: BTreeMap::new() })
    }

    pub fn position(&self, name: &str) -> Result<ExportPosition, Failure> {
        self.db.with_connection(false, |conn| position(conn, &self.path, name))
    }

    pub fn state(&self, name: &str) -> Result<ChangeState, Failure> {
        self.db.with_connection(false, |conn| {
            let mut stmt = conn.prepare("SELECT key, row FROM typed_export_state WHERE name = ?1 ORDER BY key").map_err(|e| storage(&self.path, e))?;
            let rows = stmt.query_map([name], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))).map_err(|e| storage(&self.path, e))?;
            let mut state = ChangeState::new();
            for row in rows {
                let (key, body) = row.map_err(|e| storage(&self.path, e))?;
                state.insert(key, serde_json::from_str(&body).map_err(|e| storage(&self.path, e))?);
            }
            Ok(state)
        })
    }

    /// Stage a complete publication and its next state in one transaction. A pending
    /// publication remains byte-identical until its completion marker is acknowledged.
    pub fn stage(&mut self, name: &str, publication: ExportPublication<'_>, events: &[ChangeEvent], state: &ChangeState) -> Result<bool, Failure> {
        let path = &self.path;
        let staged = self.db.with_connection(true, |tx| {
            tx.execute("INSERT OR IGNORE INTO typed_export (name) VALUES (?1)", [name]).map_err(|e| storage(path, e))?;
            let (pending, next): (Option<String>, i64) = tx.query_row(
                "SELECT pending_publication, next_sequence FROM typed_export WHERE name = ?1", [name],
                |r| Ok((r.get(0)?, r.get(1)?)),
            ).map_err(|e| storage(path, e))?;
            if pending.is_some() {
                return Ok(false);
            }
            let acknowledged: Option<String> = tx.query_row("SELECT acknowledged_publication FROM typed_export_binding WHERE name = ?1", [name], |r| r.get(0))
                .optional().map_err(|e| storage(path, e))?.flatten();
            if acknowledged.as_deref() == Some(publication.id) {
                return Ok(false);
            }
            if next < 0 || events.is_empty() || events.first().is_none_or(|e| e.sequence != next as u64) ||
                events.last().is_none_or(|e| !matches!(e.change, contextful_core::export::Change::PublicationComplete { .. })) ||
                events.iter().enumerate().any(|(i, e)| e.publication != publication.id || (next as u64).checked_add(i as u64) != Some(e.sequence))
            {
                return Err(storage(path, "a staged publication has consecutive events ending in a completion marker"));
            }
            tx.execute("DELETE FROM typed_export_next_state WHERE name = ?1", [name]).map_err(|e| storage(path, e))?;
            for (key, row) in state {
                let body = serde_json::to_string(row).map_err(|e| storage(path, e))?;
                tx.execute("INSERT INTO typed_export_next_state (name, key, row) VALUES (?1, ?2, ?3)", params![name, key, body]).map_err(|e| storage(path, e))?;
            }
            for event in events {
                let body = serde_json::to_string(event).map_err(|e| storage(path, e))?;
                let sequence = i64::try_from(event.sequence).map_err(|e| storage(path, e))?;
                tx.execute("INSERT INTO typed_export_event (name, sequence, body) VALUES (?1, ?2, ?3)", params![name, sequence, body]).map_err(|e| storage(path, e))?;
            }
            let next_sequence = events.last().expect("checked nonempty").sequence.checked_add(1)
                .and_then(|n| i64::try_from(n).ok())
                .ok_or_else(|| storage(path, "event sequence exceeds machine cursor range"))?;
            tx.execute("UPDATE typed_export SET pending_publication = ?2, next_sequence = ?3 WHERE name = ?1", params![name, publication.id, next_sequence]).map_err(|e| storage(path, e))?;
            tx.execute("INSERT INTO typed_export_binding (name, identity, destination, pending_source) VALUES (?1, ?2, ?3, ?4) ON CONFLICT(name) DO UPDATE SET identity = excluded.identity, destination = excluded.destination, pending_source = excluded.pending_source", params![name, publication.identity, publication.destination, publication.source]).map_err(|e| storage(path, e))?;
            Ok(true)
        })?;
        if staged { self.offered.remove(name); }
        Ok(staged)
    }

    /// Return a consecutive prefix and remember its frontier for this process. A restart
    /// must read the outbox again before acknowledging a delivery.
    pub fn pending(&mut self, name: &str, limit: usize) -> Result<Vec<ChangeEvent>, Failure> {
        self.offered.remove(name);
        let events = self.db.with_connection(false, |conn| {
            let mut stmt = conn.prepare(
                "SELECT body FROM typed_export_event WHERE name = ?1 AND sequence > COALESCE((SELECT ack_sequence FROM typed_export WHERE name = ?1), -1) ORDER BY sequence LIMIT ?2"
            ).map_err(|e| storage(&self.path, e))?;
            let rows = stmt.query_map(params![name, limit as i64], |r| r.get::<_, String>(0)).map_err(|e| storage(&self.path, e))?;
            let events: Vec<ChangeEvent> = rows.map(|r| {
                let body = r.map_err(|e| storage(&self.path, e))?;
                serde_json::from_str(&body).map_err(|e| storage(&self.path, e))
        }).collect::<Result<_, _>>()?;
        let expected = position(conn, &self.path, name)?.ack_sequence.map_or(0, |n| n.saturating_add(1));
        if events.iter().enumerate().any(|(i, e)| expected.checked_add(i as u64) != Some(e.sequence)) {
            return Err(storage(&self.path, format!("export `{name}` has a gap in its pending events")));
        }
        Ok(events)
        })?;
        if let (Some(first), Some(last)) = (events.first(), events.last()) {
            self.offered.insert(name.to_owned(), (first.sequence, last.sequence));
        }
        Ok(events)
    }

    /// Narrow an offered row-count batch to the byte-bounded prefix actually sent.
    pub fn offer(&mut self, name: &str, through: u64) -> Result<(), Failure> {
        let Some((first, last)) = self.offered.get_mut(name) else {
            return Err(storage(&self.path, format!("export `{name}` has no offered batch")));
        };
        if through < *first || through > *last {
            return Err(storage(&self.path, format!("export `{name}` cannot offer an event outside its pending batch")));
        }
        *last = through;
        Ok(())
    }

    /// Advance only over the exact prefix a consumer acknowledged. The staged state
    /// becomes current in the same transaction that acknowledges its marker.
    pub fn acknowledge(&mut self, name: &str, through: u64) -> Result<(), Failure> {
        let path = &self.path;
        let Some(&(offered_first, offered_last)) = self.offered.get(name) else {
            return Err(storage(path, format!("export `{name}` has no offered batch")));
        };
        if through != offered_last {
            return Err(storage(path, format!("export `{name}` acknowledgement is not the offered frontier")));
        }
        let through = i64::try_from(through).map_err(|e| storage(path, e))?;
        self.db.with_connection(true, |tx| {
            let (ack, pending): (i64, Option<String>) = tx.query_row("SELECT ack_sequence, pending_publication FROM typed_export WHERE name = ?1", [name], |r| Ok((r.get(0)?, r.get(1)?)))
                .map_err(|e| storage(path, e))?;
            let Some(publication) = pending else { return Err(storage(path, "no pending publication to acknowledge")) };
            let first: Option<i64> = tx.query_row("SELECT MIN(sequence) FROM typed_export_event WHERE name = ?1 AND sequence > ?2", params![name, ack], |r| r.get(0)).map_err(|e| storage(path, e))?;
            let Some(first) = first else { return Err(storage(path, "no pending events to acknowledge")) };
            if first != ack + 1 || u64::try_from(first).ok() != Some(offered_first) || through < first || through - first >= contextful_core::export::EXPORT_BATCH_ROWS as i64 {
                return Err(storage(path, "acknowledgement is outside the next batch"));
            }
            let count: i64 = tx.query_row("SELECT COUNT(*) FROM typed_export_event WHERE name = ?1 AND sequence BETWEEN ?2 AND ?3", params![name, first, through], |r| r.get(0)).map_err(|e| storage(path, e))?;
            if count != through - first + 1 {
                return Err(storage(path, "acknowledgement skips an event"));
            }
            let body: Option<String> = tx.query_row("SELECT body FROM typed_export_event WHERE name = ?1 AND sequence = ?2", params![name, through], |r| r.get(0)).optional().map_err(|e| storage(path, e))?;
            let Some(body) = body else { return Err(storage(path, "acknowledgement skips an event")) };
            let event: ChangeEvent = serde_json::from_str(&body).map_err(|e| storage(path, e))?;
            tx.execute("UPDATE typed_export SET ack_sequence = ?2 WHERE name = ?1", params![name, through]).map_err(|e| storage(path, e))?;
            if matches!(event.change, contextful_core::export::Change::PublicationComplete { .. }) {
                let source: String = tx.query_row("SELECT pending_source FROM typed_export_binding WHERE name = ?1", [name], |r| r.get(0)).map_err(|e| storage(path, e))?;
                tx.execute("DELETE FROM typed_export_state WHERE name = ?1", [name]).map_err(|e| storage(path, e))?;
                tx.execute("INSERT INTO typed_export_state SELECT name, key, row FROM typed_export_next_state WHERE name = ?1", [name]).map_err(|e| storage(path, e))?;
                tx.execute("DELETE FROM typed_export_next_state WHERE name = ?1", [name]).map_err(|e| storage(path, e))?;
                tx.execute("DELETE FROM typed_export_event WHERE name = ?1", [name]).map_err(|e| storage(path, e))?;
                tx.execute("UPDATE typed_export_binding SET acknowledged_publication = ?2 WHERE name = ?1", params![name, publication]).map_err(|e| storage(path, e))?;
                tx.execute("UPDATE typed_export SET source_publication = ?2, pending_publication = NULL WHERE name = ?1", params![name, source]).map_err(|e| storage(path, e))?;
            }
            Ok(())
        })?;
        self.offered.remove(name);
        Ok(())
    }
}

fn position(conn: &Connection, path: &Path, name: &str) -> Result<ExportPosition, Failure> {
    let row = conn.query_row(
        "SELECT source_publication, pending_publication, ack_sequence, next_sequence FROM typed_export WHERE name = ?1",
        [name],
        |r| Ok((r.get::<_, Option<String>>(0)?, r.get::<_, Option<String>>(1)?, r.get::<_, i64>(2)?, r.get::<_, i64>(3)?)),
    ).optional().map_err(|e| storage(path, e))?;
    let Some((source_publication, pending_publication, ack, next)) = row else {
        return Ok(ExportPosition { source_publication: None, pending_publication: None, identity: None, destination: None, ack_sequence: None, next_sequence: 0 });
    };
    let binding = conn.query_row("SELECT identity, destination FROM typed_export_binding WHERE name = ?1", [name], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))
        .optional().map_err(|e| storage(path, e))?;
    if ack < -1 || next < 0 || ack >= next {
        return Err(storage(path, format!("export `{name}` has corrupt sequence counters: ack {ack}, next {next}")));
    }
    let next_sequence = u64::try_from(next).map_err(|e| storage(path, e))?;
    let ack_sequence = (ack >= 0).then(|| u64::try_from(ack)).transpose().map_err(|e| storage(path, e))?;
    let (identity, destination) = binding.map_or((None, None), |(i, d)| (Some(i), Some(d)));
    Ok(ExportPosition { source_publication, pending_publication, identity, destination, ack_sequence, next_sequence })
}
