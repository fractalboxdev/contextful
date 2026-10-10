//! `PgCatalog` against a live server: lease rows and their fence on the database's clock,
//! the cursor compare-and-swap, owner retirement, run rows, and several daemons' connections
//! racing one database.

use crate::server::{self, Server};
use crate::{at, owner, run_row, T0};
use contextful_core::coordinate::{Cas, Catalog, CursorRow, LeaseKey};
use contextful_core::run::record::RunStatus;
use contextful_core::run::RunError;
use contextful_core::store::StoreError;
use contextful_pg::PgCatalog;
use serde_json::json;

fn catalog(server: &Server) -> PgCatalog {
    PgCatalog::connect(&server.conninfo).unwrap()
}

fn pipeline() -> LeaseKey {
    LeaseKey::Pipeline("feed".into())
}

fn cursor(p: &str) -> CursorRow {
    CursorRow { position: Some(json!(p)), version: 0, marker_run_id: Some("run-1".into()), marker_committed_at: Some(at(T0)) }
}

/// Acquisition takes the fence by one conditional update and evaluates expiry on the
/// database's clock; release clears the holder and keeps the fence.
#[test]
fn a_lease_is_taken_once_per_holder_and_expires_on_the_databases_clock() {
    let Some(server) = server::start() else { return };
    let c = catalog(&server);
    let before = c.now().unwrap();
    let a = c.acquire(&pipeline(), "holder-a", 30).unwrap().unwrap();
    let after = c.now().unwrap();
    assert_eq!((a.holder.as_str(), a.fence), ("holder-a", 1));
    assert!(before.plus_secs(30) <= a.expires_at && a.expires_at <= after.plus_secs(30), "{before} {} {after}", a.expires_at);
    assert!(c.acquire(&pipeline(), "holder-b", 30).unwrap().is_none(), "a live holder keeps the row");
    assert!(c.lease_holds(&a).unwrap());
    c.release(&a).unwrap();

    let expiring = c.acquire(&pipeline(), "holder-a", 0).unwrap().unwrap();
    assert_eq!(expiring.fence, 2);
    assert!(!c.lease_holds(&expiring).unwrap(), "expiry is read on the database's clock");
    let b = c.acquire(&pipeline(), "holder-b", 30).unwrap().unwrap();
    assert_eq!(b.fence, 3);
    assert!(c.renew(&expiring, 30).unwrap().is_none(), "a superseded lease renews nothing");
    let renewed = c.renew(&b, 60).unwrap().unwrap();
    assert_eq!(renewed.fence, 3);
    assert!(renewed.expires_at > b.expires_at);

    c.release(&b).unwrap();
    let row = c.lease_row(&pipeline()).unwrap();
    assert_eq!((row.holder, row.expires_at, row.fence), (None, None, 3), "release clears the holder and keeps the fence");
    assert_eq!(c.acquire(&pipeline(), "holder-c", 30).unwrap().unwrap().fence, 4);
}

#[test]
fn a_cursor_update_matches_only_the_stored_version_and_the_current_fence() {
    let Some(server) = server::start() else { return };
    let c = catalog(&server);
    assert_eq!(c.cursor("feed", "filings").unwrap(), CursorRow::default());
    assert_eq!(c.cursor_cas("feed", "filings", 0, cursor("p1"), None).unwrap(), Cas::Applied);
    assert_eq!(c.cursor_cas("feed", "filings", 0, cursor("p2"), None).unwrap(), Cas::VersionMoved);
    let stored = c.cursor("feed", "filings").unwrap();
    assert_eq!((stored.version, stored.position), (1, Some(json!("p1"))));
    assert_eq!(stored.marker_run_id.as_deref(), Some("run-1"));

    let stale = c.acquire(&pipeline(), "a", 0).unwrap().unwrap();
    let current = c.acquire(&pipeline(), "b", 30).unwrap().unwrap();
    match c.cursor_cas("feed", "filings", 1, cursor("p2"), Some(&stale)).unwrap() {
        Cas::Fenced(StoreError::LeaseFenced(m)) => assert!(m.contains("fence 1") && m.contains("at fence 2"), "{m}"),
        other => panic!("{other:?}"),
    }
    assert_eq!(c.cursor("feed", "filings").unwrap().version, 1, "the fenced commit applied nothing");
    assert_eq!(c.cursor_cas("feed", "filings", 1, cursor("p2"), Some(&current)).unwrap(), Cas::Applied);
    assert_eq!(c.cursor("feed", "filings").unwrap().position, Some(json!("p2")));
}

