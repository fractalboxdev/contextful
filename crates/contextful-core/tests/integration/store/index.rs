//! `store.index`: the sidecar declaration, its shared identifier column and the checks a
//! pass runs before building one.

use contextful_core::pipeline::declare::{read_manifest, ManifestFile};
use contextful_core::run::RunError;
use contextful_core::store::declare::TableDecl;
use contextful_core::store::index::{IndexKind, Metric, DEFAULT_EF_CONSTRUCTION, DEFAULT_M};
use contextful_core::store::reconcile::{Column, ColumnType, FloatItem, Schema};
use contextful_core::store::StoreError;

fn table(block: &str) -> TableDecl {
    TableDecl::parse_pipeline(&format!("[[pipeline.tables]]\n{block}\n")).unwrap().remove(0)
}

fn schema(columns: &[(&str, ColumnType)]) -> Schema {
    Schema { columns: columns.iter().map(|(n, ty)| Column { name: n.to_string(), ty: *ty, nullable: true }).collect() }
}

/// Manifest validation of a pipeline whose one table carries `block`.
fn validate_manifest(block: &str) -> Result<(), RunError> {
    let text = format!(
        "[[pipeline]]\nid = \"docs\"\n[pipeline.source]\nname = \"http\"\nconfig = {{ endpoint = \"https://api.example.test/v1\" }}\n[[pipeline.tables]]\n{block}\n"
    );
    read_manifest(&ManifestFile { path: "contextful.toml".into(), text }).unwrap().remove(0).spec.validate()
}

const VECTOR: &str = "[[pipeline.tables.indexes]]\nkind = \"vector\"\ncolumn = \"embedding\"\nmodel = \"e5-small\"\ndim = 4\n";

/// A sidecar is declared per table under `indexes` with a kind, a `column`, an `id_column` and builder parameters; the vector kind takes `model`, `dim`, the `cosine` metric, `m` and `ef_construction`.
// spec: store.index.declaration@7dc16b5c
#[test]
fn a_vector_sidecar_declares_its_column_identifier_and_builder_parameters() {
    let t = table(
        "name = \"passages\"\n[[pipeline.tables.indexes]]\nkind = \"vector\"\ncolumn = \"embedding\"\nid_column = \"passage_id\"\n\
         model = \"e5-small\"\ndim = 384\nmetric = \"cosine\"\nm = 8\nef_construction = 64\n",
    );
    let idx = &t.indexes()[0];
    assert_eq!((idx.kind, idx.column.as_str(), idx.id_column.as_deref()), (IndexKind::Vector, "embedding", Some("passage_id")));
    assert_eq!((idx.model.as_str(), idx.dim, idx.metric, idx.m(), idx.ef_construction()), ("e5-small", 384, Metric::Cosine, 8, 64));
    assert!(t.canonical().contains("\"indexes\":[{\"kind\":\"vector\""), "{}", t.canonical());

    // The metric and builder parameters default; an unknown metric or key refuses the block.
    let d = table(&format!("name = \"passages\"\nprimary_key = [\"passage_id\"]\n{VECTOR}"));
    assert_eq!((d.indexes()[0].metric, d.indexes()[0].m(), d.indexes()[0].ef_construction()), (Metric::Cosine, DEFAULT_M, DEFAULT_EF_CONSTRUCTION));
    for bad in ["metric = \"l2\"", "graph = \"flat\""] {
        let block = format!("[[pipeline.tables]]\nname = \"passages\"\n{VECTOR}{bad}\n");
        assert!(TableDecl::parse_pipeline(&block).is_err(), "{bad}");
    }
    // A table without a declaration has no sidecar and omits the key.
    let plain = table("name = \"passages\"");
    assert!(plain.indexes().is_empty() && !plain.canonical().contains("indexes"));
}

