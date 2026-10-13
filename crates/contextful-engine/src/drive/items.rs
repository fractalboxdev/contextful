//! Independent item execution shares the journal, owner keeper and landing runner.

use super::{job_scope, BodyOwner, Calls, Drive, Output, PreparedCalls, ReadInput};
use crate::execution::{Close, Tally};
use crate::runner::{Engine, EngineError};
pub use contextful_core::run::drive::ItemPolicy;
use contextful_core::run::drive::{
    Emitted, InputRow, InputSet, PreparedEmitted, RowStop, INPUT_STEP,
};
use contextful_core::run::effect::{
    EffectAdmission, EffectScope, PreparedEmission, RecordedBodyPlan,
};
use contextful_core::run::journal::EntryKey;
use contextful_core::run::own::{OwnerPins, OwnerScope, PlanPins};
use contextful_core::run::ports::{BlobStore, JournalStore, Landed, OpenExecution};
use contextful_core::run::record::RunStatus;
use contextful_core::run::{Failure, FailureTag};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Mutex;

/// Durable independent outcome; overlap remains due for the next scheduler fire.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ItemStatus {
    Succeeded(Landed),
    Failed(Failure),
    Parked,
    Deferred,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ItemOutcome {
    pub logical_id: String,
    /// Content identity of the item, its input and executable plan, independent of order.
    pub execution_id: String,
    pub run_id: Option<String>,
    pub status: ItemStatus,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ItemReport {
    pub items: Vec<ItemOutcome>,
}
impl ItemReport {
    pub fn succeeded(&self) -> bool {
        self.items
            .iter()
            .all(|i| matches!(i.status, ItemStatus::Succeeded(_)))
    }
}

#[derive(Deserialize, Serialize)]
struct ItemReceipt {
    run_id: String,
    rows: u64,
    bytes: u64,
    failure: Option<Failure>,
}

fn receipt_outcome(
    logical_id: &str,
    id: &str,
    value: serde_json::Value,
) -> Result<ItemOutcome, Failure> {
    let receipt: ItemReceipt = serde_json::from_value(value)
        .map_err(|e| Failure::new(FailureTag::Storage, e.to_string()))?;
    Ok(ItemOutcome {
        logical_id: logical_id.into(),
        execution_id: id.into(),
        run_id: Some(receipt.run_id),
        status: match receipt.failure {
            Some(f) => ItemStatus::Failed(f),
            None => ItemStatus::Succeeded(Landed {
                rows: receipt.rows,
                bytes: receipt.bytes,
            }),
        },
    })
}

/// The admission key excludes job and version, so neither changes entity exclusion.
pub fn entity_lease_key(
    namespace: &str,
    entity: &serde_json::Value,
) -> contextful_core::coordinate::LeaseKey {
    let bytes = serde_json::to_vec(&("isolated-entity-v1", namespace, entity))
        .expect("entity key serializes");
    contextful_core::coordinate::LeaseKey::Pipeline(format!(
        "entity:{}",
        contextful_core::run::journal::sha256_hex(&bytes)
    ))
}

impl<J: JournalStore + Clone + Send + Sync, B: BlobStore + Clone + Send + Sync> Engine<J, B> {
    /// Run stable items independently. A completed receipt skips its body and landing;
    /// a retry reuses its owner's journal. A held entity returns Deferred for the scheduler.
    pub fn drive_items(
        &self,
        drive: &Drive<'_>,
        policy: &ItemPolicy,
        read: &mut ReadInput<'_>,
        land: &mut dyn FnMut(&str, &Emitted) -> Result<Landed, Failure>,
    ) -> Result<ItemReport, EngineError> {
        self.drive_items_admitted(drive, policy, read, &mut |id, out, _| land(id, out))
    }

    /// Landing through the shared runner retains the item owner's fence to publication.
    pub fn drive_items_admitted(
        &self,
        drive: &Drive<'_>,
        policy: &ItemPolicy,
        read: &mut ReadInput<'_>,
        land: &mut dyn FnMut(&str, &Emitted, &BodyOwner) -> Result<Landed, Failure>,
    ) -> Result<ItemReport, EngineError> {
        self.drive_items_using(
            drive,
            policy,
            drive.input.pins(),
            read,
            land,
            &|row, calls| drive.body.run(row, calls).map(|out| (out, Vec::new())),
        )
    }

    /// Independent items with the registered canonical effect projection.
    pub fn drive_items_recorded(
        &self,
        drive: &Drive<'_>,
        policy: &ItemPolicy,
        plan: &RecordedBodyPlan,
        admission: &dyn EffectAdmission,
        read: &mut ReadInput<'_>,
        land: &mut dyn FnMut(&str, &PreparedEmitted, &BodyOwner) -> Result<Landed, Failure>,
    ) -> Result<ItemReport, EngineError> {
        let body = drive.body.recorded().ok_or_else(|| {
            contextful_core::run::RunError::JournalRedactionConflict(
                "body declares no protected result projection".into(),
            )
        })?;
        let outputs: Vec<_> = plan
            .effects()
            .map(|e| e.table.clone())
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect();
        if RecordedBodyPlan::compile(body.effects(), &outputs)?.identity() != plan.identity() {
            return Err(contextful_core::run::RunError::Invalid(
                "caller projection differs from registered body".into(),
            )
            .into());
        }
        let authorities: Vec<_> = plan
            .effects()
            .map(|e| admission.identity(e).map(|a| (e.label.clone(), a)))
            .collect::<Result<_, _>>()?;
        let identity = contextful_core::run::journal::sha256_hex(
            &serde_json::to_vec(&(
                "prepared-body-owner-v1",
                drive.input.plan_ref(),
                plan.identity(),
                authorities,
            ))
            .map_err(|e| contextful_core::run::RunError::Invalid(e.to_string()))?,
        );
        let pins = PlanPins {
            plan_ref: identity.clone(),
            identities: BTreeMap::from([
                ("input".into(), drive.input.plan_ref()),
                ("body_projection".into(), plan.identity()),
            ]),
        }
        .into();
        self.drive_items_using(drive, policy, pins, read, land, &|row, calls| {
            let prepared = PreparedCalls {
                calls,
                plan,
                admission,
                identity: &identity,
                issued: Mutex::new(Vec::new()),
            };
            let outputs = body.run_recorded(row, &prepared)?;
            let issued = prepared
                .issued
                .into_inner()
                .unwrap_or_else(|e| e.into_inner());
            for (table, emissions) in &outputs {
                for emission in emissions {
                    if emission.table() != table || !issued.contains(emission.scope()) {
                        return Err(RowStop::Failed(Failure::deterministic(
                            FailureTag::Permanent,
                            "body returned an effect outside this item's admitted calls",
                        )));
                    }
                    PreparedEmission::replay(
                        plan,
                        emission.label(),
                        emission.scope(),
                        &emission.encode()?,
                        admission,
                    )?;
                }
            }
            Ok((outputs, issued))
        })
    }

    fn drive_items_using<T: Send, F>(
        &self,
        drive: &Drive<'_>,
        policy: &ItemPolicy,
        pins: OwnerPins,
        read: &mut ReadInput<'_>,
        land: &mut dyn FnMut(&str, &Output<T>, &BodyOwner) -> Result<Landed, Failure>,
        run: &F,
    ) -> Result<ItemReport, EngineError>
    where
        F: Fn(&InputRow, &Calls<'_, '_, J, B>) -> Result<(Output<T>, Vec<EffectScope>), RowStop>
            + Sync,
    {
        policy.validate()?;
        let aggregate = self.open_execution(&OpenExecution {
            scope: job_scope(&drive.job),
            pins: pins.clone(),
            run_id: drive.run_id.clone(),
            site_id: drive.site_id.clone(),
            pid: drive.pid,
            boot_id: drive.boot_id.clone(),
            trace_id: None,
            connector: None,
            schedule: drive.schedule.clone(),
        })?;
        let opened = aggregate
            .opened_at()
            .unwrap_or(self.catalog.now()?)
            .to_string();
        let key = EntryKey::new(
            aggregate.execution_id(),
            INPUT_STEP,
            drive.input.plan_ref().as_bytes(),
        );
        let recorded = match aggregate.step_keyed(&key, &|_| true, &mut |_| {
            read(drive.input.as_of.as_deref().unwrap_or(&opened)).map(|set| set.encode())
        }) {
            Ok(value) => value,
            Err(close) => {
                let error: EngineError = match &close {
                    Close::Failed(f) => f.clone().into(),
                    Close::Refused(e) => e.clone().into(),
                };
                aggregate.close_with(Err(close))?;
                return Err(error);
            }
        };
        let set = InputSet::decode(recorded.bytes())?;
        self.catalog.update_run(aggregate.run_id(), &mut |row| {
            row.input = Some(set.bounds());
            Ok(())
        })?;
        let mut seen = BTreeSet::new();
        let mut rows = Vec::new();
        for row in &set.rows {
            let key = row
                .get(&policy.logical_key)
                .filter(|v| !v.is_null())
                .ok_or_else(|| {
                    contextful_core::run::RunError::Invalid(
                        "isolated item lacks its logical key".into(),
                    )
                })?;
            let entity = row
                .get(&policy.entity_key)
                .filter(|v| !v.is_null())
                .ok_or_else(|| {
                    contextful_core::run::RunError::Invalid(
                        "isolated item lacks its entity key".into(),
                    )
                })?;
            if !seen.insert(key.to_string()) {
                return Err(contextful_core::run::RunError::Invalid(
                    "isolated input repeats a logical key".into(),
                )
                .into());
            }
            let logical_id = contextful_core::run::journal::sha256_hex(
                &serde_json::to_vec(&(&policy.entity_namespace, key))
                    .expect("item identity serializes"),
            );
            let execution_id = contextful_core::run::journal::sha256_hex(
                &serde_json::to_vec(&(
                    "isolated-item-v1",
                    &drive.job,
                    &logical_id,
                    &pins,
                    row,
                    policy,
                ))
                .expect("item execution serializes"),
            );
            rows.push((
                InputRow {
                    key: logical_id.clone(),
                    row: row.clone(),
                },
                execution_id,
                entity.clone(),
            ));
        }
        enum Message<T> {
            Land(
                String,
                Output<T>,
                BodyOwner,
                std::sync::mpsc::SyncSender<Result<Landed, Failure>>,
            ),
            Done(Result<ItemOutcome, EngineError>),
        }
        let (send, receive) = std::sync::mpsc::channel();
        let next = AtomicUsize::new(0);
        let results = std::thread::scope(|threads| {
            for _ in 0..drive.max_in_flight.max(1).min(rows.len()) {
                let send = send.clone();
                let rows = &rows;
                let next = &next;
                let pins = &pins;
                let set = &set;
                let aggregate = &aggregate;
                threads.spawn(move || {
                    while let Some((row, id, entity)) =
                        rows.get(next.fetch_add(1, Ordering::SeqCst))
                    {
                        let result = (|| {
                            let scope = OwnerScope::host(&format!("item:{id}"));
                            if let Some(receipt) = self.catalog.cursor_at(&scope)?.position {
                                return receipt_outcome(&row.key, id, receipt)
                                    .map_err(EngineError::from);
                            }
                            let attempt = format!("{}:item:{}", drive.run_id, id);
                            let lease = match self.catalog.acquire(
                                &entity_lease_key(&policy.entity_namespace, entity),
                                &attempt,
                                contextful_core::run::record::OWNER_LEASE_TTL_SECS,
                            )? {
                                Some(l) => l,
                                None => {
                                    return Ok(ItemOutcome {
                                        logical_id: row.key.clone(),
                                        execution_id: id.clone(),
                                        run_id: None,
                                        status: ItemStatus::Deferred,
                                    })
                                }
                            };
                            if let Some(receipt) = self.catalog.cursor_at(&scope)?.position {
                                self.catalog.release(&lease)?;
                                return receipt_outcome(&row.key, id, receipt)
                                    .map_err(EngineError::from);
                            }
                            let open = OpenExecution {
                                scope: scope.clone(),
                                pins: pins.clone(),
                                run_id: attempt.clone(),
                                site_id: drive.site_id.clone(),
                                pid: drive.pid,
                                boot_id: drive.boot_id.clone(),
                                trace_id: None,
                                connector: None,
                                schedule: drive.schedule.clone(),
                            };
                            let mut x = match self.begin(&open) {
                                Ok(x) => x,
                                Err(e) => {
                                    self.catalog.release(&lease)?;
                                    return Err(e);
                                }
                            };
                            x.hold(lease);
                            x.inherit(aggregate);
                            if let Err(close) = x.claim() {
                                let (tag, message) = close.recorded();
                                x.close_with(Err(close))?;
                                return Ok(ItemOutcome {
                                    logical_id: row.key.clone(),
                                    execution_id: id.clone(),
                                    run_id: Some(attempt),
                                    status: ItemStatus::Failed(Failure::new(tag, message)),
                                });
                            }
                            self.catalog.update_run(x.run_id(), &mut |r| {
                                r.input = Some(set.bounds());
                                Ok(())
                            })?;
                            let calls = Calls {
                                x: &x,
                                key: row.key.clone(),
                                close: Mutex::new(None),
                            };
                            let result = run(row, &calls);
                            let outcome = match result {
                                Ok((output, effects)) => {
                                    let (reply, answer) = std::sync::mpsc::sync_channel(1);
                                    send.send(Message::Land(
                                        id.clone(),
                                        output,
                                        BodyOwner {
                                            scope: scope.clone(),
                                            execution_id: x.execution_id().into(),
                                            run_id: attempt.clone(),
                                            identity: pins.plan_ref().into(),
                                            pins: pins.clone(),
                                            parents: x.parents(),
                                            effects,
                                            fence: x.lease(),
                                        },
                                        reply,
                                    ))
                                    .map_err(|_| {
                                        Failure::new(
                                            FailureTag::Storage,
                                            "item landing channel closed",
                                        )
                                    })?;
                                    answer
                                        .recv()
                                        .map_err(|_| {
                                            Failure::new(
                                                FailureTag::Storage,
                                                "item landing response closed",
                                            )
                                        })?
                                        .map_err(Close::Failed)
                                }
                                Err(RowStop::Parked) => {
                                    drop(calls);
                                    x.park()?;
                                    return Ok(ItemOutcome {
                                        logical_id: row.key.clone(),
                                        execution_id: id.clone(),
                                        run_id: Some(attempt),
                                        status: ItemStatus::Parked,
                                    });
                                }
                                Err(RowStop::Failed(f)) => Err(calls
                                    .close
                                    .lock()
                                    .unwrap_or_else(|e| e.into_inner())
                                    .take()
                                    .unwrap_or(Close::Failed(f))),
                            };
                            drop(calls);
                            let status = match &outcome {
                                Ok(l) => ItemStatus::Succeeded(*l),
                                Err(c) => {
                                    let (tag, message) = c.recorded();
                                    ItemStatus::Failed(Failure::new(tag, message))
                                }
                            };
                            let failures = self
                                .catalog
                                .runs(None)?
                                .iter()
                                .filter(|r| {
                                    r.host_scope.as_deref() == scope.host_id()
                                        && r.status == RunStatus::Failed
                                })
                                .count()
                                + 1;
                            let terminal = match &status {
                                ItemStatus::Succeeded(_) => true,
                                ItemStatus::Failed(f) => {
                                    !matches!(
                                        f.tag,
                                        FailureTag::Transient
                                            | FailureTag::RateLimited
                                            | FailureTag::Storage
                                            | FailureTag::Canceled
                                    ) || failures >= policy.max_attempts as usize
                                }
                                _ => false,
                            };
                            if terminal {
                                x.assert_publication_current()?;
                                let cached = x.cursor_row()?;
                                let receipt = match &status {
                                    ItemStatus::Succeeded(l) => ItemReceipt {
                                        run_id: attempt.clone(),
                                        rows: l.rows,
                                        bytes: l.bytes,
                                        failure: None,
                                    },
                                    ItemStatus::Failed(f) => ItemReceipt {
                                        run_id: attempt.clone(),
                                        rows: 0,
                                        bytes: 0,
                                        failure: Some(f.clone()),
                                    },
                                    _ => unreachable!(),
                                };
                                let next = contextful_core::coordinate::CursorRow {
                                    position: Some(serde_json::to_value(receipt).map_err(|e| {
                                        Failure::new(FailureTag::Storage, e.to_string())
                                    })?),
                                    ..cached.clone()
                                };
                                match x.retire(Some((next, cached.version)), x.lease().as_ref())? {
                                    contextful_core::coordinate::Cas::Applied => {}
                                    _ => {
                                        return Err(Failure::new(
                                            FailureTag::Transient,
                                            "item completion fence changed",
                                        )
                                        .into())
                                    }
                                }
                                self.journal.collect(x.execution_id())?;
                            }
                            x.close_with(outcome.map(|l| (l, Tally::default())))?;
                            Ok(ItemOutcome {
                                logical_id: row.key.clone(),
                                execution_id: id.clone(),
                                run_id: Some(attempt),
                                status,
                            })
                        })();
                        if send.send(Message::Done(result)).is_err() {
                            break;
                        }
                    }
                });
            }
            drop(send);
            let mut results = Vec::new();
            for message in receive {
                match message {
                    Message::Land(id, out, owner, reply) => {
                        let admitted = self
                            .validate_body_owner(&owner)
                            .and_then(|_| land(&id, &out, &owner));
                        let _ = reply.send(admitted);
                    }
                    Message::Done(result) => results.push(result),
                }
            }
            results
        });
        let mut items = match results.into_iter().collect::<Result<Vec<_>, _>>() {
            Ok(items) => items,
            Err(e) => {
                aggregate.close_with(Err(Close::Failed(Failure::new(
                    FailureTag::Storage,
                    e.to_string(),
                ))))?;
                return Err(e);
            }
        };
        items.sort_by(|a, b| a.logical_id.cmp(&b.logical_id));
        let report = ItemReport { items };
        let outcome = if report.succeeded() {
            let landed = report
                .items
                .iter()
                .fold(Landed::default(), |mut total, item| {
                    if let ItemStatus::Succeeded(l) = item.status {
                        total.rows += l.rows;
                        total.bytes += l.bytes;
                    }
                    total
                });
            Ok((landed, Tally::default()))
        } else {
            Err(Close::Failed(Failure::new(
                FailureTag::Transient,
                "isolated items contain failed, parked or deferred outcomes",
            )))
        };
        aggregate.close_with(outcome)?;
        Ok(report)
    }
}
