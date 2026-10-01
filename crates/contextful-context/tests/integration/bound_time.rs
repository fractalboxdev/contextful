//! `store.bound-time`: bounded reads executed over the resolved relation.

use crate::support::{at, decl, s, Fixture};
use contextful_context::fold::fold;
use contextful_core::store::bound_time::{Bound, Bounds};
use contextful_core::store::reconcile::ColumnType;
use serde_json::json;

fn as_of(v: &str) -> Bounds {
    Bounds { as_of: Some(Bound::parse(v).unwrap()), valid_as_of: None }
}

fn valid_as_of(v: &str) -> Bounds {
    Bounds { as_of: None, valid_as_of: Some(Bound::parse(v).unwrap()) }
}

#[cfg(feature = "read")]
/// `as_of` resolves each table to the newest reachable snapshot created at or before it, plus the committed runs at or before it that snapshot omits, inside the table's FROM-source.
#[test]
fn as_of_returns_the_same_rows_before_and_after_a_fold() {
    let f = Fixture::new();
    let d = decl("name = \"filings\"\nprimary_key = [\"doc\"]");
    f.land(&d, "run-1", json!([{"doc": "a", "v": 1}]), "2030-01-01T00:00:00Z").unwrap();
    f.land(&d, "run-2", json!([{"doc": "a", "v": 2}]), "2030-01-01T02:00:00Z").unwrap();
    let bound = as_of("2030-01-01T01:00:00Z");
    let before = f.query(&d, bound, "SELECT v FROM t");
    assert_eq!(before, [[s("1")]]);
    fold(&f.store, &d, at("2030-01-01T03:00:00Z")).unwrap();
    assert_eq!(f.query(&d, bound, "SELECT v FROM t"), before);
    assert_eq!(f.query(&d, Bounds::default(), "SELECT v FROM t"), [[s("2")]]);
}

#[cfg(feature = "read")]
/// `valid_as_of` wraps the same inner source with `from <= valid_as_of AND (to IS NULL OR to > valid_as_of)` over the declared pair.
// spec: store.bound-time.valid-as-of@79fe2747
#[test]
fn valid_as_of_selects_the_rows_valid_at_the_instant() {
    let f = Fixture::new();
    let d = decl("name = \"rates\"\nprimary_key = [\"ccy\"]\n[pipeline.tables.valid_time]\nfrom = \"from_ts\"\nto = \"to_ts\"");
    let ts = [("from_ts", ColumnType::Timestamp), ("to_ts", ColumnType::Timestamp)];
    f.land_typed(&d, "run-1", json!([
        {"ccy": "eur", "rate": 1, "from_ts": "2030-01-01T00:00:00Z", "to_ts": "2030-02-01T00:00:00Z"},
        {"ccy": "gbp", "rate": 2, "from_ts": "2030-01-15T00:00:00Z", "to_ts": null},
    ]), "2030-01-01T00:00:00Z", &ts).unwrap();
    let rel = f.scan(&d, valid_as_of("2030-01-20T00:00:00Z")).unwrap().relation;
    assert!(rel.contains("\"from_ts\" <= TIMESTAMPTZ '2030-01-20T00:00:00.000000000Z' AND (\"to_ts\" IS NULL OR \"to_ts\" > TIMESTAMPTZ '2030-01-20T00:00:00.000000000Z')"), "{rel}");
    assert_eq!(f.query(&d, valid_as_of("2030-01-20T00:00:00Z"), "SELECT ccy FROM t ORDER BY ccy"), [[s("eur")], [s("gbp")]]);
    assert_eq!(f.query(&d, valid_as_of("2030-01-10T00:00:00Z"), "SELECT ccy FROM t"), [[s("eur")]]);
    // `to` is exclusive, `from` inclusive.
    assert_eq!(f.query(&d, valid_as_of("2030-02-01T00:00:00Z"), "SELECT ccy FROM t"), [[s("gbp")]]);
    assert_eq!(f.query(&d, valid_as_of("2030-01-15T00:00:00Z"), "SELECT ccy FROM t ORDER BY ccy"), [[s("eur")], [s("gbp")]]);
    assert_eq!(f.scan(&d, valid_as_of("2030-01-15T00:00:00Z")).unwrap().bounds.unwrap()["valid_as_of"], "2030-01-15T00:00:00.000000000Z");
}

