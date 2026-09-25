//! `run.advance`: cursor kinds, the watermark, the frontier and ordering.

use contextful_core::run::advance::{admits, advance, compare, frontier, open_watermark, resolve_concurrent, watermark, CursorKind};
use contextful_core::run::RunError;
use serde_json::{json, Map, Value};

fn rows(values: &[Value]) -> Vec<Map<String, Value>> {
    values.iter().map(|v| json!({ "updated_at": v }).as_object().unwrap().clone()).collect()
}

/// A cursor declares `monotonic` for a timestamp, autoincrement or watermark, `opaque-token` for a vendor
/// continuation token, or `snapshot-id` for a log sequence number or version id. An undeclared kind resolves to
/// `opaque-token`.
// spec: run.advance.cursor-kind@eb822467
#[test]
fn three_kinds_and_an_undeclared_one_reads_opaque_token() {
    assert_eq!(CursorKind::resolve(Some("monotonic")).unwrap(), CursorKind::Monotonic);
    assert_eq!(CursorKind::resolve(Some("opaque-token")).unwrap(), CursorKind::OpaqueToken);
    assert_eq!(CursorKind::resolve(Some("snapshot-id")).unwrap(), CursorKind::SnapshotId);
    assert_eq!(CursorKind::resolve(None).unwrap(), CursorKind::OpaqueToken);
    assert!(CursorKind::resolve(Some("latest")).is_err());
    assert!(!CursorKind::Monotonic.single_writer());
    assert!(CursorKind::OpaqueToken.single_writer() && CursorKind::SnapshotId.single_writer());
}

/// A watermark position serializes as `{"field": "<name>", "at": <value>}`, naming the column it was measured
/// against.
// spec: run.advance.watermark-shape@b6edb281
#[test]
fn a_watermark_names_its_field() {
    assert_eq!(watermark("updated_at", json!("2030-01-01T00:00:00Z")), json!({"field": "updated_at", "at": "2030-01-01T00:00:00Z"}));
    assert_eq!(serde_json::to_string(&watermark("id", json!(7))).unwrap(), r#"{"at":7,"field":"id"}"#);
}

/// Opening a position whose stored field differs from the declared `incremental` field raises
/// `CursorFieldMismatch` before any request leaves the host.
// spec: run.advance.field-rename@3dad792d
#[test]
fn a_position_measured_on_another_field_is_refused() {
    let stored = watermark("modified_at", json!(10));
    match open_watermark(Some(&stored), "updated_at") {
        Err(RunError::CursorFieldMismatch(m)) => assert!(m.contains("modified_at") && m.contains("updated_at"), "{m}"),
        other => panic!("{other:?}"),
    }
    assert_eq!(open_watermark(Some(&stored), "modified_at").unwrap(), Some(&json!(10)));
    assert_eq!(open_watermark(None, "updated_at").unwrap(), None);
}

/// The frontier counts every fetched row, landed or not. A committed position moves forward or holds; an empty or
/// older window never rewinds it.
// spec: run.advance.frontier@6949bad8
#[test]
fn the_position_moves_forward_or_holds() {
    let f = frontier("updated_at", &rows(&[json!(3), json!(9), json!(5)])).unwrap();
    assert_eq!(f, Some(json!(9)));
    assert_eq!(advance(Some(&json!(4)), f.as_ref()).unwrap(), Some(json!(9)));
    // An older window holds, an empty one holds.
    assert_eq!(advance(Some(&json!(12)), Some(&json!(9))).unwrap(), Some(json!(12)));
    assert_eq!(frontier("updated_at", &rows(&[])).unwrap(), None);
    assert_eq!(advance(Some(&json!(12)), None).unwrap(), Some(json!(12)));
    // Instants order as instants, whatever their offset spelling.
    // 09:00 at +08:00 is 01:00 UTC, later than 00:30 UTC though it sorts earlier as text.
    let f = frontier("updated_at", &rows(&[json!("2030-01-01T00:30:00Z"), json!("2030-01-01T09:00:00+08:00")])).unwrap();
    assert_eq!(f, Some(json!("2030-01-01T09:00:00+08:00")));
    let f = frontier("updated_at", &rows(&[json!("2030-01-01T02:00:00Z"), json!("2030-01-01T09:00:00+08:00")])).unwrap();
    assert_eq!(f, Some(json!("2030-01-01T02:00:00Z")));
}

/// A row with no orderable value in the clock field, or a stream switching between text and numeric positions
/// mid-pass, raises `CursorPositionUnorderable`, terminal for the pull.
// spec: run.advance.unorderable-position@f36a2a0c
#[test]
fn an_unorderable_clock_value_refuses_the_pull() {
    for bad in [vec![json!(1), json!(null)], vec![json!(1), json!(true)], vec![json!({"a": 1})], vec![json!(1), json!("2030-01-01T00:00:00Z")]] {
        assert!(matches!(frontier("updated_at", &rows(&bad)), Err(RunError::CursorPositionUnorderable(_))), "{bad:?}");
    }
    let missing = vec![json!({"id": 1}).as_object().unwrap().clone()];
    assert!(matches!(frontier("updated_at", &missing), Err(RunError::CursorPositionUnorderable(_))));
    assert!(matches!(advance(Some(&json!("a")), Some(&json!(1))), Err(RunError::CursorPositionUnorderable(_))));
}

/// A `monotonic` position is concurrent-safe and commits the highest value observed. An `opaque-token` or
/// `snapshot-id` position moves only under a single-writer lease. Last-write-wins governs no cursor.
#[test]
fn concurrent_positions_resolve_by_kind_never_by_recency() {
    let (a, b) = (watermark("updated_at", json!(9)), watermark("updated_at", json!(4)));
    assert_eq!(resolve_concurrent(CursorKind::Monotonic, &a, &b).unwrap(), a);
    assert_eq!(resolve_concurrent(CursorKind::Monotonic, &b, &a).unwrap(), a, "the later write does not win");
    assert!(resolve_concurrent(CursorKind::OpaqueToken, &json!("p2"), &json!("p3")).is_err());
    assert!(resolve_concurrent(CursorKind::SnapshotId, &json!({"lsn": "0/1"}), &json!({"lsn": "0/2"})).is_err());
    assert_eq!(resolve_concurrent(CursorKind::OpaqueToken, &json!("p2"), &json!("p2")).unwrap(), json!("p2"));
}

/// A polled load admits a row whose clock value is at or after the stored position.
#[test]
fn the_boundary_value_is_admitted() {
    assert!(admits(Some(&json!(9)), &json!(9)).unwrap());
    assert!(admits(Some(&json!(9)), &json!(10)).unwrap());
    assert!(!admits(Some(&json!(9)), &json!(8)).unwrap());
    assert!(admits(None, &json!(0)).unwrap());
    assert_eq!(compare(&json!("b"), &json!("a")).unwrap(), std::cmp::Ordering::Greater);
}
