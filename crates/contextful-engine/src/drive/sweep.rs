//! `run.sweep`: one execution under `job:<name>` freezes the sweep's `as_of` and runs a
//! store-driven execution per origin, each under its own scope and execution id, reading at
//! `available_as_of` equal to its origin.

use super::{close_of, job_scope, Drive};
use crate::execution::{Close, Execution, Tally};
use crate::runner::{Engine, EngineError};
use contextful_core::run::drive::{Emitted, InputSet, StoreInput};
use contextful_core::run::journal::{sha256_hex, EntryKey, Row as JournalRow};
use contextful_core::run::own::{OwnerPins, PlanPins};
use contextful_core::run::ports::{BlobStore, JournalStore, Landed, OpenExecution};
use contextful_core::run::record::{RunRow, RunStatus};
use contextful_core::run::sweep::{OriginPin, Sweep, SweepState};
use contextful_core::run::{Failure, FailureTag};
use contextful_core::time::Instant;
use serde_json::json;
use std::collections::BTreeMap;

/// The step recording a sweep's frozen `as_of` (`run.sweep.frozen-as-of`).
pub const SWEEP_OPEN_STEP: &str = "sweep-open";

/// Reads one origin's input set at the frozen `as_of`; under `state = "carried"` the
/// preceding origin's commit instant, at which the job's output tables resolve.
pub type ReadOrigin<'a> = dyn FnMut(&StoreInput, &str, Option<Instant>) -> Result<InputSet, Failure> + 'a;
/// Lands one origin's emitted rows under that origin's run id.
pub type LandOrigin<'a> = dyn FnMut(&StoreInput, &str, &Emitted) -> Result<Landed, Failure> + 'a;

/// One sweep fire: the store-driven fire it repeats per origin, and its declaration.
pub struct SweepFire<'a> {
    pub drive: Drive<'a>,
    pub sweep: Sweep,
}

/// A closed sweep: its own run row, whose execution id is the sweep id, and each origin's
/// run row in origin order.
#[derive(Debug, Clone)]
pub struct Swept {
    pub sweep: RunRow,
    pub origins: Vec<RunRow>,
}

impl SweepFire<'_> {
    /// The sweep's plan reference: the job's input pins beside the declaration.
    fn pins(&self) -> (String, OwnerPins) {
        let declaration = self.sweep.canonical();
        let plan_ref = sha256_hex(json!({ "kind": "sweep", "input": self.drive.input.plan_ref(), "sweep": declaration }).to_string().as_bytes());
        let identities = BTreeMap::from([
            ("body".to_string(), self.drive.input.body.clone()),
            ("statement".to_string(), sha256_hex(self.drive.input.statement.as_bytes())),
            ("as_of".to_string(), self.drive.input.as_of.clone().unwrap_or_else(|| "open".to_string())),
            ("sweep".to_string(), sha256_hex(declaration.as_bytes())),
        ]);
        let mut identities = identities;
        if let Some(identity) = &self.drive.input.structural_identity {
            identities.insert("structural_identity".to_string(), identity.clone());
        }
        (plan_ref.clone(), PlanPins { plan_ref, identities }.into())
    }
}

impl<J: JournalStore + Clone + Send + Sync, B: BlobStore + Clone + Send + Sync> Engine<J, B> {
    /// Run one sweep: every origin it declares, ascending, none collapsed
    /// (`run.sweep.no-collapse`). An origin already recorded by an earlier attempt of the
    /// same sweep answers from the journal and does not run again.
    pub fn sweep(&self, fire: &SweepFire<'_>, read: &mut ReadOrigin<'_>, land: &mut LandOrigin<'_>) -> Result<Swept, EngineError> {
        let drive = &fire.drive;
        let (plan_ref, pins) = fire.pins();
        let open = OpenExecution {
            scope: job_scope(&drive.job),
            pins,
            run_id: drive.run_id.clone(),
            site_id: drive.site_id.clone(),
            pid: drive.pid,
            boot_id: drive.boot_id.clone(),
            trace_id: None,
            connector: None,
            schedule: drive.schedule.clone(),
        };
        let x = self.open_execution(&open)?;
        let mut origins = Vec::new();
        let outcome = self.sweep_open(&x, fire, &plan_ref, read, land, &mut origins);
        let sweep = x.close_with(outcome)?;
        Ok(Swept { sweep, origins })
    }

