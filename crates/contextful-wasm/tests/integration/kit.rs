//! `assurance.test.connector-kit`: the authoring kit's conformance suite, recorded-HTTP
//! replay and position-monotonicity property, run against the probe guest.

use crate::support::{open, text};
use contextful_wasm::kit::{self, Exchange, Replay};
use contextful_wasm::{Cursor, CursorKind, DataType, Field, Schema};

/// The probe's `items` positions are ASCII decimals: a longer one is later.
fn decimal(a: &[u8], b: &[u8]) -> std::cmp::Ordering {
    kit::length_then_bytes(a, b)
}

// spec: assurance.test.connector-kit@20ef05ac
#[test]
fn the_kit_passes_the_probe_guest_and_replays_a_recorded_exchange() {
    // Conformance: discovery yields valid schemas, each table drains finitely, and every
    // reported position reopens onto exactly the remaining rows.
    let mut s = open();
    let reports = kit::conform(&mut s, &["items"], 64).unwrap();
    assert_eq!(reports.len(), 1, "{reports:?}");
    let items = &reports[0];
    assert_eq!(items.table, "items");
    assert!(items.rows > 0 && items.positions.len() > 1, "{items:?}");
    kit::monotonic(&items.positions, decimal).unwrap();

    // The property: from any position the read reports, reopening never moves back.
    let cases = kit::monotonic_from_any_position(&mut s, "items", 64, decimal, 16, 0x5eed).unwrap();
    assert_eq!(cases, 16);

    // Replay: a recorded exchange answers the guest without the vendor.
    let replay = Replay::start(vec![Exchange { method: "GET".into(), path: "/v1/orders".into(), status: 200, body: "recorded".into() }]);
    let mut s = open();
    s.open(&format!("fetch {}", replay.url("/v1/orders")), None).unwrap();
    assert_eq!(text(&s.next().unwrap().unwrap()), "200 recorded");
    assert_eq!(replay.served(), 1);
    assert!(replay.misses().is_empty(), "{:?}", replay.misses());
    let mut s = open();
    s.open(&format!("fetch {}", replay.url("/v1/unrecorded")), None).unwrap();
    assert!(text(&s.next().unwrap().unwrap()).starts_with("599 "));
    assert_eq!(replay.misses(), ["GET /v1/unrecorded"]);
}

#[test]
fn the_kit_refuses_an_invalid_schema_a_backward_position_and_an_endless_read() {
    let field = |name: &str| Field { name: name.into(), ty: DataType::Int64, nullable: false };
    let good = Schema { name: "t".into(), fields: vec![field("id")], primary_key: vec!["id".into()] };
    assert!(kit::validate(&good).is_ok());
    for (bad, why) in [
        (Schema { name: String::new(), ..good.clone() }, "name"),
        (Schema { fields: Vec::new(), primary_key: Vec::new(), ..good.clone() }, "no field"),
        (Schema { fields: vec![field("id"), field("id")], ..good.clone() }, "twice"),
        (Schema { primary_key: vec!["missing".into()], ..good.clone() }, "primary key"),
    ] {
        let e = kit::validate(&bad).unwrap_err();
        assert!(e.contains(why), "{e}");
    }
    let at = |b: &[u8]| Cursor { kind: CursorKind::Monotonic, bytes: b.to_vec() };
    assert!(kit::monotonic(&[at(b"2"), at(b"4"), at(b"10")], decimal).is_ok());
    assert!(kit::monotonic(&[at(b"4"), at(b"2")], decimal).unwrap_err().contains("moved back"));
    let mut s = open();
    let e = kit::conform(&mut s, &["items"], 1).unwrap_err();
    assert!(e.contains("exhaust"), "{e}");
    let mut s = open();
    let e = kit::conform(&mut s, &["no-such-table"], 64).unwrap_err();
    assert!(e.contains("no-such-table"), "{e}");
}
