//! Row-age retention at the read and fold boundaries.

use crate::support::{at, decl, s, Fixture};
use contextful_context::fold::fold;
use contextful_core::store::bound_time::{Bound, Bounds};
use contextful_core::store::fold::FoldOutcome;
use contextful_core::store::reconcile::ColumnType;
use serde_json::json;

#[cfg(feature = "read")]
#[test]
fn old_rows_are_hidden_before_fold_and_inside_an_older_snapshot() {
    let f = Fixture::new();
    let d = decl("name = \"events\"\nretain_rows = { column = \"_ingested_at\", age = \"30d\" }");
    f.land(&d, "old", json!([{"id": "old"}]), "2000-01-01T00:00:00Z").unwrap();
    f.land(&d, "fresh", json!([{"id": "fresh"}]), "3000-01-01T00:00:00Z").unwrap();
    assert_eq!(f.query(&d, Bounds::default(), "SELECT id FROM t ORDER BY id"), [[s("fresh")]]);

    fold(&f.store, &d, at("3000-01-02T00:00:00Z")).unwrap();
    let past = Bounds { as_of: Some(Bound::parse("3000-01-01T12:00:00Z").unwrap()), valid_as_of: None };
    assert_eq!(f.query(&d, past, "SELECT id FROM t ORDER BY id"), [[s("fresh")]]);
}

#[cfg(feature = "read")]
#[test]
fn copied_base_arrival_controls_derived_row_expiry() {
    let f = Fixture::new();
    let d = decl("name = \"derived\"\ncolumns = { base_arrived_at = \"timestamp\" }\nretain_rows = { column = \"base_arrived_at\", age = \"30d\" }");
    f.land_typed(&d, "derive", json!([
        {"id": "old-base", "base_arrived_at": "2000-01-01T00:00:00Z"},
        {"id": "fresh-base", "base_arrived_at": "3000-01-01T00:00:00Z"}
    ]), "3000-01-02T00:00:00Z", &[("base_arrived_at", ColumnType::Timestamp)]).unwrap();
    assert_eq!(f.query(&d, Bounds::default(), "SELECT id FROM t"), [[s("fresh-base")]]);
}

#[cfg(feature = "read")]
#[test]
fn fold_excludes_expired_rows_and_reports_cutoff_and_drops() {
    let f = Fixture::new();
    let d = decl("name = \"events\"\npartition_by = [\"sender_day\"]\nretain_rows = { column = \"_ingested_at\", age = \"30d\" }");
    f.land(&d, "old", json!([{"id": "old", "sender_day": "old"}]), "3000-01-01T00:00:00Z").unwrap();
    f.land(&d, "fresh", json!([{"id": "fresh", "sender_day": "fresh"}]), "3000-02-01T00:00:00Z").unwrap();
    let outcome = fold(&f.store, &d, at("3000-02-02T00:00:00Z")).unwrap();
    let FoldOutcome::Folded { rows, .. } = &outcome else { panic!("{outcome:?}") };
    assert_eq!(*rows, 1);
    let report = outcome.to_string();
    assert!(report.contains("3000-01-03"), "{report}");
    assert!(report.contains("1 row"), "{report}");
    assert!(report.contains("1 partition"), "{report}");
    let files = f.scan(&d, Bounds::default()).unwrap().files;
    assert_eq!(files.len(), 1);
    assert!(files[0].contains("sender_day=fresh"), "{files:?}");
    let path = f.store.root().join(&files[0]);
    let raw = crate::support::query(&format!("SELECT id FROM read_parquet('{}')", path.display()));
    assert_eq!(raw, [[s("fresh")]]);
}

#[test]
fn retention_refuses_an_undeclared_or_non_timestamp_column() {
    let f = Fixture::new();
    for column in ["missing", "count"] {
        let d = decl(&format!("name = \"events\"\ncolumns = {{ count = \"int64\" }}\nretain_rows = {{ column = \"{column}\", age = \"30d\" }}"));
        assert!(f.land(&d, "run", json!([{"count": 1}]), "2030-01-01T00:00:00Z").is_err());
        assert!(f.store.committed_runs("events").unwrap().is_empty());
    }
}
