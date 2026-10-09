//! `connector.export`: the calls a guest offers across the boundary.

use crate::support::{host, loopback, open, open_with};
use contextful_core::run::ports::{Cancellation, Pull, PullRequest, Source};
use contextful_core::run::FailureTag;
use contextful_wasm::batch::rows;
use contextful_wasm::{CursorKind, DataType, GuestSource, Limits};
use serde_json::json;

/// The shared types interface carries a schema (name, fields, primary key) and a field (name, data type, nullability)
/// over `boolean`, `int32`, `int64`, `float64`, `string`, `bytes`, `timestamp-millis` and `json`.
// spec: connector.export.type-taxonomy@c8ce8b76
#[test]
fn a_schema_declares_fields_over_the_eight_scalar_types() {
    let schemas = open().discover().unwrap();
    assert_eq!(schemas.len(), 1);
    let s = &schemas[0];
    assert_eq!(s.name, "items");
    assert_eq!(s.primary_key, vec!["id".to_string()]);
    let types: Vec<DataType> = s.fields.iter().map(|f| f.ty).collect();
    assert_eq!(
        types,
        [
            DataType::Int64,
            DataType::Boolean,
            DataType::Int32,
            DataType::Float64,
            DataType::String,
            DataType::Bytes,
            DataType::TimestampMillis,
            DataType::Json
        ]
    );
    assert!(s.fields.iter().filter(|f| f.nullable).map(|f| f.name.as_str()).eq(["doc"]));
}

/// A batch crosses the boundary as Arrow IPC bytes, and a position as opaque bytes beside a declared cursor kind.
// spec: connector.export.batch-encoding@757b86c0
#[test]
fn a_batch_is_arrow_ipc_and_a_position_is_bytes_beside_a_kind() {
    let mut s = open();
    s.open("items", None).unwrap();
    let batch = s.next().unwrap().expect("a batch");
    assert_eq!(&batch[..4], &[0xff, 0xff, 0xff, 0xff], "an IPC stream opens with a continuation marker");
    let got = rows(&batch).unwrap();
    assert_eq!(got.len(), 2);
    assert_eq!(
        serde_json::Value::Object(got[0].clone()),
        json!({ "id": 1, "flag": false, "small": 10, "ratio": 0.5, "name": "item-1", "blob": "AQ==", "at": 1_700_000_000_001i64, "doc": "{\"n\":1}" })
    );
    let at = s.position().unwrap();
    assert_eq!(at.kind, CursorKind::Monotonic);
    assert_eq!(at.bytes, b"2".to_vec(), "the guest owns the byte format");
    assert!(rows(b"not arrow").is_err_and(|f| f.tag == FailureTag::SchemaIncompatible));
}

/// The host reads an Arrow `Binary` or `FixedSizeBinary` column as bytes of that width and a `FixedSizeList` of
/// `Float32` or `Float16` as that vector, each landing in its {{store.reconcile.binary-and-vector}} type, never as text.
// spec: connector.export.arrow-types@eab47828
#[test]
fn arrow_bytes_and_float_vectors_cross_in_their_own_types() {
    use arrow_array::builder::{FixedSizeListBuilder, Float16Builder, Float32Builder};
    use arrow_array::{ArrayRef, BinaryArray, FixedSizeBinaryArray, RecordBatch};
    use std::sync::Arc;
    let mut half = FixedSizeListBuilder::new(Float16Builder::new(), 3);
    for x in [0.5, -1.0, 0.25] {
        half.values().append_value(half::f16::from_f64(x));
    }
    half.append(true);
    (0..3).for_each(|_| half.values().append_value(half::f16::ZERO));
    half.append(false);
    let mut single = FixedSizeListBuilder::new(Float32Builder::new(), 2);
    single.values().append_slice(&[1.5, 2.0]);
    single.append(true);
    single.values().append_slice(&[3.0, 4.0]);
    single.append(true);
    let columns: Vec<(&str, ArrayRef)> = vec![
        ("blob", Arc::new(BinaryArray::from(vec![Some(&[0xaa_u8, 0x01][..]), None]))),
        ("digest", Arc::new(FixedSizeBinaryArray::try_from_sparse_iter_with_size([Some([0xde_u8, 0xad, 0xbe, 0xef]), Some([0, 1, 2, 3])].into_iter(), 4).unwrap())),
        ("embedding", Arc::new(half.finish())),
        ("wide", Arc::new(single.finish())),
    ];
    let batch = RecordBatch::try_from_iter(columns).unwrap();
    let mut ipc = Vec::new();
    {
        let mut w = arrow_ipc::writer::StreamWriter::try_new(&mut ipc, &batch.schema()).unwrap();
        w.write(&batch).unwrap();
        w.finish().unwrap();
    }
    let read = contextful_wasm::batch::read(&ipc).unwrap();
    assert_eq!(
        read.rows.into_iter().map(serde_json::Value::Object).collect::<Vec<_>>(),
        [
            json!({"blob": "qgE=", "digest": "3q2+7w==", "embedding": [0.5, -1.0, 0.25], "wide": [1.5, 2.0]}),
            json!({"blob": null, "digest": "AAECAw==", "embedding": null, "wide": [3.0, 4.0]}),
        ]
    );
    let types: Vec<(&str, &str)> = read.types.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect();
    assert_eq!(types, [("blob", "binary"), ("digest", "binary(4)"), ("embedding", "float16[3]"), ("wide", "float32[2]")]);

    // The guest source hands the types over beside the rows: the fixture's `bytes` field as `binary`.
    struct Never;
    impl Cancellation for Never {
        fn requested(&self) -> bool {
            false
        }
    }
    let mut source = GuestSource::new(open(), "items");
    let req = PullRequest { step_label: "pull".into(), position: None, idempotency_key: "k".into() };
    let pull = Pull::decode(&source.pull(&req, &Never).unwrap()).unwrap();
    assert_eq!(pull.rows[0]["blob"], "AQ==");
    assert_eq!(pull.types.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect::<Vec<_>>(), [("blob", "binary")]);
}

