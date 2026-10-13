//! Transactional remote outboxes retain stable event identities across replacement.

use crate::{from_text, to_text, Fail, PgCatalog};
use contextful_core::coordinate::{Catalog, Lease, LeaseKey};
use contextful_core::export::{
    Change, ChangeEvent, ChangeState, ExportCursor, ExportPosition, ExportPublication,
    EXPORT_BATCH_ROWS,
};
use contextful_core::run::{Failure, FailureTag};
use postgres::Transaction;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Serialize, Deserialize)]
struct State {
    #[serde(default)]
    checkpoint: ExportCursor,
    position: ExportPosition,
    current: ChangeState,
    next: ChangeState,
    events: Vec<ChangeEvent>,
    pending_source: Option<String>,
    acknowledged_publication: Option<String>,
}

impl Default for State {
    fn default() -> Self {
        Self {
            checkpoint: ExportCursor::default(),
            position: ExportPosition {
                source_publication: None,
                pending_publication: None,
                identity: None,
                destination: None,
                ack_sequence: None,
                next_sequence: 0,
            },
            current: ChangeState::new(),
            next: ChangeState::new(),
            events: Vec::new(),
            pending_source: None,
            acknowledged_publication: None,
        }
    }
}

/// A PostgreSQL outbox with process-local offered frontiers and an optional writer lease.
pub struct PgExportLedger {
    db: PgCatalog,
    offered: BTreeMap<String, (u64, u64)>,
    leases: BTreeMap<String, Lease>,
}

fn fenced(tx: &mut Transaction, lease: Option<&Lease>, fail: Fail) -> Result<(), Failure> {
    if let Some(lease) = lease {
        let row = crate::lease_row(tx, &lease.key, true, fail)?;
        let now: i64 = tx
            .query_one(&format!("SELECT {}", crate::NOW_US), &[])
            .map_err(|e| fail(&e))?
            .get(0);
        if !row.held_by(lease, crate::instant_us(now, fail)?) {
            return Err(Failure::deterministic(
                FailureTag::Permanent,
                "ExecutionWriterFenced: export writer no longer owns its durable lease",
            ));
        }
    }
    Ok(())
}

fn load(tx: &mut Transaction, name: &str, fail: Fail) -> Result<State, Failure> {
    let state: State = tx
        .query_opt(
            "SELECT body FROM contextful_export WHERE name = $1",
            &[&name],
        )
        .map_err(|e| fail(&e))?
        .map(|r| from_text(r.get(0), fail))
        .transpose()?
        .unwrap_or_default();
    if state
        .position
        .ack_sequence
        .is_some_and(|a| a >= state.position.next_sequence)
    {
        return Err(fail(&"export sequence counters are corrupt"));
    }
    Ok(state)
}

impl PgExportLedger {
    pub fn connect(conninfo: &str) -> Result<Self, Failure> {
        let db = PgCatalog::connect(conninfo)?;
        crate::stores::schema(&db)?;
        db.with(|tx, fail| {
            tx.execute("SELECT pg_advisory_xact_lock($1)", &[&crate::SCHEMA_LOCK]).map_err(|e| fail(&e))?;
            tx.batch_execute("CREATE TABLE IF NOT EXISTS contextful_export (name TEXT PRIMARY KEY, body TEXT NOT NULL)").map_err(|e| fail(&e))
        })?;
        Ok(Self {
            db,
            offered: BTreeMap::new(),
            leases: BTreeMap::new(),
        })
    }

    /// Acquire a remote export writer; every mutation and dispatch checks its fence.
    pub fn claim(&mut self, name: &str, holder: &str, ttl_secs: u64) -> Result<(), Failure> {
        let lease = self
            .db
            .acquire(
                &LeaseKey::Cadence(format!("export:{name}")),
                holder,
                ttl_secs,
            )?
            .ok_or_else(|| {
                Failure::new(
                    FailureTag::Transient,
                    format!("export `{name}` has an active durable writer"),
                )
            })?;
        self.leases.insert(name.into(), lease);
        Ok(())
    }

    /// The lease row stays locked through the bounded external dispatch; takeover waits.
    pub fn dispatch<T>(&self, name: &str, effect: impl FnOnce() -> T) -> Result<T, Failure> {
        let lease = self.leases.get(name).ok_or_else(|| {
            Failure::deterministic(
                FailureTag::Permanent,
                "ExecutionWriterFenced: export dispatch requires a durable writer lease",
            )
        })?;
        if self
            .db
            .renew(lease, contextful_core::coordinate::CADENCE_LEASE_TTL_SECS)?
            .is_none()
        {
            return Err(Failure::deterministic(
                FailureTag::Permanent,
                "ExecutionWriterFenced: export writer lease transferred",
            ));
        }
        self.db.with(|tx, fail| {
            fenced(tx, Some(lease), fail)?;
            Ok(effect())
        })
    }

    fn edit<T>(
        &self,
        name: &str,
        f: impl FnOnce(&mut State, Fail) -> Result<T, Failure>,
    ) -> Result<T, Failure> {
        self.db.with(|tx, fail| {
            fenced(tx, self.leases.get(name), fail)?;
            let lock = format!("export:{name}");
            tx.execute("SELECT pg_advisory_xact_lock(hashtextextended($1, 0))", &[&lock]).map_err(|e| fail(&e))?;
            let mut state = load(tx, name, fail)?;
            let result = f(&mut state, fail)?;
            let body = to_text(&state, fail)?;
            tx.execute("INSERT INTO contextful_export (name, body) VALUES ($1, $2) ON CONFLICT (name) DO UPDATE SET body = excluded.body", &[&name, &body]).map_err(|e| fail(&e))?;
            Ok(result)
        })
    }

