//! `store.available-time`: the declaration, the `available_as_of` wrap and its echo.

use contextful_core::read::template::parse_templates;
use contextful_core::store::bound_time::{Bound, Bounds};
use contextful_core::store::declare::{AvailableTime, TableDecl, ValidTime};
use contextful_core::store::reconcile::{Column, ColumnType, Schema};
use contextful_core::store::relation::relation_bounded;
use contextful_core::store::StoreError;

const EARNINGS: &str = "[[pipeline.tables]]\nname = \"earnings\"\nprimary_key = [\"filing_id\"]\n\n[pipeline.tables.available_time]\ncolumn = \"accepted_at\"\n";

fn available(s: &str) -> Bounds {
    Bounds { available_as_of: Some(Bound::parse(s).unwrap()), ..Bounds::default() }
}

/// `[pipeline.tables.available_time]` declares `column`, the `timestamptz` instant a value became available from its source, and optional `zone`, UTC by default; landing preserves that instant, never an engine stamp, and availability bounds compare that column alone.
// spec: store.available-time.declaration@05062acd
#[test]
fn an_available_time_block_declares_its_column_and_zone() {
    let t = TableDecl::parse_pipeline(EARNINGS).unwrap().remove(0);
    assert_eq!(t.available_time, Some(AvailableTime { column: "accepted_at".into(), ..AvailableTime::default() }));
    let zoned = TableDecl::parse_pipeline(&format!("{EARNINGS}zone = \"America/New_York\"\n")).unwrap().remove(0);
    assert_eq!(zoned.available_time.unwrap().zone.as_deref(), Some("America/New_York"));
    assert!(TableDecl::parse_pipeline(&format!("{EARNINGS}lag_days = 3\n")).is_err(), "an unknown key refuses the block");

    // The bound compares the declared column alone: neither `_ingested_at` nor valid time.
    let rel = relation_bounded(&t, &["/s/a.parquet".into()], &[], &[], available("2030-04-15T00:00:00Z"), None).unwrap();
    assert!(rel.contains("\"accepted_at\" <= TIMESTAMPTZ '2030-04-15T00:00:00.000000000Z'"), "{rel}");
    assert!(!rel.contains("\"_ingested_at\" <="), "{rel}");
}

/// Beyond {{store.declare.table-block}}, a table block may declare `available_time`, absent from the canonical serialization when unset.
// spec: store.available-time.table-key@940b0709
#[test]
fn an_unset_available_time_is_absent_from_the_canonical_serialization() {
    let plain = TableDecl::named("prices");
    assert!(!serde_json::to_string(&plain).unwrap().contains("available_time"));
    let t = TableDecl::parse_pipeline(EARNINGS).unwrap().remove(0);
    let json = serde_json::to_value(&t).unwrap();
    assert_eq!(json["available_time"], serde_json::json!({ "column": "accepted_at" }));
}

/// `available_as_of` wraps the inner source with `<column> <= available_as_of` over the declared column, conjoined with {{store.bound-time.as-of}} and {{store.bound-time.valid-as-of}} where given.
#[test]
fn available_as_of_wraps_the_inner_source_beneath_the_key_and_beside_valid_time() {
    let mut t = TableDecl::parse_pipeline(EARNINGS).unwrap().remove(0);
    t.valid_time = Some(ValidTime { from: "period_end".into(), to: None });
    let bounds = Bounds { valid_as_of: Some(Bound::parse("2030-03-31T00:00:00Z").unwrap()), ..available("2030-05-03") };
    let rel = relation_bounded(&t, &["/s/a.parquet".into()], &[], &[], bounds, None).unwrap();
    // A date-only bound is exclusive at the start of the next day.
    let filter = "WHERE \"accepted_at\" < TIMESTAMPTZ '2030-05-04T00:00:00.000000000Z'";
    let at = rel.find(filter).unwrap_or_else(|| panic!("{rel}"));
    // The availability filter sits beneath the key's dedup, so it picks among rows the bound admits.
    assert!(at < rel.find("WHERE __contextful_rn = 1").unwrap(), "{rel}");
    assert!(rel.contains("\"period_end\" <= TIMESTAMPTZ '2030-03-31T00:00:00.000000000Z'"), "{rel}");
}