#[test]
fn retiring_an_owner_caches_its_position_in_the_same_transaction() {
    let Some(server) = server::start() else { return };
    let c = catalog(&server);
    c.put_owner(&owner("x-1")).unwrap();
    assert_eq!(c.owner("feed", "filings").unwrap(), Some(owner("x-1")));

    // A moved version applies nothing: the owner stays and the cursor keeps its row.
    assert_eq!(c.retire("feed", "filings", "x-1", Some((cursor("p1"), 4)), None).unwrap(), Cas::VersionMoved);
    assert_eq!(c.owner("feed", "filings").unwrap(), Some(owner("x-1")));
    assert_eq!(c.cursor("feed", "filings").unwrap().version, 0);

    // Another execution's retirement leaves this owner in place.
    assert_eq!(c.retire("feed", "filings", "x-0", None, None).unwrap(), Cas::Applied);
    assert!(c.owner("feed", "filings").unwrap().is_some());

    assert_eq!(c.retire("feed", "filings", "x-1", Some((cursor("p1"), 0)), None).unwrap(), Cas::Applied);
    assert_eq!(c.owner("feed", "filings").unwrap(), None);
    assert_eq!(c.cursor("feed", "filings").unwrap().position, Some(json!("p1")));
}

#[test]
fn run_rows_list_by_pipeline_in_byte_order_and_a_refused_update_writes_nothing() {
    let Some(server) = server::start() else { return };
    let c = catalog(&server);
    for (id, pipeline, status) in [("run-b", "feed", RunStatus::Running), ("run-a", "feed", RunStatus::Pending), ("run-B", "feed", RunStatus::Pending), ("run-c", "other", RunStatus::Pending)] {
        c.put_run(&run_row(id, pipeline, status)).unwrap();
    }
    assert_eq!(c.run("run-a").unwrap().unwrap().status, RunStatus::Pending);
    assert!(c.run("run-9").unwrap().is_none());
    let feed: Vec<String> = c.runs(Some("feed")).unwrap().into_iter().map(|r| r.run_id).collect();
    assert_eq!(feed, ["run-B", "run-a", "run-b"], "run ids order by bytes, not by the database's collation");
    assert_eq!(c.runs(None).unwrap().len(), 4);

    let refused = c.update_run("run-a", &mut |row| {
        row.status = RunStatus::Success;
        Err(RunError::CancelTargetNotInFlight("refused".into()))
    });
    assert!(matches!(refused.unwrap(), Some(Err(_))));
    assert_eq!(c.run("run-a").unwrap().unwrap().status, RunStatus::Pending, "a refused update writes nothing");
    let updated = c.update_run("run-a", &mut |row| {
        row.status = RunStatus::Running;
        Ok(())
    });
    assert_eq!(updated.unwrap().unwrap().unwrap().status, RunStatus::Running);
    assert!(c.update_run("run-9", &mut |_| Ok(())).unwrap().is_none());
}

