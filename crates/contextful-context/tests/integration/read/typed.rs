//! Binary and vector columns through the read face: their JSON encoding and the masks
//! their types admit.

use super::*;
use contextful_core::store::reconcile::{ColumnType, FloatItem};
use serde_json::json;

const TYPED: &str = r#"
[[pipeline.tables]]
name = "lab/embeddings"

[pipeline.tables.policy.columns]
digest = { strategy = "hash" }
secret = { strategy = "drop" }
hidden = { strategy = "drop" }
"#;

fn types() -> HashMap<String, ColumnType> {
    [
        ("blob", ColumnType::Binary),
        ("digest", ColumnType::FixedSizeBinary(5)),
        ("secret", ColumnType::Binary),
        ("embedding", ColumnType::FixedSizeList(FloatItem::Float16, 3)),
        ("hidden", ColumnType::FixedSizeList(FloatItem::Float32, 2)),
    ]
    .into_iter()
    .map(|(c, t)| (c.to_string(), t))
    .collect()
}

/// A face over the fixture store plus `lab/embeddings`, opened after the table lands so
/// its masks are held to the table's schema.
fn typed_reads(policy: &str) -> Result<Reads, ReadFault> {
    let manifest = format!("{MANIFEST}{policy}");
    let mut r = Reads::with_manifest(&format!("{MANIFEST}{TYPED}"));
    let decl = TableDecl::named("lab/embeddings");
    let rows = json!([
        // "dmFsdWU=" is the bytes of "value".
        {"id": "e1", "blob": "qgE=", "digest": "dmFsdWU=", "secret": "qgE=", "embedding": [0.5, -1.0, 0.25], "hidden": [1.5, 2.0]},
        {"id": "e2", "blob": null, "digest": null, "secret": null, "embedding": null, "hidden": null},
    ]);
    let rows = rows.as_array().unwrap().iter().map(|r| r.as_object().unwrap().clone()).collect();
    let ctx = RunContext {
        node: NodeId::parse("ingest-a").unwrap(),
        injection: Injection { run_id: "run-0001".into(), site_id: "site-a".into(), batch_seq: Some(0), authored_by: None, taint: None },
        committed_at: at("2030-01-10T00:00:00Z"),
    };
    land(&r.store, &decl, &Batch { rows, types: types() }, &ctx).unwrap();
    r.face = Face::open(r.store.clone(), &manifest, pepper())?;
    Ok(r)
}

/// Binary is padded base64, and a fixed-size float array, a vector column included, is a JSON array holding each element as a float cell.
#[test]
fn a_read_renders_bytes_as_base64_and_vectors_as_number_arrays() {
    let r = typed_reads(TYPED).unwrap();
    let s = r.session(&["lab/*"], None, None);
    let rows = r.query(&s, r#"SELECT id, blob, embedding FROM "lab/embeddings" ORDER BY id"#).unwrap();
    assert_eq!(column(&rows, "blob"), [json!("qgE="), Value::Null]);
    assert_eq!(column(&rows, "embedding"), [json!([0.5, -1.0, 0.25]), Value::Null]);
    // A caller's own blob and float array encode the same way; a list of text stays a container.
    let own = r.query(&s, "SELECT '\\xAA\\x01'::BLOB AS b, [1.5, 2.0]::FLOAT[2] AS v, ['a', 'b'] AS l").unwrap();
    assert_eq!(own.rows[0], [json!("qgE="), json!([1.5, 2.0]), json!("[a, b]")]);
    // The rows tool serializes the same projection.
    let all = r.face.rows(&s, "lab/embeddings", None).unwrap();
    assert!(column(&all, "embedding").contains(&json!([0.5, -1.0, 0.25])));
}

/// A binary column masks by `drop` or by `hash` over its bytes, a vector column by `drop` alone; another strategy on either raises `EnforceStrategyOutsideType` at manifest load.
#[test]
fn binary_columns_hash_their_bytes_and_vectors_drop() {
    let r = typed_reads(TYPED).unwrap();
    let s = r.session(&["lab/*"], None, None);
    let rows = r.query(&s, r#"SELECT id, digest, secret, hidden FROM "lab/embeddings" ORDER BY id"#).unwrap();
    // The query layer digests the stored bytes, joining the write layer's digest of the same bytes.
    let written = contextful_policy::enforce::policy::TablePolicy::from_decl(
        &TableDecl::parse_pipeline(TYPED).unwrap().remove(0),
    )
    .unwrap()
    .columns["digest"]
    .mask
    .as_ref()
    .unwrap()
    .apply(&pepper(), Some("dmFsdWU="), ColumnType::FixedSizeBinary(5))
    .unwrap();
    assert_eq!(column(&rows, "digest"), [json!(written), Value::Null]);
    assert_eq!(written, pepper().digest("value"));
    assert_eq!(column(&rows, "secret"), [Value::Null, Value::Null]);
    assert_eq!(column(&rows, "hidden"), [Value::Null, Value::Null]);

    // A strategy the type does not admit refuses the whole manifest at load.
    for policy in [
        "hidden = { strategy = \"hash\" }",
        "embedding = { strategy = \"bucket:5\" }",
        "blob = { strategy = \"tokenize\" }",
        "digest = { strategy = \"truncate:3\" }",
    ] {
        let manifest = format!("\n[[pipeline.tables]]\nname = \"lab/embeddings\"\n\n[pipeline.tables.policy.columns]\n{policy}\n");
        let message = refused_with(typed_reads(&manifest).map(|_| ()), "EnforceStrategyOutsideType");
        assert!(message.contains("lab/embeddings"), "{message}");
    }
}
