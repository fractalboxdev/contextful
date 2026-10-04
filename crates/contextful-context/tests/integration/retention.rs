//! Row-age retention at the read and fold boundaries.

use crate::support::{at, decl, s, Fixture};
use contextful_context::fold::fold;
use contextful_context::ContextError;
use contextful_core::store::bound_time::{Bound, Bounds};
use contextful_core::store::declare::TableDecl;
use contextful_core::store::fold::FoldOutcome;
use contextful_core::store::reconcile::ColumnType;
use contextful_core::store::StoreError;
#[cfg(feature = "read")]
use parquet::file::reader::{FileReader, SerializedFileReader};
use serde_json::json;
#[cfg(feature = "read")]
use std::io::{Seek, SeekFrom, Write};

#[cfg(feature = "read")]
fn land_keyed_retention_revisions(f: &Fixture, d: &TableDecl) {
    let timestamp = &[("retained_at", ColumnType::Timestamp)];
    f.land_typed(
        d,
        "older",
        json!([{"id": "same", "rev": 1, "sender_day": "older", "retained_at": "2100-01-01T00:00:00Z"}]),
        "2020-01-01T00:00:00Z",
        timestamp,
    )
    .unwrap();
    f.land_typed(
        d,
        "newer",
        json!([{"id": "same", "rev": 2, "sender_day": "newer", "retained_at": "2000-01-01T00:00:00Z"}]),
        "2020-01-02T00:00:00Z",
        timestamp,
    )
    .unwrap();
}

#[cfg(feature = "read")]
#[test]
fn an_expired_keyed_winner_does_not_restore_an_older_live_row_on_read() {
    let f = Fixture::new();
    let d = decl("name = \"events\"\nprimary_key = [\"id\"]\norder_by = \"rev\"\npartition_by = [\"sender_day\"]\ncolumns = { retained_at = \"timestamp\" }\nretain_rows = { column = \"retained_at\", age = \"30d\" }");
    land_keyed_retention_revisions(&f, &d);
    assert!(f.query(&d, Bounds::default(), "SELECT id FROM t").is_empty());
}

#[cfg(feature = "read")]
#[test]
fn an_expired_keyed_winner_does_not_restore_an_older_live_row_after_fold() {
    let f = Fixture::new();
    let d = decl("name = \"events\"\nprimary_key = [\"id\"]\norder_by = \"rev\"\npartition_by = [\"sender_day\"]\ncolumns = { retained_at = \"timestamp\" }\nretain_rows = { column = \"retained_at\", age = \"30d\" }");
    land_keyed_retention_revisions(&f, &d);
    let outcome = fold(&f.store, &d, at("2030-02-02T00:00:00Z")).unwrap();
    assert!(matches!(outcome, FoldOutcome::Folded { rows: 0, .. }), "{outcome:?}");
    assert!(f.query(&d, Bounds::default(), "SELECT id FROM t").is_empty());
}

#[cfg(feature = "read")]
#[test]
fn old_rows_are_hidden_before_fold_and_inside_an_older_snapshot() {
    let f = Fixture::new();
    let d = decl("name = \"events\"\nretain_rows = { column = \"_ingested_at\", age = \"30d\" }");
    f.land(&d, "old", json!([{"id": "old"}]), "2000-01-01T00:00:00Z")
        .unwrap();
    f.land(
        &d,
        "fresh",
        json!([{"id": "fresh"}]),
        "2200-01-01T00:00:00Z",
    )
    .unwrap();
    assert_eq!(
        f.query(&d, Bounds::default(), "SELECT id FROM t ORDER BY id"),
        [[s("fresh")]]
    );

    fold(&f.store, &d, at("2200-01-02T00:00:00Z")).unwrap();
    let past = Bounds {
        as_of: Some(Bound::parse("2200-01-01T12:00:00Z").unwrap()),
        valid_as_of: None,
    };
    assert_eq!(
        f.query(&d, past, "SELECT id FROM t ORDER BY id"),
        [[s("fresh")]]
    );
}

#[cfg(feature = "read")]
#[test]
fn a_superseded_snapshot_does_not_restore_a_now_expired_row() {
    let f = Fixture::new();
    let d = decl("name = \"events\"\nretain_rows = { column = \"_ingested_at\", age = \"30d\" }");
    let unix = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs() as i64;
    let now = contextful_core::time::Instant::from_unix_secs(unix).unwrap();
    let old = now.minus_secs(31 * 86_400);
    let first_fold = old.plus_secs(86_400);
    f.land(&d, "old", json!([{"id": "old"}]), &old.to_rfc3339()).unwrap();
    fold(&f.store, &d, first_fold).unwrap();
    f.land(&d, "fresh", json!([{"id": "fresh"}]), &now.minus_secs(2 * 86_400).to_rfc3339()).unwrap();
    fold(&f.store, &d, now.minus_secs(86_400)).unwrap();
    let past = Bounds { as_of: Some(Bound { at: first_fold.plus_secs(3600), inclusive: true }), valid_as_of: None };
    assert!(f.query(&d, past, "SELECT id FROM t").is_empty());
    assert_eq!(f.query(&d, Bounds::default(), "SELECT id FROM t"), [[s("fresh")]]);
}

