//! `run.export-iceberg`: the Iceberg `[[export]]` block, the Iceberg type each stored
//! column takes, and the namespace each credential owns.

use contextful_core::export::iceberg::{plan, IcebergRecord, IcebergType, ICEBERG_COMMIT_ATTEMPTS, ICEBERG_DATA_FILE_BYTES};
use contextful_core::export::{parse_exports, ExportError, Signal};
use contextful_core::store::reconcile::{ColumnType, FloatItem, StructField};

fn block(name: &str, credential: &str, namespace: &str, table_name: &str) -> String {
    format!(
        "[[export]]\nname = \"{name}\"\ntable = \"events\"\nformat = \"iceberg\"\ncredential = \"{credential}\"\n\
         catalog = \"https://catalog.example.com/iceberg\"\nstorage = \"https://objects.example.com\"\n\
         warehouse = \"exports\"\nnamespace = \"{namespace}\"\ntable_name = \"{table_name}\"\nformat_version = 3\n\
         token = \"${{secret://catalog-token}}\"\n"
    )
}

fn columns(list: &[(&str, ColumnType)]) -> Vec<(String, ColumnType)> {
    list.iter().map(|(n, t)| (n.to_string(), t.clone())).collect()
}

/// An Iceberg block declares `format = "iceberg"`, one landed `table`, the admitted `credential` it reads as, a REST
/// `catalog` URI, a `storage` endpoint, a `warehouse`, a `namespace`, a `table_name`, a `format_version` of 2 or 3, and
/// a token reference.
#[test]
fn an_iceberg_block_declares_its_catalog_target_and_credential() {
    let text = block("events-copy", "svc://mirror", "mirror", "events");
    let export = parse_exports(&text).unwrap().remove(0);
    assert_eq!(export.signal, Signal::Iceberg);
    let target = export.iceberg.as_ref().unwrap();
    assert_eq!(target.credential, "svc://mirror");
    assert_eq!(target.catalog.as_str(), "https://catalog.example.com/iceberg");
    assert_eq!(target.storage.host_str(), Some("objects.example.com"));
    assert_eq!((target.warehouse.as_str(), target.namespace.as_str(), target.table_name.as_str()), ("exports", "mirror", "events"));
    assert_eq!(target.format_version, 3);
    assert!(target.token.has_reference());

    for (from, to) in [
        ("format_version = 3", "format_version = 1"),
        ("format_version = 3", "format_version = 4"),
        ("credential = \"svc://mirror\"\n", ""),
        ("namespace = \"mirror\"\n", ""),
        ("token = \"${secret://catalog-token}\"\n", ""),
        ("catalog = \"https://catalog.example.com/iceberg\"", "catalog = \"ftp://catalog.example.com\""),
        ("storage = \"https://objects.example.com\"", "storage = \"objects.example.com\""),
    ] {
        let refused = parse_exports(&text.replacen(from, to, 1));
        assert!(matches!(refused, Err(ExportError::Invalid(_))), "{from} → {to}: {refused:?}");
    }
    // An Iceberg block takes no OTLP or typed field.
    for extra in ["signal = \"logs\"\n", "endpoint = \"https://otel.example.com/v1/logs\"\n", "key = [\"id\"]\n", "schedule = \"every 30s\"\n"] {
        assert!(parse_exports(&format!("{text}{extra}")).is_err(), "{extra}");
    }
}

/// Each stored column takes one Iceberg type that holds every value it stores.
#[test]
fn each_stored_column_takes_the_iceberg_type_holding_its_values() {
    let stored = columns(&[
        ("flag", ColumnType::Boolean),
        ("small", ColumnType::Int32),
        ("count", ColumnType::Int64),
        ("score", ColumnType::Float64),
        ("title", ColumnType::Utf8),
        ("doc", ColumnType::Json),
        ("blob", ColumnType::Binary),
        ("digest", ColumnType::FixedSizeBinary(32)),
        ("empty", ColumnType::Null),
        ("tags", ColumnType::List(Box::new(ColumnType::Utf8))),
        ("attrs", ColumnType::Map(Box::new(ColumnType::Int64))),
        ("place", ColumnType::Struct(vec![StructField::new("city", ColumnType::Utf8), StructField::new("floor", ColumnType::Int32)])),
        ("_commit_seq", ColumnType::Int64),
        ("_ingested_at", ColumnType::Timestamp),
    ]);
    let planned = plan("events-copy", &stored, 2).unwrap();
    let names: Vec<&str> = planned.columns.iter().map(|c| c.name.as_str()).collect();
    assert_eq!(names, ["flag", "small", "count", "score", "title", "doc", "blob", "digest", "empty", "tags", "attrs", "place"], "the engine's `_` columns stay its own");
    let types: Vec<&IcebergType> = planned.columns.iter().map(|c| &c.ty).collect();
    assert_eq!(
        types,
        [
            &IcebergType::Boolean,
            &IcebergType::Int,
            &IcebergType::Long,
            &IcebergType::Double,
            &IcebergType::String,
            &IcebergType::String,
            &IcebergType::Binary,
            &IcebergType::Fixed(32),
            &IcebergType::String,
            &IcebergType::List(Box::new(IcebergType::String)),
            &IcebergType::Map(Box::new(IcebergType::Long)),
            &IcebergType::Struct(vec![("city".into(), IcebergType::String), ("floor".into(), IcebergType::Int)]),
        ]
    );
    assert!(planned.properties.is_empty());
}

