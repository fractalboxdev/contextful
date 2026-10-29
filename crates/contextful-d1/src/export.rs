//! D1 export state keeps pending identities and checkpoints across container replacement.

use crate::{D1Catalog, D1};
use contextful_core::coordinate::{Catalog, ExecutionAdmission, Lease, LeaseKey};
use contextful_core::export::{
    Change, ChangeEvent, ChangeState, DeliveryMark, DeliveryProgress, ExportCursor, ExportPosition,
    ExportPublication, EXPORT_BATCH_ROWS,
};
use contextful_core::run::{Failure, FailureTag};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
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
    /// The outbox's committed writes and last acknowledgement (`run.export.status-version`).
    #[serde(default)]
    mark: DeliveryMark,
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
            events: vec![],
            pending_source: None,
            acknowledged_publication: None,
            mark: DeliveryMark::default(),
        }
    }
}

pub struct D1ExportLedger {
    db: D1,
    catalog: D1Catalog,
    offered: BTreeMap<String, (u64, u64)>,
    leases: BTreeMap<String, Lease>,
}

fn failure(error: impl std::fmt::Display) -> Failure {
    Failure::new(FailureTag::Storage, format!("D1 export: {error}"))
}

fn fenced() -> Failure {
    Failure::deterministic(
        FailureTag::Permanent,
        "ExecutionWriterFenced: export writer no longer owns its durable lease",
    )
}

impl D1ExportLedger {
    pub fn connect(endpoint: &str) -> Result<Self, Failure> {
        let db = D1::connect(endpoint).map_err(failure)?;
        db.health().map_err(failure)?;
        Ok(Self {
            db,
            catalog: D1Catalog::connect(endpoint)?,
            offered: BTreeMap::new(),
            leases: BTreeMap::new(),
        })
    }

    fn load(&self, name: &str) -> Result<(Option<i64>, State), Failure> {
        let rows = self
            .db
            .query(
                "SELECT version, body FROM contextful_export WHERE name = ?",
                vec![json!(name)],
            )
            .map_err(failure)?
            .rows;
        let Some(row) = rows.first() else {
            return Ok((None, State::default()));
        };
        let version = row
            .get("version")
            .and_then(Value::as_i64)
            .ok_or_else(|| failure("missing export version"))?;
        let body = row
            .get("body")
            .and_then(Value::as_str)
            .ok_or_else(|| failure("missing export body"))?;
        let state: State = serde_json::from_str(body).map_err(failure)?;
        if state
            .position
            .ack_sequence
            .is_some_and(|a| a >= state.position.next_sequence)
        {
            return Err(failure("export sequence counters are corrupt"));
        }
        Ok((Some(version), state))
    }

    fn lease_condition(&self, name: &str, sql: &mut String, args: &mut Vec<Value>) {
        if let Some(lease) = self.leases.get(name) {
            sql.push_str(" AND EXISTS (SELECT 1 FROM contextful_lease WHERE key = ? AND holder = ? AND fence = ? AND expires_us > CAST((julianday('now') - 2440587.5) * 86400000000 AS INTEGER))");
            args.extend([json!(lease.key), json!(lease.holder), json!(lease.fence)]);
        }
    }

    fn edit<T>(
        &self,
        name: &str,
        mut f: impl FnMut(&mut State) -> Result<T, Failure>,
    ) -> Result<T, Failure> {
        for _ in 0..16 {
            if let Some(lease) = self.leases.get(name) {
                if !self.catalog.lease_holds(lease)? {
                    return Err(fenced());
                }
            }
            let (version, mut state) = self.load(name)?;
            let result = f(&mut state)?;
            let body = serde_json::to_string(&state).map_err(failure)?;
            // D1 limits each row to 2 MiB; leave space for the row's key and metadata.
            if body.len() > 1_900_000 {
                return Err(failure(
                    "PublicationStorageUnsupported: D1 outbox row exceeds its storage bound",
                ));
            }
            let (mut sql, mut args) = if let Some(version) = version {
                ("UPDATE contextful_export SET version = version + 1, body = ? WHERE name = ? AND version = ?".to_string(), vec![json!(body), json!(name), json!(version)])
            } else {
                ("INSERT OR IGNORE INTO contextful_export (name, version, body) SELECT ?, 0, ? WHERE 1 = 1".to_string(), vec![json!(name), json!(body)])
            };
            self.lease_condition(name, &mut sql, &mut args);
            if self.db.query(&sql, args).map_err(failure)?.changes == 1 {
                return Ok(result);
            }
        }
        Err(Failure::new(
            FailureTag::Transient,
            "D1 export state changed repeatedly or writer lease moved",
        ))
    }