/// A read carrying `available_as_of` adds `available_as_of` to the object {{store.bound-time.echo}} echoes, in the same instant form.
// spec: store.available-time.bound-echo@7cc95b22
#[test]
fn an_availability_bound_joins_the_echo() {
    assert_eq!(available("2030-04-15T00:00:00+02:00").echo().unwrap(), serde_json::json!({ "available_as_of": "2030-04-14T22:00:00.000000000Z", "inclusive": true }));
    let both = Bounds { as_of: Some(Bound::parse("2030-06-01").unwrap()), ..available("2030-04-15T00:00:00Z") };
    assert_eq!(
        both.echo().unwrap(),
        serde_json::json!({ "as_of": "2030-06-02T00:00:00.000000000Z", "available_as_of": "2030-04-15T00:00:00.000000000Z", "inclusive": false })
    );
    // The file-selecting bounds keep `as_of` alone.
    assert_eq!(both.transaction().echo().unwrap(), serde_json::json!({ "as_of": "2030-06-02T00:00:00.000000000Z", "inclusive": false }));
}

/// An `available_as_of` read naming a table that declares no `available_time` raises `StoreAvailableTimeUndeclared`, naming the table.
// spec: store.available-time.undeclared@e1e991f5
#[test]
fn available_as_of_on_a_table_declaring_no_availability_time_is_refused() {
    let t = TableDecl::named("prices");
    match relation_bounded(&t, &["/s/a.parquet".into()], &[], &[], available("2030-01-01T00:00:00Z"), None) {
        Err(StoreError::StoreAvailableTimeUndeclared(m)) => assert!(m.contains("prices"), "{m}"),
        other => panic!("expected StoreAvailableTimeUndeclared, got {other:?}"),
    }
}

/// A template parameter named `available_as_of` refuses the manifest as {{read.guard.template-reserved-parameter}} refuses the other reserved names; that name carries the read's availability bound on every template tool.
// spec: read.guard.available-reserved-parameter@0d3143ac
#[test]
fn a_template_parameter_named_available_as_of_refuses_the_manifest() {
    let manifest = "[[query_templates]]\nid = \"x\"\nsql = \"SELECT 1 WHERE ? IS NOT NULL\"\nparameters = [\"available_as_of:timestamp\"]\n";
    let refused = parse_templates(manifest).unwrap_err();
    assert!(refused.0.contains("available_as_of"), "{refused:?}");
}

const VINTAGES: &str = "[[pipeline.tables]]\nname = \"gdp\"\nprimary_key = [\"series\"]\ncontent_hash_column = \"digest\"\n\n[pipeline.tables.valid_time]\nfrom = \"period\"\n\n[pipeline.tables.available_time]\ncolumn = \"released_at\"\n";

fn schema(columns: &[(&str, ColumnType)]) -> Schema {
    Schema { columns: columns.iter().map(|(n, t)| Column::new(*n, t.clone(), true)).collect() , ..Default::default() }
}

/// A vintage table declaring no `content_hash_column` raises `StoreVintageHashUndeclared` at validation, naming the table.
// spec: store.available-time.vintage-hash-undeclared@56bcb6bc
#[test]
fn a_vintage_table_without_a_content_hash_is_refused() {
    let columns = schema(&[("filing_id", ColumnType::Utf8), ("accepted_at", ColumnType::Timestamp)]);
    let t = TableDecl::parse_pipeline(EARNINGS).unwrap().remove(0);
    match t.validate(&columns) {
        Err(StoreError::StoreVintageHashUndeclared(m)) => assert!(m.contains("earnings"), "{m}"),
        other => panic!("expected StoreVintageHashUndeclared, got {other:?}"),
    }
    // An unkeyed table keeps every row and is no vintage table.
    let unkeyed = TableDecl::parse_pipeline(&EARNINGS.replace("primary_key = [\"filing_id\"]\n", "")).unwrap().remove(0);
    unkeyed.validate(&columns).unwrap();
}