    pub fn position(&self, name: &str) -> Result<ExportPosition, Failure> {
        self.db.with(|tx, fail| Ok(load(tx, name, fail)?.position))
    }
    pub fn state(&self, name: &str) -> Result<ChangeState, Failure> {
        self.db.with(|tx, fail| Ok(load(tx, name, fail)?.current))
    }

    pub fn checkpoint(&self, name: &str) -> Result<ExportCursor, Failure> {
        self.db
            .with(|tx, fail| Ok(load(tx, name, fail)?.checkpoint))
    }

    pub fn advance_checkpoint(&mut self, name: &str, cursor: ExportCursor) -> Result<(), Failure> {
        self.edit(name, |s, fail| {
            if cursor < s.checkpoint {
                return Err(fail(&"export checkpoint cannot move backwards"));
            }
            s.checkpoint = cursor;
            Ok(())
        })
    }

    pub fn stage(
        &mut self,
        name: &str,
        publication: ExportPublication<'_>,
        events: &[ChangeEvent],
        next: &ChangeState,
    ) -> Result<bool, Failure> {
        let staged = self.edit(name, |s, fail| {
            if s.position.pending_publication.is_some()
                || s.acknowledged_publication.as_deref() == Some(publication.id)
            {
                return Ok(false);
            }
            let first = s.position.next_sequence;
            if events.is_empty()
                || events
                    .last()
                    .is_none_or(|e| !matches!(e.change, Change::PublicationComplete { .. }))
                || events.iter().enumerate().any(|(i, e)| {
                    e.publication != publication.id
                        || first.checked_add(i as u64) != Some(e.sequence)
                })
            {
                return Err(fail(
                    &"a staged publication has consecutive events ending in a completion marker",
                ));
            }
            let next_sequence = events
                .last()
                .and_then(|e| e.sequence.checked_add(1))
                .ok_or_else(|| fail(&"export event sequence is exhausted"))?;
            s.position.pending_publication = Some(publication.id.into());
            s.position.identity = Some(publication.identity.into());
            s.position.destination = Some(publication.destination.into());
            s.position.next_sequence = next_sequence;
            s.pending_source = Some(publication.source.into());
            s.events = events.to_vec();
            s.next = next.clone();
            Ok(true)
        })?;
        if staged {
            self.offered.remove(name);
        }
        Ok(staged)
    }

    pub fn pending(&mut self, name: &str, limit: usize) -> Result<Vec<ChangeEvent>, Failure> {
        self.offered.remove(name);
        let events = self.db.with(|tx, fail| {
            let s = load(tx, name, fail)?;
            let first = s.position.ack_sequence.map_or(0, |n| n.saturating_add(1));
            let events: Vec<_> = s
                .events
                .into_iter()
                .filter(|e| e.sequence >= first)
                .take(limit)
                .collect();
            if events
                .iter()
                .enumerate()
                .any(|(i, e)| first.checked_add(i as u64) != Some(e.sequence))
            {
                return Err(fail(&"export pending events have a gap"));
            }
            Ok(events)
        })?;
        if let (Some(first), Some(last)) = (events.first(), events.last()) {
            self.offered
                .insert(name.into(), (first.sequence, last.sequence));
        }
        Ok(events)
    }

    pub fn offer(&mut self, name: &str, through: u64) -> Result<(), Failure> {
        let Some((first, last)) = self.offered.get_mut(name) else {
            return Err(crate::storage(
                self.db.name(),
                "export has no offered batch",
            ));
        };
        if through < *first || through > *last {
            return Err(crate::storage(
                self.db.name(),
                "export offer is outside its pending batch",
            ));
        }
        *last = through;
        Ok(())
    }

    pub fn acknowledge(&mut self, name: &str, through: u64) -> Result<(), Failure> {
        let Some(&(first, last)) = self.offered.get(name) else {
            return Err(crate::storage(
                self.db.name(),
                "export has no offered batch",
            ));
        };
        if through != last {
            return Err(crate::storage(
                self.db.name(),
                "export acknowledgement is not its offered frontier",
            ));
        }
        self.edit(name, |s, fail| {
            let pending = s
                .position
                .pending_publication
                .as_ref()
                .ok_or_else(|| fail(&"export has no pending publication"))?;
            if first != s.position.ack_sequence.map_or(0, |a| a.saturating_add(1))
                || through < first
                || through - first >= EXPORT_BATCH_ROWS as u64
            {
                return Err(fail(&"export acknowledgement is outside the next batch"));
            }
            let events: Vec<_> = s
                .events
                .iter()
                .filter(|e| e.sequence >= first && e.sequence <= through)
                .collect();
            if events.len() as u64 != through - first + 1
                || events
                    .iter()
                    .enumerate()
                    .any(|(i, e)| e.sequence != first + i as u64)
            {
                return Err(fail(&"export acknowledgement skips an event"));
            }
            s.position.ack_sequence = Some(through);
            if events
                .last()
                .is_some_and(|e| matches!(e.change, Change::PublicationComplete { .. }))
            {
                s.acknowledged_publication = Some(pending.clone());
                s.position.source_publication = s.pending_source.take();
                s.position.pending_publication = None;
                s.current = std::mem::take(&mut s.next);
                s.events.clear();
            }
            Ok(())
        })?;
        self.offered.remove(name);
        Ok(())
    }
}

impl Drop for PgExportLedger {
    fn drop(&mut self) {
        for lease in self.leases.values() {
            let _ = self.db.release(lease);
        }
    }
}