    pub fn claim(&mut self, name: &str, holder: &str, ttl_secs: u64) -> Result<(), Failure> {
        let lease = self
            .catalog
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

    pub fn dispatch<T>(&self, name: &str, effect: impl FnOnce() -> T) -> Result<T, Failure> {
        let lease = self.leases.get(name).ok_or_else(fenced)?;
        if self
            .catalog
            .renew(lease, contextful_core::coordinate::CADENCE_LEASE_TTL_SECS)?
            .is_none()
        {
            return Err(fenced());
        }
        let admission = ExecutionAdmission::new(vec![], vec![lease.clone()], true)?;
        let _guard = self.catalog.execution_guard(&admission)?;
        Ok(effect())
    }

    pub fn position(&self, name: &str) -> Result<ExportPosition, Failure> {
        Ok(self.load(name)?.1.position)
    }
    /// The export's progress, read without a lease or delivery (`run.export.status`).
    pub fn progress(&self, name: &str) -> Result<DeliveryProgress, Failure> {
        let state = self.load(name)?.1;
        Ok(state.mark.typed(&state.position))
    }
    pub fn state(&self, name: &str) -> Result<ChangeState, Failure> {
        Ok(self.load(name)?.1.current)
    }
    pub fn checkpoint(&self, name: &str) -> Result<ExportCursor, Failure> {
        Ok(self.load(name)?.1.checkpoint)
    }

    pub fn advance_checkpoint(&mut self, name: &str, cursor: ExportCursor) -> Result<(), Failure> {
        self.edit(name, |state| {
            if cursor < state.checkpoint {
                return Err(failure("export checkpoint cannot move backwards"));
            }
            state.checkpoint = cursor;
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
        let staged = self.edit(name, |state| {
            if state.position.pending_publication.is_some()
                || state.acknowledged_publication.as_deref() == Some(publication.id)
            {
                return Ok(false);
            }
            let first = state.position.next_sequence;
            if events.is_empty()
                || events
                    .last()
                    .is_none_or(|e| !matches!(e.change, Change::PublicationComplete { .. }))
                || events.iter().enumerate().any(|(i, e)| {
                    e.publication != publication.id
                        || first.checked_add(i as u64) != Some(e.sequence)
                })
            {
                return Err(failure(
                    "a staged publication has consecutive events ending in a completion marker",
                ));
            }
            let next_sequence = events
                .last()
                .and_then(|e| e.sequence.checked_add(1))
                .ok_or_else(|| failure("export event sequence is exhausted"))?;
            state.position.pending_publication = Some(publication.id.into());
            state.position.identity = Some(publication.identity.into());
            state.position.destination = Some(publication.destination.into());
            state.position.next_sequence = next_sequence;
            state.pending_source = Some(publication.source.into());
            state.events = events.to_vec();
            state.next = next.clone();
            state.mark.wrote();
            Ok(true)
        })?;
        if staged {
            self.offered.remove(name);
        }
        Ok(staged)
    }

    pub fn pending(&mut self, name: &str, limit: usize) -> Result<Vec<ChangeEvent>, Failure> {
        self.offered.remove(name);
        let state = self.load(name)?.1;
        let first = state
            .position
            .ack_sequence
            .map_or(0, |n| n.saturating_add(1));
        let events: Vec<_> = state
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
            return Err(failure("export pending events have a gap"));
        }
        if let (Some(first), Some(last)) = (events.first(), events.last()) {
            self.offered
                .insert(name.into(), (first.sequence, last.sequence));
        }
        Ok(events)
    }

    pub fn offer(&mut self, name: &str, through: u64) -> Result<(), Failure> {
        let Some((first, last)) = self.offered.get_mut(name) else {
            return Err(failure("export has no offered batch"));
        };
        if through < *first || through > *last {
            return Err(failure("export offer is outside its pending batch"));
        }
        *last = through;
        Ok(())
    }

    pub fn acknowledge(&mut self, name: &str, through: u64) -> Result<(), Failure> {
        let Some(&(first, last)) = self.offered.get(name) else {
            return Err(failure("export has no offered batch"));
        };
        if through != last {
            return Err(failure(
                "export acknowledgement is not its offered frontier",
            ));
        }
        let at = wall_clock().map_err(failure)?;
        self.edit(name, |state| {
            let pending = state
                .position
                .pending_publication
                .as_ref()
                .ok_or_else(|| failure("export has no pending publication"))?;
            if first
                != state
                    .position
                    .ack_sequence
                    .map_or(0, |a| a.saturating_add(1))
                || through < first
                || through - first >= EXPORT_BATCH_ROWS as u64
            {
                return Err(failure("export acknowledgement is outside the next batch"));
            }
            let events: Vec<_> = state
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
                return Err(failure("export acknowledgement skips an event"));
            }
            state.position.ack_sequence = Some(through);
            state.mark.acknowledged(through, at);
            if events
                .last()
                .is_some_and(|e| matches!(e.change, Change::PublicationComplete { .. }))
            {
                state.acknowledged_publication = Some(pending.clone());
                state.position.source_publication = state.pending_source.take();
                state.position.pending_publication = None;
                state.current = std::mem::take(&mut state.next);
                state.events.clear();
            }
            Ok(())
        })?;
        self.offered.remove(name);
        Ok(())
    }
}

impl Drop for D1ExportLedger {
    fn drop(&mut self) {
        for lease in self.leases.values() {
            let _ = self.catalog.release(lease);
        }
    }
}

/// The wall-clock instant an acknowledgement records (`run.export.status-version`).
fn wall_clock() -> Result<contextful_core::time::Instant, String> {
    let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_err(|e| e.to_string())?.as_nanos();
    contextful_core::time::Instant::from_unix_nanos(i128::try_from(nanos).map_err(|e| e.to_string())?).map_err(|e| e.to_string())
}
