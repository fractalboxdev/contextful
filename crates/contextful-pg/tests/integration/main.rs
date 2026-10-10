//! The Postgres adapter's one integration binary: `PgCatalog` behind the `Catalog` port.
//!
//! A suite needing a server starts its own cluster from `initdb` and `postgres` on `PATH`
//! in a temporary directory, or connects to the key-value connection string in
//! `CONTEXTFUL_TEST_PG`. A host with neither, or running as root, which Postgres refuses,
//! skips those suites and prints why; the open's refusals run everywhere.

mod catalog;
mod open;
mod server;

use contextful_core::run::own::{ConnectorPin, ExecutionOwner, OwnerScope, Pins};
use contextful_core::run::record::{Phase, RunRow, RunStatus};
use contextful_core::time::Instant;

pub const T0: &str = "2030-01-01T00:00:00Z";

pub fn at(s: &str) -> Instant {
    Instant::parse(s).unwrap_or_else(|e| panic!("{s}: {e}"))
}

/// A run row of pipeline `pipeline` in `status`.
pub fn run_row(run_id: &str, pipeline: &str, status: RunStatus) -> RunRow {
    RunRow {
        run_id: run_id.into(),
        pipeline_id: pipeline.into(),
        table: "filings".into(),
        site_id: "site-a".into(),
        status,
        owner: None,
        started_at: at(T0),
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
        execution_id: "x-1".into(),
        stop: None,
        host_scope: None,
        input: None,
        failed_branches: Vec::new(),
    }
}

/// A pending owner of `feed`/`filings` holding `execution_id`.
pub fn owner(execution_id: &str) -> ExecutionOwner {
    ExecutionOwner {
        execution_id: execution_id.into(),
        scope: OwnerScope::table("feed", "filings"),
        pins: Pins {
            connector: ConnectorPin { id: "vendor".into(), version: "1".into(), world: "native".into(), hash: "h".into() },
            content_hash: "plan-1".into(),
            input_hash: "input-1".into(),
        }
        .into(),
        attempts: vec!["run-1".into()],
        opened_at: at(T0),
    }
}
