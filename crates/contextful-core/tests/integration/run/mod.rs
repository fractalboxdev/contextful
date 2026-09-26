//! The `run` contract's domain, one module per operation.

mod advance;
mod cancel;
mod derive;
mod journal;
mod own;
mod plan;
mod project;
mod record;
mod retry;
mod suspend;

use contextful_core::run::record::{Phase, RunRow, RunStatus};
use contextful_core::time::Instant;

pub fn at(s: &str) -> Instant {
    Instant::parse(s).unwrap_or_else(|e| panic!("{s}: {e}"))
}

/// A running row of pipeline `feed` started at `started`.
pub fn row(run_id: &str, started: &str) -> RunRow {
    RunRow {
        run_id: run_id.into(),
        pipeline_id: "feed".into(),
        table: "filings".into(),
        site_id: "site-a".into(),
        status: RunStatus::Running,
        owner: None,
        started_at: at(started),
        ended_at: None,
        rows: 0,
        bytes: 0,
        batches: 0,
        error_kind: None,
        error_message: None,
        connector_id: "vendor".into(),
        connector_version: "1".into(),
        connector_hash: "h".into(),
        trace_id: None,
        phase: Phase::Plan,
        execution_id: "x-1".into(),
        stop: None,
    }
}
