//! `machine.sqlite` behind the `Catalog` port: lease rows and fences, the cursor
//! compare-and-swap, the pending owner and run rows.

use crate::{at, owner, run_row, SetClock, T0};
use contextful_core::coordinate::{Cas, Catalog, CursorRow, LeaseKey};
use contextful_core::run::record::RunStatus;
use contextful_core::run::RunError;
use contextful_core::store::catalog::MACHINE_CATALOG_FILE;
use contextful_core::store::StoreError;
use contextful_sqlite::MachineCatalog;
use serde_json::json;
use std::sync::Arc;

fn catalog(dir: &tempfile::TempDir, clock: &SetClock) -> MachineCatalog {
    MachineCatalog::open(&dir.path().join(MACHINE_CATALOG_FILE), Arc::new(clock.clone())).unwrap()
}

fn pipeline() -> LeaseKey {
    LeaseKey::Pipeline("feed/filings".into())
}

fn cursor(p: &str) -> CursorRow {
    CursorRow { position: Some(json!(p)), marker_run_id: Some("run-1".into()), marker_committed_at: Some(at(T0)), ..CursorRow::default() }
}

#[test]
fn a_lease_is_taken_once_per_holder_and_expires_on_the_catalogs_clock() {
    let dir = tempfile::tempdir().unwrap();
    let clock = SetClock::new();
    let c = catalog(&dir, &clock);
    let a = c.acquire(&pipeline(), "holder-a", 30).unwrap().unwrap();
    assert_eq!((a.holder.as_str(), a.fence, a.expires_at), ("holder-a", 1, at(T0).plus_secs(30)));
    assert!(c.acquire(&pipeline(), "holder-b", 30).unwrap().is_none(), "a live holder keeps the row");
    assert!(c.lease_holds(&a).unwrap());

    clock.advance(30);
    assert!(!c.lease_holds(&a).unwrap(), "expiry is read on the catalog's clock");
    let b = c.acquire(&pipeline(), "holder-b", 30).unwrap().unwrap();
    assert_eq!(b.fence, 2);
    assert!(c.renew(&a, 30).unwrap().is_none(), "a superseded lease renews nothing");
    let renewed = c.renew(&b, 60).unwrap().unwrap();
    assert_eq!((renewed.fence, renewed.expires_at), (2, at(T0).plus_secs(90)));

    c.release(&b).unwrap();
    let row = c.lease_row(&pipeline()).unwrap();
    assert_eq!((row.holder, row.expires_at, row.fence), (None, None, 2), "release clears the holder and keeps the fence");
    assert_eq!(c.acquire(&pipeline(), "holder-c", 30).unwrap().unwrap().fence, 3);
}