#[test]
fn the_newest_run_start_reads_a_pipeline_or_a_host_scope() {
    let Some(server) = server::start() else { return };
    let c = catalog(&server);
    assert_eq!(c.last_run_start("feed").unwrap(), None);
    for (id, pipeline, secs) in [("run-1", "feed", 0), ("run-2", "feed", 7200), ("run-3", "feed", 3600), ("run-4", "other", 9000)] {
        let mut row = run_row(id, pipeline, RunStatus::Success);
        row.started_at = at(T0).plus_secs(secs);
        c.put_run(&row).unwrap();
    }
    // A sub-second start sorts after the whole second before it.
    let mut row = run_row("run-5", "feed", RunStatus::Success);
    row.started_at = at("2030-01-01T02:00:00.5Z");
    c.put_run(&row).unwrap();
    let mut hosted = run_row("run-6", "", RunStatus::Success);
    hosted.host_scope = Some("nightly".into());
    hosted.started_at = at(T0).plus_secs(60);
    c.put_run(&hosted).unwrap();
    assert_eq!(c.last_run_start("feed").unwrap(), Some(row.started_at));
    assert_eq!(c.last_run_start("other").unwrap(), Some(at(T0).plus_secs(9000)));
    assert_eq!(c.last_run_start("nightly").unwrap(), Some(at(T0).plus_secs(60)));
    assert_eq!(c.last_run_start("none").unwrap(), None);
}

#[test]
fn rows_outlive_the_connection_that_wrote_them() {
    let Some(server) = server::start() else { return };
    {
        let c = catalog(&server);
        c.acquire(&pipeline(), "a", 30).unwrap().unwrap();
        c.cursor_cas("feed", "filings", 0, cursor("p1"), None).unwrap();
        c.put_run(&run_row("run-1", "feed", RunStatus::Success)).unwrap();
    }
    let c = catalog(&server);
    assert_eq!(c.lease_row(&pipeline()).unwrap().fence, 1);
    assert_eq!(c.cursor("feed", "filings").unwrap().version, 1);
    assert_eq!(c.run("run-1").unwrap().unwrap().status, RunStatus::Success);
}

/// Daemons sharing one database, each its own `PgCatalog` connection, race the
/// compare-and-swap and the lease acquisition; the database serializes them, so no update
/// is lost, every acquisition takes a distinct fence, and one holder wins a live lease.
#[test]
fn daemons_racing_on_one_database_lose_no_update_and_repeat_no_fence() {
    let Some(server) = server::start() else { return };
    catalog(&server);
    const DAEMONS: u64 = 6;
    const EACH: u64 = 15;
    let fences = std::sync::Mutex::new(Vec::new());
    let winners = std::sync::Mutex::new(Vec::new());
    std::thread::scope(|s| {
        for d in 0..DAEMONS {
            let (server, fences, winners) = (&server, &fences, &winners);
            s.spawn(move || {
                let c = catalog(server);
                if c.acquire(&LeaseKey::Cadence("deploy".into()), &format!("d{d}"), 300).unwrap().is_some() {
                    winners.lock().unwrap().push(d);
                }
                let mut applied = 0;
                while applied < EACH {
                    let seen = c.cursor("feed", "filings").unwrap().version;
                    if c.cursor_cas("feed", "filings", seen, cursor(&format!("d{d}")), None).unwrap() == Cas::Applied {
                        applied += 1;
                    }
                    if let Some(l) = c.acquire(&LeaseKey::Compaction("filings".into()), &format!("d{d}"), 0).unwrap() {
                        fences.lock().unwrap().push(l.fence);
                    }
                }
            });
        }
    });
    let c = catalog(&server);
    assert_eq!(c.cursor("feed", "filings").unwrap().version, DAEMONS * EACH);
    assert_eq!(winners.into_inner().unwrap().len(), 1, "one daemon holds the live cadence lease");
    let mut fences = fences.into_inner().unwrap();
    assert!(!fences.is_empty(), "no daemon acquired the compaction lease");
    let n = fences.len();
    fences.sort_unstable();
    fences.dedup();
    assert_eq!(fences.len(), n, "no fence repeats: {fences:?}");
    assert_eq!(c.lease_row(&LeaseKey::Compaction("filings".into())).unwrap().fence, n as u64);
}
