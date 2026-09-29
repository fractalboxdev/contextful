//! The SQLite adapter's one integration binary: `machine.sqlite` behind the `Catalog`
//! port and the run storage ports, `derived.sqlite` behind the `DerivedCatalog` port.

mod derived;
mod machine;
mod stores;

use contextful_core::ports::Clock;
use contextful_core::run::own::{ConnectorPin, ExecutionOwner, OwnerScope, Pins};
use contextful_core::run::record::{Phase, RunRow, RunStatus};
use contextful_core::time::Instant;
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::Arc;

pub const T0: &str = "2030-01-01T00:00:00Z";

pub fn at(s: &str) -> Instant {
    Instant::parse(s).unwrap_or_else(|e| panic!("{s}: {e}"))
}

/// A clock a test moves by hand.
#[derive(Clone)]
pub struct SetClock(Arc<AtomicI64>);

impl SetClock {
    pub fn new() -> SetClock {
        SetClock(Arc::new(AtomicI64::new(at(T0).unix_secs())))
    }

    pub fn advance(&self, secs: i64) {
        self.0.fetch_add(secs, Ordering::SeqCst);
    }
}

impl Clock for SetClock {
    fn now(&self) -> Instant {
        Instant::from_unix_secs(self.0.load(Ordering::SeqCst)).unwrap()
    }
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