/// A sidecar's `id_column` defaults to a single-column primary key on a table declaring no `valid_time`, and every sidecar of a table shares it, so keyed, composite-key and unkeyed tables each take one.
// spec: store.index.id-column@7ca40f13
#[test]
fn the_identifier_defaults_to_a_single_key_and_serves_every_table_shape() {
    let keyed = table(&format!("name = \"passages\"\nprimary_key = [\"passage_id\"]\n{VECTOR}"));
    assert_eq!(keyed.id_column().unwrap(), Some("passage_id"));
    let composite = table(&format!("name = \"passages\"\nprimary_key = [\"doc\", \"page\"]\n{VECTOR}id_column = \"passage_id\"\n"));
    assert_eq!(composite.id_column().unwrap(), Some("passage_id"));
    let unkeyed = table(&format!("name = \"passages\"\n{VECTOR}id_column = \"digest\"\n"));
    assert_eq!(unkeyed.id_column().unwrap(), Some("digest"));
    // A second sidecar naming the same column, or defaulting to it, shares it.
    let two = table(&format!(
        "name = \"passages\"\nprimary_key = [\"passage_id\"]\n{VECTOR}[[pipeline.tables.indexes]]\nkind = \"vector\"\ncolumn = \"title_embedding\"\n\
         id_column = \"passage_id\"\nmodel = \"e5-small\"\ndim = 4\n"
    ));
    assert_eq!(two.id_column().unwrap(), Some("passage_id"));
    assert_eq!(table("name = \"passages\"").id_column().unwrap(), None);
    // A valid-time table keeps one row per key and line, so its key is no default.
    let lined = table(&format!(
        "name = \"rates\"\nprimary_key = [\"ccy\"]\n[pipeline.tables.valid_time]\nfrom = \"from_ts\"\n{VECTOR}id_column = \"rate_id\"\n"
    ));
    assert_eq!(lined.id_column().unwrap(), Some("rate_id"));
}

/// A declaration naming no `id_column` on a table without a single-column primary key or declaring `valid_time`, one naming a valid-time table's single key, or two sidecars naming different ones, raises `StoreIndexIdColumnUnresolved` at manifest validation.
// spec: store.index.id-column-unresolved@0ec32ff9
#[test]
fn a_sidecar_without_one_shared_identifier_is_refused() {
    let s = schema(&[("doc", ColumnType::Utf8), ("page", ColumnType::Int64), ("embedding", ColumnType::FixedSizeList(FloatItem::Float32, 4))]);
    for block in [
        format!("name = \"passages\"\n{VECTOR}"),
        format!("name = \"passages\"\nprimary_key = [\"doc\", \"page\"]\n{VECTOR}"),
        format!(
            "name = \"passages\"\nprimary_key = [\"doc\"]\n{VECTOR}[[pipeline.tables.indexes]]\nkind = \"vector\"\ncolumn = \"embedding\"\n\
             id_column = \"page\"\nmodel = \"other\"\ndim = 4\n"
        ),
    ] {
        let t = table(&block);
        assert!(matches!(t.id_column(), Err(StoreError::StoreIndexIdColumnUnresolved(_))), "{block}");
        assert!(matches!(t.validate_indexes(&s), Err(StoreError::StoreIndexIdColumnUnresolved(_))), "{block}");
        // Manifest validation refuses the declaration before any row lands.
        let got = validate_manifest(&block);
        assert!(matches!(got, Err(RunError::Store(StoreError::StoreIndexIdColumnUnresolved(_)))), "{block}: {got:?}");
    }
    // A valid-time table keeps one row per key and line: its key neither defaults nor serves.
    let lined = "name = \"rates\"\nprimary_key = [\"ccy\"]\n[pipeline.tables.valid_time]\nfrom = \"from_ts\"\n";
    for block in [format!("{lined}{VECTOR}"), format!("{lined}{VECTOR}id_column = \"ccy\"\n")] {
        assert!(matches!(table(&block).id_column(), Err(StoreError::StoreIndexIdColumnUnresolved(_))), "{block}");
        let got = validate_manifest(&block);
        assert!(matches!(got, Err(RunError::Store(StoreError::StoreIndexIdColumnUnresolved(_)))), "{block}: {got:?}");
    }
    validate_manifest(&format!("{lined}{VECTOR}id_column = \"rate_id\"\n")).unwrap();
    // The same shapes naming one identifier validate.
    let named = table(&format!("name = \"passages\"\nprimary_key = [\"doc\", \"page\"]\n{VECTOR}id_column = \"doc\"\n"));
    named.validate_indexes(&s).unwrap();
}

