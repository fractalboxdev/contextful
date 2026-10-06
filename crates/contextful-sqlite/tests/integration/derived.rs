//! `derived.sqlite` behind the `DerivedCatalog` port, and both catalogs reached through
//! the ports `contextful-core` defines.

use crate::{at, SetClock, T0};
use contextful_core::coordinate::{Cas, Catalog, CursorRow, LeaseKey};
use contextful_core::store::catalog::{DerivedCatalog, DerivedRows, DerivedRun, DerivedSnapshot, DerivedTable, DERIVED_CATALOG_FILE, MACHINE_CATALOG_FILE};
use contextful_sqlite::{DerivedSqlite, MachineCatalog};
use std::sync::Arc;

fn rows(tables: &[&str]) -> DerivedRows {
    DerivedRows {
        tables: tables
            .iter()
            .map(|t| DerivedTable { table: t.to_string(), schema: "{\"fields\":[]}".into(), snapshot_id: Some(format!("snapshot-{t}")), fence: Some(3) })
            .collect(),
        snapshots: tables
            .iter()
            .map(|t| DerivedSnapshot {
                table: t.to_string(),
                snapshot_id: format!("snapshot-{t}"),
                parent: None,
                created_at: at("2030-01-01T00:00:00.000000123Z"),
                row_count: 7,
                parts: 2,
            })
            .collect(),
        runs: tables
            .iter()
            .flat_map(|t| {
                ["ingest-b", "ingest-a"].map(|node| DerivedRun {
                    table: t.to_string(),
                    run_id: "run-1".into(),
                    node_id: node.into(),
                    committed_at: at(T0),
                    parts: 1,
                    pipeline_id: Some("feed".into()),
                })
            })
            .collect(),
    }
}

#[test]
fn a_replace_swaps_the_whole_row_set_and_reads_back_in_order() {
    let dir = tempfile::tempdir().unwrap();
    let c = DerivedSqlite::open(&dir.path().join(DERIVED_CATALOG_FILE)).unwrap();
    assert_eq!(c.rows().unwrap(), DerivedRows::default());

    let first = rows(&["filings", "calls"]);
    c.replace(&first).unwrap();
    assert_eq!(c.rows().unwrap(), first.clone().sorted());
    assert_eq!(c.rows().unwrap().runs[0].node_id, "ingest-a", "runs read back by table, run id and node id");

    // A second replace leaves no row of the first behind.
    let second = rows(&["filings"]);
    c.replace(&second).unwrap();
    assert_eq!(c.rows().unwrap(), second.clone().sorted());

    drop(c);
    let reopened = DerivedSqlite::open(&dir.path().join(DERIVED_CATALOG_FILE)).unwrap();
    assert_eq!(reopened.rows().unwrap(), second.sorted(), "the instants keep their nanoseconds across a reopen");
}

#[test]
fn an_ephemeral_derived_catalog_keeps_schema_canaries_off_disk() {
    let dir = tempfile::tempdir().unwrap();
    let catalog = DerivedSqlite::open_ephemeral(&dir.path().join(DERIVED_CATALOG_FILE)).unwrap();
    let mut source = rows(&["filings"]);
    source.tables[0].schema = "schema-canary-5f1e".into();
    catalog.replace(&source).unwrap();
    assert_eq!(catalog.rows().unwrap(), source.sorted());
    assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0);
}

/// The store reaches `machine.sqlite` through {{topology.coordinate.catalog-port}} and `derived.sqlite` through the `DerivedCatalog` port, both traits in `contextful-core`; `contextful-sqlite` implements both, and a host implements either over its own connection.
// spec: store.lay-out.catalog-ports@99e29e98
#[test]
fn both_catalogs_are_reached_through_the_core_ports() {
    let dir = tempfile::tempdir().unwrap();
    let clock = SetClock::new();
    let machine: Arc<dyn Catalog + Send + Sync> =
        Arc::new(MachineCatalog::open(&dir.path().join(MACHINE_CATALOG_FILE), Arc::new(clock.clone())).unwrap());
    let derived: Box<dyn DerivedCatalog + Send + Sync> = Box::new(DerivedSqlite::open(&dir.path().join(DERIVED_CATALOG_FILE)).unwrap());

    let lease = machine.acquire(&LeaseKey::Pipeline("feed".into()), "a", 30).unwrap().unwrap();
    assert_eq!(machine.cursor_cas("feed", "filings", 0, CursorRow::default(), Some(&lease)).unwrap(), Cas::Applied);
    derived.replace(&rows(&["filings"])).unwrap();

    // The two files are separate: rebuilding the derived rows leaves every machine row.
    derived.replace(&DerivedRows::default()).unwrap();
    assert_eq!(machine.cursor("feed", "filings").unwrap().version, 1);
    assert!(machine.lease_holds(&lease).unwrap());
    let names: Vec<String> = std::fs::read_dir(dir.path()).unwrap().map(|e| e.unwrap().file_name().to_string_lossy().into_owned()).collect();
    assert!(names.contains(&DERIVED_CATALOG_FILE.to_string()) && names.contains(&MACHINE_CATALOG_FILE.to_string()), "{names:?}");
}

/// A host's own adapter satisfies the port with no SQLite type in sight.
#[test]
fn a_host_implements_the_derived_port_over_its_own_storage() {
    struct InMemory(std::sync::Mutex<DerivedRows>);
    impl DerivedCatalog for InMemory {
        fn replace(&self, rows: &DerivedRows) -> Result<(), contextful_core::run::Failure> {
            *self.0.lock().unwrap() = rows.clone().sorted();
            Ok(())
        }
        fn rows(&self) -> Result<DerivedRows, contextful_core::run::Failure> {
            Ok(self.0.lock().unwrap().clone())
        }
    }
    let dir = tempfile::tempdir().unwrap();
    let sqlite = DerivedSqlite::open(&dir.path().join(DERIVED_CATALOG_FILE)).unwrap();
    let host = InMemory(Default::default());
    for c in [&sqlite as &dyn DerivedCatalog, &host] {
        c.replace(&rows(&["filings", "calls"])).unwrap();
    }
    assert_eq!(sqlite.rows().unwrap(), host.rows().unwrap());
}