#[cfg(feature = "read")]
#[test]
fn copied_base_arrival_controls_derived_row_expiry() {
    let f = Fixture::new();
    let d = decl("name = \"derived\"\ncolumns = { base_arrived_at = \"timestamp\" }\nretain_rows = { column = \"base_arrived_at\", age = \"30d\" }");
    f.land_typed(
        &d,
        "derive",
        json!([
            {"id": "old-base", "base_arrived_at": "2000-01-01T00:00:00Z"},
            {"id": "fresh-base", "base_arrived_at": "2200-01-01T00:00:00Z"}
        ]),
        "2200-01-02T00:00:00Z",
        &[("base_arrived_at", ColumnType::Timestamp)],
    )
    .unwrap();
    assert_eq!(
        f.query(&d, Bounds::default(), "SELECT id FROM t"),
        [[s("fresh-base")]]
    );
}

#[cfg(feature = "read")]
#[test]
fn a_large_declared_age_keeps_rows_at_read_and_fold() {
    let f = Fixture::new();
    let d = decl("name = \"events\"\nretain_rows = { column = \"_ingested_at\", age = \"213503982334601d\" }");
    f.land(&d, "old", json!([{"id": "old"}]), "2000-01-01T00:00:00Z").unwrap();
    assert_eq!(f.query(&d, Bounds::default(), "SELECT id FROM t"), [[s("old")]]);
    let outcome = fold(&f.store, &d, at("2200-01-01T00:00:00Z")).unwrap();
    assert!(matches!(outcome, FoldOutcome::Folded { rows: 1, .. }), "{outcome:?}");
    assert_eq!(f.query(&d, Bounds::default(), "SELECT id FROM t"), [[s("old")]]);
}

#[cfg(feature = "read")]
#[test]
fn fold_excludes_expired_rows_and_reports_cutoff_and_drops() {
    let f = Fixture::new();
    let d = decl("name = \"events\"\npartition_by = [\"sender_day\"]\nretain_rows = { column = \"_ingested_at\", age = \"30d\" }");
    f.land(
        &d,
        "old",
        json!([{"id": "old", "sender_day": "old"}]),
        "2200-01-01T00:00:00Z",
    )
    .unwrap();
    f.land(
        &d,
        "fresh",
        json!([{"id": "fresh", "sender_day": "fresh"}]),
        "2200-02-01T00:00:00Z",
    )
    .unwrap();
    let outcome = fold(&f.store, &d, at("2200-02-02T00:00:00Z")).unwrap();
    let FoldOutcome::Folded { rows, .. } = &outcome else {
        panic!("{outcome:?}")
    };
    assert_eq!(*rows, 1);
    let report = outcome.to_string();
    assert!(report.contains("2200-01-03"), "{report}");
    assert!(report.contains("1 row"), "{report}");
    assert!(report.contains("1 partition"), "{report}");
    let files = f.scan(&d, Bounds::default()).unwrap().files;
    assert_eq!(files.len(), 1);
    assert!(files[0].contains("sender_day=fresh"), "{files:?}");
    let path = f.store.root().join(&files[0]);
    let raw = crate::support::query(&format!(
        "SELECT id FROM read_parquet('{}')",
        path.display()
    ));
    assert_eq!(raw, [[s("fresh")]]);
}

#[cfg(feature = "read")]
#[test]
fn an_idle_fold_removes_an_expired_snapshot_partition() {
    let f = Fixture::new();
    let d = decl("name = \"events\"\npartition_by = [\"sender_day\"]\nretain_rows = { column = \"_ingested_at\", age = \"30d\" }");
    f.land(
        &d,
        "old",
        json!([{"id": "old", "sender_day": "sender"}]),
        "2200-01-01T00:00:00Z",
    )
    .unwrap();
    fold(&f.store, &d, at("2200-01-02T00:00:00Z")).unwrap();
    let idle = fold(&f.store, &d, at("2200-01-03T00:00:00Z")).unwrap();
    assert!(matches!(idle, FoldOutcome::NothingLandedRetained { .. }), "{idle:?}");
    let before = f.scan(&d, Bounds::default()).unwrap().files;
    assert_eq!(before.len(), 1);
    let path = f.store.root().join(&before[0]);
    let reader = SerializedFileReader::new(std::fs::File::open(&path).unwrap()).unwrap();
    let page = reader.metadata().row_group(0).column(0).data_page_offset() as u64;
    let mut part = std::fs::OpenOptions::new().write(true).open(&path).unwrap();
    part.seek(SeekFrom::Start(page)).unwrap();
    part.write_all(&[0; 16]).unwrap();
    let sql = format!("SELECT * FROM read_parquet('{}')", path.display());
    assert!(
        contextful_context::read::operator_query(&sql, Default::default()).is_err(),
        "the damaged page remains readable"
    );
    let outcome = fold(&f.store, &d, at("2200-02-02T00:00:00Z")).unwrap();
    let FoldOutcome::Folded {
        runs,
        rows,
        retention: Some(report),
        ..
    } = outcome
    else {
        panic!("{outcome:?}")
    };
    assert_eq!(
        (runs, rows, report.rows_expired, report.partitions_dropped),
        (0, 0, 1, 1)
    );
    assert!(f.scan(&d, Bounds::default()).unwrap().files.is_empty());
}