/// A vintage table declaring {{store.declare.retain-rows}} raises `StoreVintageRetainRows` at validation, naming the table.
// spec: store.available-time.vintage-retain-rows@b01a1bfc
#[test]
fn a_vintage_table_retaining_rows_by_age_is_refused() {
    let declared = VINTAGES.replace("content_hash_column = \"digest\"\n", "content_hash_column = \"digest\"\nretain_rows = { column = \"_ingested_at\", age = \"30d\" }\n");
    let t = TableDecl::parse_pipeline(&declared).unwrap().remove(0);
    let columns = schema(&[
        ("series", ColumnType::Utf8),
        ("period", ColumnType::Timestamp),
        ("released_at", ColumnType::Timestamp),
        ("digest", ColumnType::Utf8),
        ("_ingested_at", ColumnType::Timestamp),
    ]);
    match t.validate(&columns) {
        Err(StoreError::StoreVintageRetainRows(m)) => assert!(m.contains("gdp"), "{m}"),
        other => panic!("expected StoreVintageRetainRows, got {other:?}"),
    }
}

/// A vintage table reads through {{store.declare.dedup-view}} with the availability column appended to its partition key, then resolves one row per {{store.available-time.vintage-resolution}}.
// spec: store.available-time.vintage-dedup-key@f1788c35
#[test]
fn a_vintage_table_dedupes_per_availability_instant_then_resolves() {
    let t = TableDecl::parse_pipeline(&VINTAGES.replace("[pipeline.tables.valid_time]\nfrom = \"period\"\n\n", "")).unwrap().remove(0);
    let rel = relation_bounded(&t, &["/s/a.parquet".into()], &[], &[], Bounds::default(), None).unwrap();
    let dedup = rel.find("PARTITION BY \"series\", \"released_at\" ORDER BY \"_ingested_at\" DESC").unwrap_or_else(|| panic!("{rel}"));
    let resolve = rel.find("PARTITION BY \"series\" ORDER BY \"released_at\" DESC").unwrap_or_else(|| panic!("{rel}"));
    // The resolution wraps the dedup, so the dedup renders inside it.
    assert!(resolve < dedup, "{rel}");
}

/// A vintage table declaring `valid_time` dedupes as {{store.bound-time.interval-rows}} with the availability column added to the key, so every interval keeps every vintage.
// spec: store.available-time.vintage-interval-key@f03493c7
#[test]
fn a_vintage_table_with_valid_time_keys_on_interval_and_vintage() {
    let t = TableDecl::parse_pipeline(VINTAGES).unwrap().remove(0);
    assert_eq!(t.dedup_line(), ["series", "period", "released_at"]);
    let rel = relation_bounded(&t, &["/s/a.parquet".into()], &[], &[], Bounds::default(), None).unwrap();
    assert!(rel.contains("PARTITION BY \"series\", \"period\", \"released_at\" ORDER BY"), "{rel}");
    assert!(rel.contains("PARTITION BY \"series\", \"period\" ORDER BY \"released_at\" DESC"), "{rel}");
}

/// The lag block parses its class column, a duration per class and `live_since`.
#[test]
fn a_lag_declares_a_class_column_and_a_duration_per_class() {
    let declared = format!("{EARNINGS}live_since = \"2020-01-01T00:00:00Z\"\nlag = {{ class_column = \"form\", classes = {{ \"10-Q\" = \"40d\", \"10-K\" = \"60d\" }} }}\n");
    let t = TableDecl::parse_pipeline(&declared).unwrap().remove(0);
    let available = t.available_time.unwrap();
    assert_eq!(available.lag.as_ref().unwrap().class_column, "form");
    assert_eq!(available.lag_secs("10-K").unwrap(), Some(60 * 86_400));
    assert_eq!(available.lag_secs("8-K").unwrap(), None);
    assert_eq!(available.live_since.as_deref(), Some("2020-01-01T00:00:00Z"));
    let malformed = format!("{EARNINGS}lag = {{ class_column = \"form\", classes = {{ \"10-Q\" = \"forty\" }} }}\n");
    assert!(TableDecl::parse_pipeline(&malformed).unwrap().remove(0).available_time.unwrap().lag_secs("10-Q").is_err());
}
