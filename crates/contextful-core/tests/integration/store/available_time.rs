//! `store.available-time`: the declaration, the `available_as_of` wrap and its echo.

use contextful_core::read::template::parse_templates;
use contextful_core::store::bound_time::{Bound, Bounds};
use contextful_core::store::declare::{AvailableTime, TableDecl, ValidTime};
use contextful_core::store::relation::relation_bounded;
use contextful_core::store::StoreError;

const EARNINGS: &str = "[[pipeline.tables]]\nname = \"earnings\"\nprimary_key = [\"filing_id\"]\n\n[pipeline.tables.available_time]\ncolumn = \"accepted_at\"\n";

fn available(s: &str) -> Bounds {
    Bounds { available_as_of: Some(Bound::parse(s).unwrap()), ..Bounds::default() }
}

/// `[pipeline.tables.available_time]` declares `column`, the `timestamptz` instant a row's value became public, and an optional `zone`, UTC by default; landing writes the instant resolved from the source, never an engine stamp, and an availability bound compares that column alone.
// spec: store.available-time.declaration@4bf8f9e1
#[test]
fn an_available_time_block_declares_its_column_and_zone() {
    let t = TableDecl::parse_pipeline(EARNINGS).unwrap().remove(0);
    assert_eq!(t.available_time, Some(AvailableTime { column: "accepted_at".into(), zone: None }));
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