/// A `FixedSizeList<T, n>` column exports as `list<T>` and records `n` in the table property
/// `contextful.fixed-size-list.<column>`.
#[test]
fn a_fixed_size_list_exports_as_a_list_and_records_its_width() {
    let stored = columns(&[
        ("embedding", ColumnType::FixedSizeList(FloatItem::Float32, 384)),
        ("half", ColumnType::FixedSizeList(FloatItem::Float16, 8)),
        ("nested", ColumnType::Struct(vec![StructField::new("vector", ColumnType::FixedSizeList(FloatItem::Float32, 4))])),
    ]);
    let planned = plan("events-copy", &stored, 2).unwrap();
    assert_eq!(planned.columns[0].ty, IcebergType::List(Box::new(IcebergType::Float)));
    assert_eq!(planned.columns[1].ty, IcebergType::List(Box::new(IcebergType::Float)), "half precision widens to `float`");
    assert_eq!(planned.properties.get("contextful.fixed-size-list.embedding").map(String::as_str), Some("384"));
    assert_eq!(planned.properties.get("contextful.fixed-size-list.half").map(String::as_str), Some("8"));
    assert_eq!(planned.properties.get("contextful.fixed-size-list.nested.vector").map(String::as_str), Some("4"));
}

/// Under `format_version = 3`, a nanosecond timestamp column exports as `timestamptz_ns`, keeping every digit.
#[test]
fn a_nanosecond_timestamp_exports_whole_under_format_version_3() {
    let stored = columns(&[("observed_at", ColumnType::Timestamp), ("window", ColumnType::List(Box::new(ColumnType::Timestamp)))]);
    let planned = plan("events-copy", &stored, 3).unwrap();
    assert_eq!(planned.columns[0].ty, IcebergType::TimestamptzNs);
    assert_eq!(planned.columns[1].ty, IcebergType::List(Box::new(IcebergType::TimestamptzNs)));
}

/// A nanosecond timestamp column, top-level or nested, under `format_version = 2` raises `ExportTypeUnsupported`
/// naming the column.
#[test]
fn a_nanosecond_timestamp_under_format_version_2_refuses_naming_the_column() {
    for (column, ty) in [
        ("observed_at", ColumnType::Timestamp),
        ("visits", ColumnType::List(Box::new(ColumnType::Timestamp))),
        ("place", ColumnType::Struct(vec![StructField::new("seen", ColumnType::Timestamp)])),
    ] {
        let stored = columns(&[("title", ColumnType::Utf8), (column, ty)]);
        match plan("events-copy", &stored, 2) {
            Err(ExportError::ExportTypeUnsupported(m)) => {
                assert!(m.contains(&format!("`{column}")) && m.contains("events-copy"), "{m}");
                assert!(m.starts_with("export"), "{m}");
            }
            other => panic!("{column}: {other:?}"),
        }
    }
    let shown = plan("events-copy", &columns(&[("observed_at", ColumnType::Timestamp)]), 2).unwrap_err().to_string();
    assert!(shown.starts_with("ExportTypeUnsupported: "), "{shown}");
}

/// Two Iceberg exports naming one namespace under different credentials, one credential under two namespaces, or one
/// table raise `ExportNamespaceShared`, naming both exports.
#[test]
fn a_namespace_serves_one_credential_and_a_credential_one_namespace() {
    let two = |a: String, b: String| parse_exports(&format!("{a}{}", b.replace("table = \"events\"", "table = \"notes\"")));
    // One credential, one namespace, two tables: admitted.
    assert_eq!(two(block("a", "svc://one", "one", "events"), block("b", "svc://one", "one", "notes")).unwrap().len(), 2);
    // Two credentials, two namespaces: admitted.
    assert_eq!(two(block("a", "svc://one", "one", "events"), block("b", "svc://two", "two", "events")).unwrap().len(), 2);
    for (a, b) in [
        (block("a", "svc://one", "shared", "events"), block("b", "svc://two", "shared", "notes")),
        (block("a", "svc://one", "one", "events"), block("b", "svc://one", "two", "notes")),
        (block("a", "svc://one", "one", "events"), block("b", "svc://one", "one", "events")),
    ] {
        match two(a, b) {
            Err(ExportError::ExportNamespaceShared(m)) => assert!(m.contains("`a`") && m.contains("`b`"), "{m}"),
            other => panic!("{other:?}"),
        }
    }
}

/// An export records its committed snapshot id, source frontier, read identity and declared target, and commits
/// nothing when all four equal the table's current snapshot id and the export's present values.
#[test]
fn an_export_commits_nothing_while_its_record_matches_the_table_and_the_source() {
    let record = IcebergRecord { snapshot_id: 7, frontier: "f1".into(), identity: "i1".into(), target: "t1".into() };
    assert!(record.holds(Some(7), "f1", "i1", "t1"));
    assert!(!record.holds(Some(8), "f1", "i1", "t1"), "another writer committed");
    assert!(!record.holds(None, "f1", "i1", "t1"), "the table lost its snapshot");
    assert!(!record.holds(Some(7), "f2", "i1", "t1"), "the source committed");
    assert!(!record.holds(Some(7), "f1", "i2", "t1"), "the grant changed");
    assert!(!record.holds(Some(7), "f1", "i1", "t2"), "the block points elsewhere");
}

/// The commit attempts and the data file ceiling.
#[test]
fn the_commit_attempts_and_the_data_file_ceiling_hold_their_bounds() {
    assert_eq!(ICEBERG_COMMIT_ATTEMPTS, 3);
    assert_eq!(ICEBERG_DATA_FILE_BYTES, 128 * 1024 * 1024);
}