#[cfg(feature = "read")]
#[test]
fn footer_pruning_uses_the_top_level_timestamp_column() {
    let f = Fixture::new();
    let d = decl("name = \"events\"\npartition_by = [\"sender_day\"]\ncolumns = { meta = \"struct<retained_at: timestamp>\", retained_at = \"timestamp\" }\nretain_rows = { column = \"retained_at\", age = \"30d\" }");
    f.land(&d, "run", json!([{
        "sender_day": "day",
        "meta": {"retained_at": "2200-01-01T00:00:00Z"},
        "retained_at": "2200-02-01T00:00:00Z"
    }]), "2200-02-01T00:00:00Z").unwrap();
    fold(&f.store, &d, at("2200-02-02T00:00:00Z")).unwrap();
    let outcome = fold(&f.store, &d, at("2200-02-03T00:00:00Z")).unwrap();
    assert!(matches!(outcome, FoldOutcome::NothingLandedRetained { .. }), "{outcome:?}");
    assert_eq!(f.query(&d, Bounds::default(), "SELECT sender_day FROM t"), [[s("day")]]);
}

#[test]
fn collection_names_each_removed_directory() {
    let f = Fixture::new();
    let d = decl("name = \"events\"\npartition_by = [\"sender_day\"]\nretain_rows = { column = \"_ingested_at\", age = \"30d\" }");
    f.land(
        &d,
        "first",
        json!([{"id": "first", "sender_day": "first"}]),
        "2200-01-01T00:00:00Z",
    )
    .unwrap();
    fold(&f.store, &d, at("2200-01-02T00:00:00Z")).unwrap();
    f.land(
        &d,
        "second",
        json!([{"id": "second", "sender_day": "second"}]),
        "2200-01-03T00:00:00Z",
    )
    .unwrap();
    fold(&f.store, &d, at("2200-01-04T00:00:00Z")).unwrap();
    let removed =
        contextful_context::fold::collect(&f.store, &d, at("2200-01-10T00:00:00Z")).unwrap();
    assert!(
        removed.iter().any(|p| p == "data/runs/first/ingest-a"),
        "{removed:?}"
    );
    f.land(
        &d,
        "third",
        json!([{"id": "third", "sender_day": "third"}]),
        "2200-01-11T00:00:00Z",
    )
    .unwrap();
    let outcome = fold(&f.store, &d, at("2200-02-05T00:00:00Z")).unwrap();
    let FoldOutcome::Folded { retention: Some(report), collected, .. } = &outcome else {
        panic!("{outcome:?}")
    };
    assert_eq!(report.cutoff, at("2200-01-06T00:00:00Z"));
    assert_eq!((report.rows_expired, report.partitions_dropped), (2, 2));
    assert!(collected.iter().any(|p| p == "data/runs/second/ingest-a"), "{collected:?}");
    assert!(collected.iter().any(|p| p.starts_with("data/snapshots/")), "{collected:?}");
    let printed = outcome.to_string();
    assert!(printed.contains("2200-01-06") && printed.contains("2 rows expired") && printed.contains("2 partitions dropped"), "{printed}");
    assert!(
        printed.contains("data/runs/second/ingest-a"),
        "{outcome:?}"
    );
    assert!(printed.contains("data/snapshots/"), "{outcome:?}");
}

#[test]
fn retention_refuses_an_undeclared_or_non_timestamp_column() {
    let f = Fixture::new();
    for column in ["missing", "count"] {
        let d = decl(&format!("name = \"events\"\ncolumns = {{ count = \"int64\" }}\nretain_rows = {{ column = \"{column}\", age = \"30d\" }}"));
        let result = f.land(&d, "run", json!([{"count": 1}]), "2030-01-01T00:00:00Z");
        assert!(matches!(result, Err(ContextError::Store(StoreError::StoreRetentionColumnInvalid(_)))), "{result:?}");
        assert!(f.store.committed_runs("events").unwrap().is_empty());
    }
    assert!(TableDecl::parse_pipeline("[[pipeline.tables]]\nname = \"events\"\nretain_rows = { column = \"_ingested_at\", age = \"soon\" }").is_err());
}

#[test]
fn retention_refuses_a_null_source_timestamp_before_a_batch_lands() {
    let f = Fixture::new();
    let d = decl("name = \"derived\"\ncolumns = { base_arrived_at = \"timestamp\" }\nretain_rows = { column = \"base_arrived_at\", age = \"30d\" }");
    let result = f.land(&d, "bad", json!([{"id": "x", "base_arrived_at": null}]), "2200-01-01T00:00:00Z");
    assert!(matches!(result, Err(ContextError::Store(StoreError::StoreRetentionColumnInvalid(_)))), "{result:?}");
    assert!(f.store.committed_runs("derived").unwrap().is_empty());
}
