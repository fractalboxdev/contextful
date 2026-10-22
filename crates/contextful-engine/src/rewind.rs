//! `run.own.rewind`: retiring a pending owner an operator or an applied declaration
//! supersedes, with the retirement recorded as one `canceled` run row under its scope.

use crate::runner::{Engine, EngineError};
use contextful_core::run::journal::sha256_hex;
use contextful_core::run::own::{ExecutionOwner, OwnerPins, OwnerScope};
use contextful_core::run::ports::{BlobStore, JournalStore};
use contextful_core::run::record::{Phase, RunRow, RunStatus};
use contextful_core::run::suspend::AwakeableState;
use contextful_core::run::{Failure, FailureTag, RunError};
use contextful_core::coordinate::Cas;

impl<J: JournalStore, B: BlobStore> Engine<J, B> {
    /// The pending owner holding `execution_id` and every run row naming it, found through
    /// the rows its attempts and refused opens wrote. A chunk owner is never found.
    fn pending_execution(&self, execution_id: &str) -> Result<Option<(ExecutionOwner, Vec<RunRow>)>, Failure> {
        let rows: Vec<RunRow> = self.catalog.runs(None)?.into_iter().filter(|r| r.execution_id == execution_id).collect();
        let mut scopes: Vec<OwnerScope> = Vec::new();
        for row in &rows {
            let scope = match &row.host_scope {
                Some(id) => OwnerScope::host(id),
                None => OwnerScope::table(&row.pipeline_id, &row.table),
            };
            if !scopes.contains(&scope) {
                scopes.push(scope);
            }
        }
        for scope in scopes {
            if let Some(owner) = self.catalog.owner_at(&scope)?.filter(|o| o.execution_id == execution_id) {
                return Ok(Some((owner, rows)));
            }
        }
        Ok(None)
    }

    /// Retire the pending owner holding `execution_id` (`run.own.rewind`). Without `force`,
    /// an attempt or refused open under the execution must have asked another plan
    /// reference (`run.own.rewind-plan-current`); `force` never overrides
    /// `run.own.rewind-unsafe`. Answers the retirement's run row.
    pub fn rewind(&self, execution_id: &str, reason: &str, force: bool) -> Result<RunRow, EngineError> {
        let Some((owner, rows)) = self.pending_execution(execution_id)? else {
            return Err(RunError::ExecutionRewindNotPending(format!("no pending owner holds execution `{execution_id}`; it completed, was retired, or keys a backfill chunk")).into());
        };
        let pinned = owner.pins.plan_ref();
        let asked: Vec<&str> = rows.iter().filter_map(|r| r.plan_ref.as_deref()).filter(|p| *p != pinned).collect();
        if asked.is_empty() && !force {
            return Err(RunError::ExecutionRewindPlanCurrent(format!(
                "no attempt under execution `{execution_id}` of {} asked a plan reference other than its pinned {pinned}, so its next fire resumes it; pass `--force` to retire it anyway",
                owner.scope
            ))
            .into());
        }
        self.retire_pending(&owner, reason)
    }

    /// Retire the pending owner of `scope` when `superseded` holds for its pins
    /// (`run.own.apply-retires-superseded`); `None` when no owner is pending or its pins
    /// still hold.
    pub fn retire_superseded(&self, scope: &OwnerScope, superseded: &dyn Fn(&OwnerPins) -> bool, reason: &str) -> Result<Option<RunRow>, EngineError> {
        match self.catalog.owner_at(scope)? {
            Some(owner) if superseded(&owner.pins) => self.retire_pending(&owner, reason).map(Some),
            _ => Ok(None),
        }
    }

    /// Refuse an owner a live attempt, a waiting attempt or a pending awakeable holds
    /// (`run.own.rewind-unsafe`); otherwise retire it, leaving the scope's position alone,
    /// and record the retirement (`run.own.rewind-record`).
    fn retire_pending(&self, owner: &ExecutionOwner, reason: &str) -> Result<RunRow, EngineError> {
        let id = &owner.execution_id;
        let now = self.catalog.now()?;
        for attempt in &owner.attempts {
            if self.holder_live(attempt)? {
                return Err(RunError::ExecutionRewindUnsafe(format!("attempt `{attempt}` holds execution `{id}` of {} on an unexpired owner lease and may hold its callback guard", owner.scope)).into());
            }
            if self.catalog.run(attempt)?.is_some_and(|r| r.status == RunStatus::Waiting) {
                return Err(RunError::ExecutionRewindUnsafe(format!("attempt `{attempt}` of execution `{id}` is waiting on an external callback; resolve or cancel it first")).into());
            }
        }
        if let Some(store) = &self.awakeables {
            if let Some(a) = store.rows()?.into_iter().find(|a| &a.execution_id == id && a.state == AwakeableState::Pending && now < a.deadline) {
                return Err(RunError::ExecutionRewindUnsafe(format!("awakeable `{}` of execution `{id}` at step `{}` is unresolved until {}", a.token, a.step_label, a.deadline)).into());
            }
        }
        match self.catalog.retire_at(&owner.scope, id, None, None)? {
            Cas::Applied => {}
            Cas::VersionMoved | Cas::Fenced(_) => return Err(Failure::new(FailureTag::Transient, format!("the owner of {} moved while execution `{id}` retired", owner.scope)).into()),
        }
        let (pipeline_id, table) = owner.scope.pipeline_table().unwrap_or_default();
        let site_id = match owner.attempts.last() {
            Some(attempt) => self.catalog.run(attempt)?.map(|r| r.site_id).unwrap_or_default(),
            None => String::new(),
        };
        let pinned = owner.pins.plan_ref();
        // The row spans the owner's life, so a start the cadence reads moves nothing.
        let row = RunRow {
            run_id: format!("rewind-{}", &sha256_hex(format!("{id}:{now}").as_bytes())[..16]),
            pipeline_id: pipeline_id.to_string(),
            table: table.to_string(),
            site_id,
            status: RunStatus::Canceled,
            owner: None,
            started_at: owner.opened_at,
            ended_at: Some(now),
            rows: 0,
            bytes: 0,
            batches: 0,
            skipped: 0,
            fetched: 0,
            kept: 0,
            declined: Default::default(),
            audit: Vec::new(),
            error_kind: Some(FailureTag::Canceled),
            error_message: Some(format!("rewound: {reason}; retired execution `{id}` pinned to plan reference {pinned}")),
            connector_id: String::new(),
            connector_version: String::new(),
            connector_hash: String::new(),
            trace_id: None,
            phase: Phase::Plan,
            pipeline_phase: None,
            execution_id: id.clone(),
            plan_ref: Some(pinned.to_string()),
            stop: None,
            host_scope: owner.scope.host_id().map(str::to_string),
            input: None,
            failed_branches: Vec::new(),
            credential_providers: Default::default(),
            bound_hits: Vec::new(),
            next_attempt_at: None,
            unpushed: None,
        };
        self.catalog.put_run(&row)?;
        Ok(row)
    }
}
