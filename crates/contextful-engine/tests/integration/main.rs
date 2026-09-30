//! The engine's one integration binary, one module per operation.

mod cancel;
mod command;
mod coordinate;
mod drive;
mod execution;
mod guard;
mod journal;
mod project;
mod writers;
mod runner;
mod stores;
mod support;
mod suspend;

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
        error_kind: None,
        error_message: None,
        connector_id: "vendor".into(),
        connector_version: "1".into(),
        connector_hash: "h".into(),
        trace_id: None,
        phase: Phase::Plan,
        execution_id: "x-1".into(),
        stop: None,
        host_scope: None,
        input: None,
    }
}