#[test]
fn a_cursor_update_matches_only_the_stored_version_and_the_current_fence() {
    let dir = tempfile::tempdir().unwrap();
    let clock = SetClock::new();
    let c = catalog(&dir, &clock);
    assert_eq!(c.cursor("feed", "filings").unwrap(), CursorRow::default());
    assert_eq!(c.cursor_cas("feed", "filings", 0, cursor("p1"), None).unwrap(), Cas::Applied);
    assert_eq!(c.cursor_cas("feed", "filings", 0, cursor("p2"), None).unwrap(), Cas::VersionMoved);
    let stored = c.cursor("feed", "filings").unwrap();
    assert_eq!((stored.version, stored.position), (1, Some(json!("p1"))));
    assert_eq!(stored.marker_run_id.as_deref(), Some("run-1"));

    let stale = c.acquire(&pipeline(), "a", 30).unwrap().unwrap();
    clock.advance(31);
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
    let dir = tempfile::tempdir().unwrap();
    let clock = SetClock::new();
    let c = catalog(&dir, &clock);
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
fn run_rows_list_by_pipeline_and_a_refused_update_writes_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let clock = SetClock::new();
    let c = catalog(&dir, &clock);
    c.put_run(&run_row("run-2", "feed", RunStatus::Running)).unwrap();
    c.put_run(&run_row("run-1", "feed", RunStatus::Pending)).unwrap();
    c.put_run(&run_row("run-3", "other", RunStatus::Pending)).unwrap();
    assert_eq!(c.run("run-1").unwrap().unwrap().status, RunStatus::Pending);
    assert!(c.run("run-9").unwrap().is_none());
    let feed: Vec<String> = c.runs(Some("feed")).unwrap().into_iter().map(|r| r.run_id).collect();
    assert_eq!(feed, ["run-1", "run-2"]);
    assert_eq!(c.runs(None).unwrap().len(), 3);

    let refused = c.update_run("run-1", &mut |row| {
        row.status = RunStatus::Success;
        Err(RunError::CancelTargetNotInFlight("refused".into()))
    });
    assert!(matches!(refused.unwrap(), Some(Err(_))));
    assert_eq!(c.run("run-1").unwrap().unwrap().status, RunStatus::Pending, "a refused update writes nothing");
    let updated = c.update_run("run-1", &mut |row| {
        row.status = RunStatus::Running;
        Ok(())
    });
    assert_eq!(updated.unwrap().unwrap().unwrap().status, RunStatus::Running);
    assert!(c.update_run("run-9", &mut |_| Ok(())).unwrap().is_none());
}

#[test]
fn rows_outlive_the_connection_that_wrote_them() {
    let dir = tempfile::tempdir().unwrap();
    let clock = SetClock::new();
    {
        let c = catalog(&dir, &clock);
        c.acquire(&pipeline(), "a", 30).unwrap().unwrap();
        c.cursor_cas("feed", "filings", 0, cursor("p1"), None).unwrap();
        c.put_run(&run_row("run-1", "feed", RunStatus::Success)).unwrap();
    }
    let c = catalog(&dir, &clock);
    assert_eq!(c.lease_row(&pipeline()).unwrap().fence, 1);
    assert_eq!(c.cursor("feed", "filings").unwrap().version, 1);
    assert_eq!(c.run("run-1").unwrap().unwrap().status, RunStatus::Success);
}

/// Connections from several threads, each its own `MachineCatalog` on one file, race the
/// compare-and-swap; the file's write lock serializes them, so no update is lost and each
/// acquisition takes a distinct fence.
#[test]
fn connections_racing_on_one_file_lose_no_update() {
    let dir = tempfile::tempdir().unwrap();
    let clock = SetClock::new();
    catalog(&dir, &clock);
    const WRITERS: u64 = 6;
    const EACH: u64 = 15;
    let fences = std::sync::Mutex::new(Vec::new());
    std::thread::scope(|s| {
        for w in 0..WRITERS {
            let (dir, clock, fences) = (&dir, &clock, &fences);
            s.spawn(move || {
                let c = catalog(dir, clock);
                let mut applied = 0;
                while applied < EACH {
                    let seen = c.cursor("feed", "filings").unwrap().version;
                    if c.cursor_cas("feed", "filings", seen, cursor(&format!("w{w}")), None).unwrap() == Cas::Applied {
                        applied += 1;
                    }
                }
                let key = LeaseKey::Partition { pipeline: "feed".into(), partition: format!("p{w}") };
                let lease = c.acquire(&LeaseKey::Compaction("filings".into()), &format!("w{w}"), 0).unwrap();
                if let Some(l) = lease {
                    fences.lock().unwrap().push(l.fence);
                }
                c.acquire(&key, "w", 30).unwrap().unwrap();
            });
        }
    });
    let c = catalog(&dir, &clock);
    assert_eq!(c.cursor("feed", "filings").unwrap().version, WRITERS * EACH);
    let mut fences = fences.into_inner().unwrap();
    let n = fences.len();
    fences.sort_unstable();
    fences.dedup();
    assert_eq!(fences.len(), n, "no fence repeats: {fences:?}");
}