    fn sweep_open(
        &self,
        x: &Execution<'_, J, B>,
        fire: &SweepFire<'_>,
        plan_ref: &str,
        read: &mut ReadOrigin<'_>,
        land: &mut LandOrigin<'_>,
        rows: &mut Vec<RunRow>,
    ) -> Result<(Landed, Tally), Close> {
        let drive = &fire.drive;
        // Every origin reads at one `as_of`, recorded once so a resume reads it too.
        let opened_at = match x.opened_at() {
            Some(at) => at,
            None => self.catalog.now()?,
        };
        let key = EntryKey::new(x.execution_id(), SWEEP_OPEN_STEP, plan_ref.as_bytes());
        let recorded = x.step_keyed(&key, &|_| true, &mut |_| Ok(drive.input.as_of.clone().unwrap_or_else(|| opened_at.to_string()).into_bytes()))?;
        let as_of = String::from_utf8_lossy(recorded.bytes()).into_owned();
        let frozen = Instant::parse(&as_of).map_err(|e| Failure::deterministic(FailureTag::Permanent, format!("the recorded sweep `as_of` does not parse: {e}")))?;
        let origins = fire.sweep.origins_at(frozen)?;
        let declaration = fire.sweep.canonical();

        let mut landed = Landed::default();
        let mut previous: Option<Instant> = None;
        let mut failed: Option<Close> = None;
        for (k, origin) in origins.iter().enumerate() {
            let origin = origin.to_rfc3339_nanos();
            let step = EntryKey::new(x.execution_id(), &format!("origin/{origin}"), plan_ref.as_bytes());
            if let Some(JournalRow::Recorded { .. }) = self.journal.row(&step)? {
                let done = x.step_keyed(&step, &|_| true, &mut |_| Err(Failure::deterministic(FailureTag::Permanent, "a recorded origin replays")))?;
                let run_id = String::from_utf8_lossy(done.bytes()).into_owned();
                let row = self.catalog.run(&run_id)?.ok_or_else(|| Failure::new(FailureTag::Storage, format!("origin run `{run_id}` has no row")))?;
                previous = row.ended_at;
                landed.rows += row.rows;
                landed.bytes += row.bytes;
                rows.push(row);
                continue;
            }
            let input = StoreInput {
                as_of: Some(as_of.clone()),
                sweep: Some(OriginPin { declaration: declaration.clone(), sweep_id: x.execution_id().to_string(), origin: origin.clone() }),
                ..drive.input.clone()
            };
            let origin_drive = Drive {
                job: format!("{}@{origin}", drive.job),
                input: input.clone(),
                body: drive.body,
                max_in_flight: drive.max_in_flight,
                run_id: format!("{}.o{k:03}", drive.run_id),
                site_id: drive.site_id.clone(),
                pid: drive.pid,
                boot_id: drive.boot_id.clone(),
                schedule: drive.schedule.clone(),
                poll: drive.poll,
            };
            let carried = match fire.sweep.state {
                SweepState::Carried => previous,
                SweepState::Fresh => None,
            };
            let run_id = origin_drive.run_id.clone();
            let row = self.drive(&origin_drive, &mut |a| read(&input, a, carried), &mut |emitted| land(&input, &run_id, emitted)).map_err(close_of)?;
            rows.push(row.clone());
            if row.status == RunStatus::Success {
                x.step_keyed(&step, &|_| true, &mut |_| Ok(row.run_id.clone().into_bytes()))?;
                previous = row.ended_at;
                landed.rows += row.rows;
                landed.bytes += row.bytes;
                continue;
            }
            let close = Close::Failed(Failure::new(
                row.error_kind.unwrap_or(FailureTag::Permanent),
                format!("origin {origin} closed {} in run `{}`: {}", row.status, row.run_id, row.error_message.clone().unwrap_or_default()),
            ));
            // A failed carried origin leaves every later origin unopened (`run.sweep.no-collapse`).
            if fire.sweep.state == SweepState::Carried {
                return Err(close);
            }
            failed.get_or_insert(close);
        }
        match failed {
            Some(close) => Err(close),
            None => Ok((landed, Tally { batches: rows.len() as u64, ..Tally::default() })),
        }
    }
}
