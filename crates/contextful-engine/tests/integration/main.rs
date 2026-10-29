//! The engine's one integration binary, one module per operation.

mod admission;
mod attempts;
mod backfill;
mod binding;
mod body_identity;
mod cancel;
mod chunk_lease;
#[cfg(unix)]
mod command;
#[cfg(windows)]
mod command_windows;
mod command_admission;
mod coordinate;
mod credentials;
mod dead_letter;
mod drive;
mod items;
mod execution;
mod guarded_callback;
mod guard;
mod journal;
mod journal_model;
mod lateness;
mod ledger;
mod nodes;
mod not_before;
mod project;
mod queue;
mod retire;
mod rewind;
mod writers;
mod runner;
mod scheduler;
mod seed;
mod stages;
mod stores;
mod support;
mod suspend;
mod sweep;

use contextful_core::run::record::{Phase, RunRow, RunStatus};

/// A row of pipeline `feed` in `status`, owned by a live lease.
pub fn support_row(run_id: &str, status: RunStatus) -> RunRow {
    RunRow {
        run_id: run_id.into(),
        pipeline_id: "feed".into(),
        table: "filings".into(),
        site_id: "site-a".into(),
        status,
        owner: Some(contextful_core::run::record::Owner::leased(1, "boot", support::at(support::T0))),
        started_at: support::at(support::T0),
        ended_at: None,
        rows: 0,
        bytes: 0,
        batches: 0,
        skipped: 0,
        fetched: 0,
        kept: 0,
        declined: Default::default(),
        audit: Vec::new(),
        error_kind: None,
        error_message: None,
        connector_id: "vendor".into(),
        connector_version: "1".into(),
        connector_hash: "h".into(),
        trace_id: None,
        phase: Phase::Plan,
        pipeline_phase: None,
        execution_id: "x-1".into(),
        stop: None,
        host_scope: None,
        plan_ref: None,
        input: None,
        failed_branches: Vec::new(),
        credential_providers: Default::default(),
        bound_hits: Vec::new(),
        next_attempt_at: None,
        unpushed: None,
    }
}
mod worker;