/// An index over a column, or naming an `id_column`, the reconciled schema lacks raises `StoreIndexColumnAbsent` at the fold, before the pass that builds it stages anything.
// spec: store.index.column-absent@0d83310b
#[test]
fn an_index_or_identifier_the_schema_lacks_is_refused() {
    let t = table(&format!("name = \"passages\"\nprimary_key = [\"passage_id\"]\n{VECTOR}"));
    let whole = schema(&[("passage_id", ColumnType::Utf8), ("embedding", ColumnType::FixedSizeList(FloatItem::Float16, 4))]);
    t.validate_indexes(&whole).unwrap();
    let no_vector = schema(&[("passage_id", ColumnType::Utf8)]);
    assert!(matches!(t.validate_indexes(&no_vector), Err(StoreError::StoreIndexColumnAbsent(m)) if m.contains("embedding")));
    let named = table(&format!("name = \"passages\"\n{VECTOR}id_column = \"digest\"\n"));
    assert!(matches!(named.validate_indexes(&whole), Err(StoreError::StoreIndexColumnAbsent(m)) if m.contains("digest")));
}

/// A vector sidecar over a column not typed as a vector of its declared `dim`, or an `id_column` typed other than text or integer, raises `StoreIndexColumnType` at manifest validation where `columns` types it, else before a landing's rows land.
// spec: store.index.column-type@716751d8
#[test]
fn a_vector_of_another_width_or_an_unreadable_identifier_is_refused() {
    let t = table(&format!("name = \"passages\"\nprimary_key = [\"passage_id\"]\n{VECTOR}"));
    for (id_ty, vector_ty, refused) in [
        (ColumnType::Utf8, ColumnType::FixedSizeList(FloatItem::Float32, 4), false),
        (ColumnType::Int64, ColumnType::FixedSizeList(FloatItem::Float16, 4), false),
        (ColumnType::Utf8, ColumnType::FixedSizeList(FloatItem::Float32, 3), true),
        (ColumnType::Utf8, ColumnType::Json, true),
        (ColumnType::Float64, ColumnType::FixedSizeList(FloatItem::Float32, 4), true),
        (ColumnType::FixedSizeBinary(32), ColumnType::FixedSizeList(FloatItem::Float32, 4), true),
    ] {
        let s = schema(&[("passage_id", id_ty), ("embedding", vector_ty)]);
        let got = t.validate_indexes(&s);
        assert_eq!(matches!(got, Err(StoreError::StoreIndexColumnType(_))), refused, "{id_ty:?} {vector_ty:?}: {got:?}");
        if !refused {
            got.unwrap();
        }
    }
    // A type `columns` declares refuses at manifest validation, before any row lands.
    for (columns, refused) in [
        ("passage_id = \"text\", embedding = \"float32[4]\"", false),
        ("passage_id = \"int64\"", false),
        ("passage_id = \"float64\"", true),
        ("passage_id = \"binary(32)\"", true),
        ("embedding = \"float32[3]\"", true),
        ("embedding = \"json\"", true),
    ] {
        let got = validate_manifest(&format!("name = \"passages\"\nprimary_key = [\"passage_id\"]\ncolumns = {{ {columns} }}\n{VECTOR}"));
        assert_eq!(matches!(got, Err(RunError::Store(StoreError::StoreIndexColumnType(_)))), refused, "{columns}: {got:?}");
        if !refused {
            got.unwrap();
        }
    }
}