/// A source exports its cursor kind, a discovery call, and an open call taking a table and an optional position. The
/// handle's `next` yields one batch or exhaustion; its `position` reports where it stands.
// spec: connector.export.read-handle@310c9d46
#[test]
fn a_read_yields_batches_to_exhaustion_and_resumes_from_its_position() {
    let mut s = open();
    assert_eq!(s.cursor_kind().unwrap(), CursorKind::Monotonic);
    s.open("items", None).unwrap();
    assert_eq!(rows(&s.next().unwrap().unwrap()).unwrap().len(), 2);
    let mid = s.position().unwrap();
    assert_eq!(rows(&s.next().unwrap().unwrap()).unwrap().len(), 1);
    assert_eq!(s.next().unwrap(), None, "the read is finite");
    let end = s.position().unwrap();

    s.open("items", Some(&mid)).unwrap();
    let rest = rows(&s.next().unwrap().unwrap()).unwrap();
    assert_eq!(rest.iter().map(|r| r["id"].as_i64().unwrap()).collect::<Vec<_>>(), [3]);
    assert_eq!(s.next().unwrap(), None);
    assert_eq!(s.position().unwrap(), end);

    // The runner's port drives the same handle, one pull per batch.
    struct Never;
    impl Cancellation for Never {
        fn requested(&self) -> bool {
            false
        }
    }
    let mut source = GuestSource::new(open(), "items");
    let mut position = None;
    let mut ids = Vec::new();
    loop {
        let req = PullRequest { step_label: "pull".into(), position: position.clone(), idempotency_key: "k".into() };
        let pull = Pull::decode(&source.pull(&req, &Never).unwrap()).unwrap();
        ids.extend(pull.rows.iter().map(|r| r["id"].as_i64().unwrap()));
        position = pull.cursor;
        if !pull.more {
            break;
        }
    }
    assert_eq!(ids, [1, 2, 3]);
    assert_eq!(position, Some(json!({ "kind": "monotonic", "bytes": "33" })));
}

/// Operator configuration and failure attribution are each an interface in an additional world, probed on the
/// instance by name. A base-world guest instantiates untouched, and neither addition moves the world version.
// spec: connector.export.optional-world@61309f57
#[test]
fn optional_interfaces_are_probed_and_a_base_guest_instantiates_untouched() {
    let (h, probe, base) = host();
    let mut b = h.open(base, loopback(), &Limits::default(), None).unwrap();
    assert!(!b.accepts_config() && !b.attributes_failures());
    assert_eq!(b.failed_partitions().unwrap(), Vec::<String>::new());
    b.open("items", None).unwrap();
    assert!(b.next().unwrap().is_some(), "a base guest reads");

    let mut p = h.open(probe, loopback(), &Limits::default(), None).unwrap();
    assert!(p.accepts_config() && p.attributes_failures());
    let wit = include_str!("../../wit/connector.wit");
    assert!(wit.contains(&format!("package {};", contextful_wasm::WORLD.replace("/source-connector", ""))));
    assert!(wit.contains("world configurable-source-connector {\n  include source-connector;"));
    assert!(wit.contains("world attributing-source-connector {\n  include source-connector;"));
}

/// Failure attribution carries at most 512 entries per session, each value at most 512 B.
// spec: connector.export.attribution-budget@804fe299
#[test]
fn attribution_keeps_512_values_of_at_most_512_bytes() {
    let mut s = open_with(loopback(), &Limits::default(), Some(&json!({ "flood": true }))).unwrap();
    let values = s.failed_partitions().unwrap();
    assert_eq!(values.len(), 512);
    assert!(values.iter().all(|v| v.len() <= 512));
    assert_eq!(values[0], "p0", "the over-long value is dropped, not truncated");
    assert_eq!(values[511], "p511");

    let mut plain = open();
    assert_eq!(plain.failed_partitions().unwrap(), ["tenant-a", "Tenant-A "], "values keep their bytes");
}

/// The destination world declares no host arm, so a guest supplies no destination.
// spec: run.land.no-host-arm@733b9d33
#[test]
fn the_host_binds_the_source_world_and_no_world_it_hosts_reaches_the_destination() {
    let wit = include_str!("../../wit/connector.wit");
    // Each world's body, keyed by name.
    let worlds: Vec<(&str, &str)> = wit
        .split("\nworld ")
        .skip(1)
        .map(|w| {
            let (name, rest) = w.split_once(" {").unwrap();
            (name, rest.split_once('}').unwrap().0)
        })
        .collect();
    let hosted = contextful_wasm::WORLD.split('/').nth(1).unwrap().split('@').next().unwrap();
    assert_eq!(hosted, "source-connector");
    let (_, destination) = worlds.iter().find(|(n, _)| *n == "destination-connector").expect("the destination world is declared");
    assert!(destination.contains("export destination;"));
    for (name, body) in &worlds {
        let reaches = body.contains("destination");
        assert_eq!(reaches, *name == "destination-connector", "`{name}`: only the destination world names the destination interface");
        // The host binds the source world and the worlds that include it, never the destination world.
        let bound = *name == hosted || body.contains(&format!("include {hosted};"));
        assert!(!(bound && reaches), "`{name}` is hosted and reaches the destination");
    }
    assert!(worlds.iter().any(|(n, _)| *n == "configurable-source-connector"));
}
