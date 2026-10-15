//! `store.available-time`: availability-bounded scans and the landing refusal.

use crate::support::{decl, Fixture};
use contextful_context::ContextError;
use contextful_core::store::bound_time::{Bound, Bounds};
use contextful_core::store::reconcile::ColumnType;
use contextful_core::store::StoreError;
use serde_json::json;

const EARNINGS: &str = "name = \"earnings\"\nprimary_key = [\"filing_id\"]\n[pipeline.tables.valid_time]\nfrom = \"period_end\"\n[pipeline.tables.available_time]\ncolumn = \"accepted_at\"";
const TYPES: [(&str, ColumnType); 2] = [("period_end", ColumnType::Timestamp), ("accepted_at", ColumnType::Timestamp)];

fn available(s: &str) -> Bounds {
    Bounds { available_as_of: Some(Bound::parse(s).unwrap()), ..Bounds::default() }
}

#[cfg(feature = "read")]
/// `available_as_of` wraps the inner source with `<column> <= available_as_of` over the declared column, conjoined with {{store.bound-time.as-of}} and {{store.bound-time.valid-as-of}} where given.
// spec: store.available-time.available-as-of@318cd14f
#[test]
fn a_row_answers_from_its_availability_instant_onward() {
    use crate::support::s;
    let f = Fixture::new();
    let d = decl(EARNINGS);
    // Period end and acceptance land together, in one run committed long after both.
    let row = json!([{ "filing_id": "acme-2030q1", "eps": "1.23", "period_end": "2030-03-31T00:00:00Z", "accepted_at": "2030-05-02T16:05:00Z" }]);
    f.land_typed(&d, "run-1", row, "2030-06-01T00:00:00Z", &TYPES).unwrap();
    let select = "SELECT filing_id FROM t";

    assert!(f.query(&d, available("2030-04-15T00:00:00Z"), select).is_empty());
    assert_eq!(f.query(&d, available("2030-05-02T16:05:00Z"), select), [[s("acme-2030q1")]], "the bound is inclusive");
    assert_eq!(f.query(&d, available("2030-05-03T00:00:00Z"), select), [[s("acme-2030q1")]]);
    // `_ingested_at` plays no part: the row committed on 1 June answers a May bound.
    let echo = f.scan(&d, available("2030-05-03T00:00:00Z")).unwrap().bounds.unwrap();
    assert_eq!(echo, json!({ "available_as_of": "2030-05-03T00:00:00.000000000Z", "inclusive": true }));

    // Conjoined with valid time: the row is valid from period end and public from acceptance.
    let both = |valid: &str, avail: &str| Bounds { valid_as_of: Some(Bound::parse(valid).unwrap()), ..available(avail) };
    assert!(f.query(&d, both("2030-03-01T00:00:00Z", "2030-05-03T00:00:00Z"), select).is_empty());
    assert_eq!(f.query(&d, both("2030-04-01T00:00:00Z", "2030-05-03T00:00:00Z"), select), [[s("acme-2030q1")]]);
}

/// An `available_as_of` read naming a table that declares no `available_time` raises `StoreAvailableTimeUndeclared`, naming the table.
#[test]
fn a_scan_bounded_by_availability_refuses_an_undeclaring_table() {
    let f = Fixture::new();
    let d = decl("name = \"prices\"\nprimary_key = [\"ticker\"]");
    f.land(&d, "run-1", json!([{ "ticker": "ACME", "close": 41.5 }]), "2030-06-01T00:00:00Z").unwrap();
    match f.scan(&d, available("2030-05-03T00:00:00Z")) {
        Err(ContextError::Store(StoreError::StoreAvailableTimeUndeclared(m))) => assert!(m.contains("prices"), "{m}"),
        other => panic!("expected StoreAvailableTimeUndeclared, got {:?}", other.map(|s| s.relation)),
    }
}

/// Landing a row whose resolved availability instant is null raises `StoreAvailableTimeNull`, naming the table and the run.
// spec: store.available-time.null@6f96e557
#[test]
fn landing_a_row_without_an_availability_instant_is_refused() {
    let f = Fixture::new();
    let d = decl(EARNINGS);
    let rows = json!([
        { "filing_id": "acme-2030q1", "period_end": "2030-03-31T00:00:00Z", "accepted_at": "2030-05-02T16:05:00Z" },
        { "filing_id": "acme-2030q2", "period_end": "2030-06-30T00:00:00Z", "accepted_at": null },
    ]);
    match f.land_typed(&d, "run-7", rows, "2030-08-01T00:00:00Z", &TYPES) {
        Err(ContextError::Store(StoreError::StoreAvailableTimeNull(m))) => assert!(m.contains("earnings") && m.contains("run-7"), "{m}"),
        other => panic!("expected StoreAvailableTimeNull, got {other:?}"),
    }
    // A row omitting the column resolves no instant either; nothing of the run lands.
    let absent = json!([{ "filing_id": "acme-2030q3", "period_end": "2030-09-30T00:00:00Z" }]);
    assert!(matches!(f.land_typed(&d, "run-8", absent, "2030-11-01T00:00:00Z", &TYPES), Err(ContextError::Store(StoreError::StoreAvailableTimeNull(_)))));
    assert!(f.store.try_schema("earnings").unwrap().is_none());
}