#[cfg(feature = "read")]
/// Over an unkeyed table a valid-time bound returns every version whose interval covers the instant.
// spec: store.bound-time.covering-versions@a39e0fdc
#[test]
fn an_unkeyed_table_returns_every_covering_version() {
    let f = Fixture::new();
    let d = decl("name = \"assignments\"\n[pipeline.tables.valid_time]\nfrom = \"from_ts\"");
    let ts = [("from_ts", ColumnType::Timestamp)];
    f.land_typed(&d, "run-1", json!([
        {"who": "dana", "role": "analyst", "from_ts": "2030-01-01T00:00:00Z"},
        {"who": "dana", "role": "lead", "from_ts": "2030-03-01T00:00:00Z"},
        {"who": "dana", "role": "reviewer", "from_ts": "2030-01-10T00:00:00Z"},
    ]), "2030-01-01T00:00:00Z", &ts).unwrap();
    // `from` alone: each row is valid from its instant onward.
    let roles = f.query(&d, valid_as_of("2030-02-01T00:00:00Z"), "SELECT role FROM t ORDER BY role");
    assert_eq!(roles, [[s("analyst")], [s("reviewer")]]);
}

#[cfg(feature = "read")]
/// A keyed table declaring `valid_time` keeps one row per key and line, so a valid-time read reaches a key's past version.
#[test]
fn a_keyed_valid_time_read_reaches_the_version_valid_then() {
    let f = Fixture::new();
    let d = decl("name = \"rates\"\nprimary_key = [\"ccy\"]\n[pipeline.tables.valid_time]\nfrom = \"from_ts\"");
    let ts = [("from_ts", ColumnType::Timestamp)];
    f.land_typed(&d, "run-1", json!([
        {"ccy": "eur", "rate": 1, "from_ts": "2030-01-01T00:00:00Z"},
        {"ccy": "eur", "rate": 2, "from_ts": "2030-03-01T00:00:00Z"},
    ]), "2030-01-01T00:00:00Z", &ts).unwrap();
    let rates = |b: &str| f.query(&d, valid_as_of(b), "SELECT rate FROM t ORDER BY rate");
    assert_eq!(rates("2030-02-15T00:00:00Z"), [[s("1")]]);
    fold(&f.store, &d, at("2030-01-01T01:00:00Z")).unwrap();
    assert_eq!(rates("2030-02-15T00:00:00Z"), [[s("1")]]);
    assert_eq!(rates("2030-03-15T00:00:00Z"), [[s("1")], [s("2")]]);
}

#[cfg(feature = "read")]
/// A date-only valid-time bound covers the whole day: a row ending at the next midnight is valid on it.
#[test]
fn a_date_only_valid_bound_covers_rows_ending_at_the_next_midnight() {
    let f = Fixture::new();
    let d = decl("name = \"rates\"\n[pipeline.tables.valid_time]\nfrom = \"from_ts\"\nto = \"to_ts\"");
    let ts = [("from_ts", ColumnType::Timestamp), ("to_ts", ColumnType::Timestamp)];
    f.land_typed(&d, "run-1", json!([
        {"rate": 1, "from_ts": "2030-01-01T00:00:00Z", "to_ts": "2030-01-16T00:00:00Z"},
        {"rate": 2, "from_ts": "2030-01-16T00:00:00Z", "to_ts": null},
    ]), "2030-01-01T00:00:00Z", &ts).unwrap();
    assert_eq!(f.query(&d, valid_as_of("2030-01-15"), "SELECT rate FROM t"), [[s("1")]]);
    assert_eq!(f.query(&d, valid_as_of("2030-01-16"), "SELECT rate FROM t"), [[s("2")]]);
}
