//! `store.snapshot` declarations, adjustment declarations and the per-column first landing.

use contextful_core::store::declare::{Adjustment, SnapshotDecl, TableDecl};
use contextful_core::store::reconcile::{Column, ColumnType, Schema};
use contextful_core::time::Instant;

const PAGES: &str = "[[pipeline.tables]]\nname = \"pages\"\nprimary_key = [\"url\"]\ncontent_hash_column = \"digest\"\n\n[pipeline.tables.available_time]\ncolumn = \"published_at\"\n\n[pipeline.tables.snapshot]\nurl = \"url\"\ncaptured_at = \"captured_at\"\ndigest = \"digest\"\nsource = \"source\"\nleak_score = \"leak_score\"\n";

fn at(s: &str) -> Instant {
    Instant::parse(s).unwrap()
}

#[test]
fn a_snapshot_block_names_its_capture_columns() {
    let t = TableDecl::parse_pipeline(PAGES).unwrap().remove(0);
    assert_eq!(
        t.snapshot,
        Some(SnapshotDecl {
            url: "url".into(),
            captured_at: "captured_at".into(),
            digest: "digest".into(),
            source: "source".into(),
            leak_score: Some("leak_score".into()),
        })
    );
    let names = t.declared_column_names();
    assert!(["url", "captured_at", "digest", "source", "leak_score"].iter().all(|c| names.contains(c)), "{names:?}");
    assert!(TableDecl::parse_pipeline(&format!("{PAGES}live = true\n")).is_err(), "an unknown key refuses the block");
    assert!(!serde_json::to_string(&TableDecl::named("plain")).unwrap().contains("snapshot"));
}

#[test]
fn an_adjustment_names_its_factor_table_and_the_columns_it_multiplies() {
    let prices = "[[pipeline.tables]]\nname = \"prices\"\n\n[pipeline.tables.valid_time]\nfrom = \"date\"\n\n[pipeline.tables.available_time]\ncolumn = \"available_at\"\nadjustment = { table = \"splits\", factor_column = \"factor\", adjust_columns = [\"close\", \"open\"] }\n";
    let t = TableDecl::parse_pipeline(prices).unwrap().remove(0);
    assert_eq!(
        t.available_time.unwrap().adjustment,
        Some(Adjustment { table: "splits".into(), factor_column: "factor".into(), adjust_columns: vec!["close".into(), "open".into()] })
    );
}

#[test]
fn first_landing_round_trips_through_schema_json_and_survives_a_merge() {
    let mut stored = Schema { columns: vec![Column::new("id", ColumnType::Utf8, true)], ..Default::default() };
    stored.first_landed.insert("id".into(), at("2014-04-30T00:00:00Z"));
    let doc = stored.to_arrow_json();
    assert_eq!(doc["first_landed_at"]["id"], "2014-04-30T00:00:00.000000000Z");
    assert_eq!(Schema::from_arrow_json(&doc).unwrap(), stored);

    // A merge keeps the stored instant and takes an arriving one only for a column it lacks.
    let mut arriving = Schema { columns: vec![Column::new("id", ColumnType::Utf8, true), Column::new("guidance", ColumnType::Utf8, true)], ..Default::default() };
    arriving.first_landed.insert("id".into(), at("2020-01-01T00:00:00Z"));
    arriving.first_landed.insert("guidance".into(), at("2020-01-01T00:00:00Z"));
    let merged = stored.merge(&arriving, &[]).unwrap();
    assert_eq!(merged.first_landed["id"], at("2014-04-30T00:00:00Z"));
    assert_eq!(merged.first_landed["guidance"], at("2020-01-01T00:00:00Z"));
}
