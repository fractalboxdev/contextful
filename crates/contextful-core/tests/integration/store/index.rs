//! `store.index`: the sidecar declaration, its shared identifier column and the checks a
//! pass runs before building one.

use contextful_core::pipeline::declare::{read_manifest, ManifestFile};
use contextful_core::run::RunError;
use contextful_core::store::declare::TableDecl;
use contextful_core::store::lay_out::SnapshotManifest;
use contextful_core::store::index::{
    is_cjk, IndexEntry, IndexKind, Metric, Tokenizer, DEFAULT_EF_CONSTRUCTION, DEFAULT_M, FULLTEXT_BUILDER,
    FULLTEXT_BUILDER_VERSION, VECTOR_BUILDER, VECTOR_BUILDER_VERSION,
};
use contextful_core::store::reconcile::{Column, ColumnType, FloatItem, Schema};
use contextful_core::store::StoreError;

fn table(block: &str) -> TableDecl {
    TableDecl::parse_pipeline(&format!("[[pipeline.tables]]\n{block}\n")).unwrap().remove(0)
}

fn schema(columns: &[(&str, ColumnType)]) -> Schema {
    Schema { columns: columns.iter().map(|(n, ty)| Column { name: n.to_string(), ty: ty.clone(), nullable: true }).collect(), ..Default::default() }
}

/// Manifest validation of a pipeline whose one table carries `block`.
fn validate_manifest(block: &str) -> Result<(), RunError> {
    let text = format!(
        "[[pipeline]]\nid = \"docs\"\n[pipeline.source]\nname = \"http\"\nconfig = {{ endpoint = \"https://api.example.test/v1\" }}\n[[pipeline.tables]]\n{block}\n"
    );
    read_manifest(&ManifestFile { path: "contextful.toml".into(), text }).unwrap().remove(0).spec.validate()
}

const VECTOR: &str = "[[pipeline.tables.indexes]]\nkind = \"vector\"\ncolumn = \"embedding\"\nmodel = \"e5-small\"\ndim = 4\n";

const FULLTEXT: &str = "[[pipeline.tables.indexes]]\nkind = \"fulltext\"\ncolumn = \"body\"\n";

/// A sidecar is declared per table under `indexes` with a kind, a `column`, an `id_column` and builder parameters; the `vector` kind takes `model`, `dim`, the `cosine` metric, `m` and `ef_construction`, the `fulltext` kind a `tokenizer`.
// spec: store.index.declaration@bffdbef1
#[test]
fn each_sidecar_kind_declares_its_column_identifier_and_builder_parameters() {
    let t = table(
        "name = \"passages\"\n[[pipeline.tables.indexes]]\nkind = \"vector\"\ncolumn = \"embedding\"\nid_column = \"passage_id\"\n\
         model = \"e5-small\"\ndim = 384\nmetric = \"cosine\"\nm = 8\nef_construction = 64\n",
    );
    let idx = &t.indexes()[0];
    assert_eq!((idx.kind, idx.column.as_str(), idx.id_column.as_deref()), (IndexKind::Vector, "embedding", Some("passage_id")));
    assert_eq!((idx.model(), idx.dim(), idx.metric(), idx.m(), idx.ef_construction()), ("e5-small", 384, Metric::Cosine, 8, 64));
    assert!(t.canonical().contains("\"indexes\":[{\"kind\":\"vector\""), "{}", t.canonical());

    // The metric and builder parameters default; an unknown metric or key refuses the block.
    let d = table(&format!("name = \"passages\"\nprimary_key = [\"passage_id\"]\n{VECTOR}"));
    assert_eq!((d.indexes()[0].metric(), d.indexes()[0].m(), d.indexes()[0].ef_construction()), (Metric::Cosine, DEFAULT_M, DEFAULT_EF_CONSTRUCTION));
    for bad in ["metric = \"l2\"", "graph = \"flat\"", "tokenizer = \"cjk\""] {
        let block = format!("[[pipeline.tables]]\nname = \"passages\"\n{VECTOR}{bad}\n");
        assert!(TableDecl::parse_pipeline(&block).is_err(), "{bad}");
    }
    // A vector block without its model or dimension refuses.
    for missing in ["kind = \"vector\"\ncolumn = \"embedding\"\ndim = 4", "kind = \"vector\"\ncolumn = \"embedding\"\nmodel = \"e5\""] {
        let block = format!("[[pipeline.tables]]\nname = \"passages\"\n[[pipeline.tables.indexes]]\n{missing}\n");
        assert!(TableDecl::parse_pipeline(&block).is_err(), "{missing}");
    }

    // The fulltext kind takes a tokenizer, `unicode` unless declared, and no vector key.
    let f = table(&format!("name = \"passages\"\nprimary_key = [\"passage_id\"]\n{FULLTEXT}tokenizer = \"cjk\"\n"));
    let idx = &f.indexes()[0];
    assert_eq!((idx.kind, idx.column.as_str(), idx.tokenizer()), (IndexKind::Fulltext, "body", Tokenizer::Cjk));
    assert!(f.canonical().contains("{\"kind\":\"fulltext\",\"column\":\"body\",\"tokenizer\":\"cjk\"}"), "{}", f.canonical());
    let plain = table(&format!("name = \"passages\"\nprimary_key = [\"passage_id\"]\n{FULLTEXT}"));
    assert_eq!(plain.indexes()[0].tokenizer(), Tokenizer::Unicode);
    for bad in ["model = \"e5\"", "dim = 4", "metric = \"cosine\"", "m = 8", "ef_construction = 64", "tokenizer = \"whitespace\""] {
        let block = format!("[[pipeline.tables]]\nname = \"passages\"\n{FULLTEXT}{bad}\n");
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

/// An `id_column` typed other than text or integer, a vector sidecar's column other than a vector of its `dim`, or a full-text sidecar's other than text raises `StoreIndexColumnType` at manifest validation where `columns` types it, else before any row lands.
// spec: store.index.column-type@5a53304a
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
        let s = schema(&[("passage_id", id_ty.clone()), ("embedding", vector_ty.clone())]);
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
    // A full-text sidecar reads text alone.
    let f = table(&format!("name = \"passages\"\nprimary_key = [\"passage_id\"]\n{FULLTEXT}"));
    for (body_ty, refused) in [(ColumnType::Utf8, false), (ColumnType::Int64, true), (ColumnType::Json, true), (ColumnType::Binary, true)] {
        let got = f.validate_indexes(&schema(&[("passage_id", ColumnType::Utf8), ("body", body_ty.clone())]));
        assert_eq!(matches!(got, Err(StoreError::StoreIndexColumnType(_))), refused, "{body_ty:?}: {got:?}");
    }
    for (columns, refused) in [("body = \"text\"", false), ("body = \"int64\"", true), ("body = \"binary\"", true)] {
        let got = validate_manifest(&format!("name = \"passages\"\nprimary_key = [\"passage_id\"]\ncolumns = {{ {columns} }}\n{FULLTEXT}"));
        assert_eq!(matches!(got, Err(RunError::Store(StoreError::StoreIndexColumnType(_)))), refused, "{columns}: {got:?}");
    }
    let absent = schema(&[("passage_id", ColumnType::Utf8)]);
    assert!(matches!(f.validate_indexes(&absent), Err(StoreError::StoreIndexColumnAbsent(m)) if m.contains("body")));
}

/// A `tokenizer` is `unicode`, the default, indexing each lowercased alphanumeric run as one term, or `cjk`, which indexes each Han, Kana or Hangul stretch of a run as overlapping character bigrams.
// spec: store.index.tokenizer@8d0fa491
#[test]
fn the_cjk_tokenizer_indexes_unspaced_stretches_as_bigrams() {
    let strings = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
    // Unicode: lowercased alphanumeric runs, whatever their script.
    assert_eq!(Tokenizer::Unicode.terms("Solar-Battery, CAFÉ 2030"), strings(&["solar", "battery", "café", "2030"]));
    assert_eq!(Tokenizer::Unicode.terms("メニューの設定画面を開く"), strings(&["メニューの設定画面を開く"]));
    // CJK: each Han, Kana or Hangul stretch as overlapping bigrams, the rest of a run whole.
    assert_eq!(Tokenizer::Cjk.terms("設定画面を開く"), strings(&["設定", "定画", "画面", "面を", "を開", "開く"]));
    assert_eq!(Tokenizer::Cjk.terms("iPhone設定"), strings(&["iphone", "設定"]));
    assert_eq!(Tokenizer::Cjk.terms("電池 x 電"), strings(&["電池", "x", "電"]));
    assert_eq!(Tokenizer::Cjk.terms("한국어 텍스트"), strings(&["한국", "국어", "텍스", "스트"]));
    assert_eq!(Tokenizer::Cjk.terms("Solar battery"), Tokenizer::Unicode.terms("Solar battery"));
    for c in ['漢', 'ひ', 'カ', 'ｶ', '한', 'ᄀ', '\u{20000}'] {
        assert!(is_cjk(c), "{c}");
    }
    for c in ['a', 'é', '1', 'ß', 'Ж'] {
        assert!(!is_cjk(c), "{c}");
    }
}

/// Two sidecar declarations of one table resolving to one path under `store.index.paths` raise `StoreIndexPathCollision`, naming both, at manifest validation and before the fold builds either.
// spec: store.index.path-collision@458056c1
#[test]
fn two_declarations_resolving_to_one_path_are_refused() {
    let vector = |column: &str, model: &str, m: u32| {
        format!("[[pipeline.tables.indexes]]\nkind = \"vector\"\ncolumn = \"{column}\"\nmodel = \"{model}\"\ndim = 4\nm = {m}\n")
    };
    let v4 = ColumnType::FixedSizeList(FloatItem::Float32, 4);
    for (block, columns) in [
        // One column and model declared twice, differing only in a builder parameter.
        (format!("{}{}", vector("embedding", "e5", 4), vector("embedding", "e5", 8)), vec!["embedding"]),
        // `-` inside a column or a model aliases across the column/model boundary.
        (format!("{}{}", vector("a-b", "c", 4), vector("a", "b-c", 4)), vec!["a-b", "a"]),
        // One column and tokenizer declared twice.
        (format!("{FULLTEXT}{FULLTEXT}"), vec![]),
    ] {
        let block = format!("name = \"passages\"\nprimary_key = [\"passage_id\"]\n{block}");
        let mut cols = vec![("passage_id", ColumnType::Utf8), ("body", ColumnType::Utf8)];
        cols.extend(columns.iter().map(|c| (*c, v4.clone())));
        let got = table(&block).validate_indexes(&schema(&cols));
        match got {
            Err(StoreError::StoreIndexPathCollision(m)) => assert!(m.contains("indexes[0]") && m.contains("indexes[1]"), "{m}"),
            other => panic!("{block}: expected StoreIndexPathCollision, got {other:?}"),
        }
        let got = validate_manifest(&block);
        assert!(matches!(got, Err(RunError::Store(StoreError::StoreIndexPathCollision(_)))), "{block}: {got:?}");
    }
    // Distinct paths validate: two models over one column, and both kinds over one column.
    let block = format!(
        "name = \"passages\"\nprimary_key = [\"passage_id\"]\n{}{}{FULLTEXT}{FULLTEXT}tokenizer = \"cjk\"\n",
        vector("embedding", "e5", 4),
        vector("embedding", "bge", 4)
    );
    table(&block)
        .validate_indexes(&schema(&[("passage_id", ColumnType::Utf8), ("body", ColumnType::Utf8), ("embedding", v4.clone())]))
        .unwrap();
    validate_manifest(&block).unwrap();
}

/// The snapshot manifest example in `spec/10-store.md` reads each `indexes` entry as the
/// typed entry its kind names, recorded by this build's builder.
#[test]
fn the_spec_snapshot_manifest_example_deserializes_its_index_entries() {
    let spec = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/../../spec/10-store.md")).unwrap();
    let after = spec.split("\n## Shapes\n").nth(1).and_then(|s| s.split("`lay_out::SnapshotManifest`").nth(1)).expect("the manifest examples");
    let block = after.split("```json").nth(1).and_then(|b| b.split("```").next()).expect("a json block");
    let manifest = serde_json::Deserializer::from_str(block)
        .into_iter::<serde_json::Value>()
        .map(Result::unwrap)
        .find(|v| v.get("indexes").is_some())
        .expect("a snapshot manifest carrying indexes");
    let manifest: SnapshotManifest = serde_json::from_value(manifest).unwrap();
    let (mut vectors, mut fulltexts) = (0, 0);
    for entry in &manifest.indexes {
        match entry {
            IndexEntry::Vector(e) => {
                assert_eq!((e.builder.as_str(), e.builder_version), (VECTOR_BUILDER, VECTOR_BUILDER_VERSION));
                assert_eq!((e.table.as_str(), e.snapshot_id.clone()), (manifest.table.as_str(), manifest.snapshot_id.to_string()));
                vectors += 1;
            }
            IndexEntry::Fulltext(e) => {
                assert_eq!((e.builder.as_str(), e.builder_version), (FULLTEXT_BUILDER, FULLTEXT_BUILDER_VERSION));
                assert_eq!((e.table.as_str(), e.snapshot_id.clone()), (manifest.table.as_str(), manifest.snapshot_id.to_string()));
                fulltexts += 1;
            }
            IndexEntry::Unrecognized(v) => panic!("an unrecognised entry {v}"),
        }
    }
    assert_eq!((vectors, fulltexts), (1, 1));
}

/// Planning warns, naming the column, where a partition specification projects more than 1000 partitions or a
/// median partition below 16 MiB.
// spec: store.index.partition-warnings@ab32a372
#[test]
fn a_partition_plan_past_either_bound_warns_naming_the_column() {
    use contextful_core::store::index::{partition_warnings, PARTITION_COUNT_WARN, PARTITION_MEDIAN_WARN_BYTES};
    assert_eq!((PARTITION_COUNT_WARN, PARTITION_MEDIAN_WARN_BYTES), (1000, 16 * 1024 * 1024));
    let by = vec!["tenant".to_string(), "day".to_string()];
    let big = PARTITION_MEDIAN_WARN_BYTES;
    // Within both bounds: no warning.
    assert!(partition_warnings(&by, &vec![big; 1000]).is_empty());
    // More than 1000 partitions.
    let many = partition_warnings(&by, &vec![big; 1001]);
    assert_eq!(many.len(), 1);
    assert!(many[0].contains("`tenant`") && many[0].contains("`day`") && many[0].contains("1001 partitions"), "{many:?}");
    // A median partition below 16 MiB, though the mean clears it.
    let small = partition_warnings(&by, &[1, 2, big * 10]);
    assert_eq!(small.len(), 1);
    assert!(small[0].contains("`tenant`") && small[0].contains("median"), "{small:?}");
    assert!(partition_warnings(&by, &[big - 1, big, big + 1]).is_empty());
    // No partition specification, nothing to warn about.
    assert!(partition_warnings(&[], &[1]).is_empty());
}

/// An entry counts as built under a declaration only when its identifier and builder
/// parameters match it, so a changed declaration rebuilds (`store.index.declared-unbuilt`).
#[test]
fn an_entry_is_built_as_a_declaration_only_under_its_identifier_and_parameters() {
    let vector: IndexEntry = serde_json::from_value(serde_json::json!({
        "kind": "vector", "path": "indexes/vec-embedding-e5-small/zone=all", "table": "passages",
        "snapshot_id": "snapshot-01742054400000000000", "column": "embedding", "id_column": "passage_id",
        "model": "e5-small", "dim": 384, "metric": "cosine", "m": DEFAULT_M, "ef_construction": DEFAULT_EF_CONSTRUCTION,
        "builder": VECTOR_BUILDER, "builder_version": VECTOR_BUILDER_VERSION, "row_count": 2, "key_version": 0
    }))
    .unwrap();
    let fulltext: IndexEntry = serde_json::from_value(serde_json::json!({
        "kind": "fulltext", "path": "indexes/fts-body-cjk", "table": "passages",
        "snapshot_id": "snapshot-01742054400000000000", "column": "body", "id_column": "passage_id", "tokenizer": "cjk",
        "builder": FULLTEXT_BUILDER, "builder_version": FULLTEXT_BUILDER_VERSION, "row_count": 2, "term_count": 3, "key_version": 0
    }))
    .unwrap();
    let decl = |block: &str| {
        table(&format!("name = \"passages\"\nprimary_key = [\"passage_id\"]\n[[pipeline.tables.indexes]]\n{block}")).indexes()[0].clone()
    };
    let vec_block = "kind = \"vector\"\ncolumn = \"embedding\"\nmodel = \"e5-small\"\ndim = 384\n";
    let fts_block = "kind = \"fulltext\"\ncolumn = \"body\"\ntokenizer = \"cjk\"\n";
    assert!(vector.built_as(&decl(vec_block), "passage_id"));
    assert!(fulltext.built_as(&decl(fts_block), "passage_id"));
    // Another identifier, another graph parameter, or another kind is not the declared sidecar.
    assert!(!vector.built_as(&decl(vec_block), "digest"));
    assert!(!fulltext.built_as(&decl(fts_block), "digest"));
    assert!(!vector.built_as(&decl(&format!("{vec_block}m = 24\n")), "passage_id"));
    assert!(!vector.built_as(&decl(&format!("{vec_block}ef_construction = 400\n")), "passage_id"));
    assert!(!fulltext.built_as(&decl(vec_block), "passage_id"));
    // An entry a newer builder records is left as written.
    let unrecognized: IndexEntry = serde_json::from_value(serde_json::json!({"kind": "sparse", "path": "indexes/sparse-body"})).unwrap();
    assert!(unrecognized.built_as(&decl(fts_block), "digest"));
}

/// A sidecar extended by a segment declares `2.0` even when the segment it extended held no
/// row, since a `1.x` build reads no `extensions` field (`store.lay-out.format-version`).
#[test]
fn an_extended_sidecar_without_segments_declares_the_current_format() {
    use contextful_core::store::lay_out::{FormatVersion, PartEntry};
    let entry = |extensions: u32| -> IndexEntry {
        serde_json::from_value(serde_json::json!({
            "kind": "fulltext", "path": "indexes/fts-body-cjk", "table": "passages",
            "snapshot_id": "snapshot-01742054400000000000", "column": "body", "id_column": "passage_id", "tokenizer": "cjk",
            "builder": FULLTEXT_BUILDER, "builder_version": FULLTEXT_BUILDER_VERSION, "extensions": extensions,
            "row_count": 2, "term_count": 3, "key_version": 0
        }))
        .unwrap()
    };
    let parts = [PartEntry::new("part-00000.parquet", 0)];
    assert!(entry(1).segments().is_empty());
    assert_eq!(FormatVersion::of_manifest(&parts, &[entry(0)]).to_string(), "1.0");
    assert_eq!(FormatVersion::of_manifest(&parts, &[entry(1)]).to_string(), "2.0");
}

/// A sidecar recording builder version 1 carries no per-entry availability instant, so the
/// next pass rebuilds it in full and a past horizon reads it filtered
/// (`store.index.rebuild`, `read.horizon.rank-statistics`).
#[test]
fn a_sidecar_without_per_entry_availability_rebuilds() {
    let entry = |kind: &str, version: u32| -> IndexEntry {
        let mut e = serde_json::json!({
            "kind": kind, "path": "indexes/x", "table": "passages", "snapshot_id": "snapshot-01742054400000000000",
            "column": "body", "id_column": "passage_id", "builder_version": version, "row_count": 2, "key_version": 0
        });
        let extra = if kind == "vector" {
            serde_json::json!({"model": "e5-small", "dim": 384, "metric": "cosine", "m": DEFAULT_M, "ef_construction": DEFAULT_EF_CONSTRUCTION, "builder": VECTOR_BUILDER})
        } else {
            serde_json::json!({"tokenizer": "unicode", "term_count": 3, "builder": FULLTEXT_BUILDER})
        };
        e.as_object_mut().unwrap().extend(extra.as_object().unwrap().clone());
        serde_json::from_value(e).unwrap()
    };
    for kind in ["vector", "fulltext"] {
        assert!(entry(kind, 1).needs_rebuild(), "{kind}");
    }
    assert!(!entry("vector", VECTOR_BUILDER_VERSION).needs_rebuild());
    assert!(!entry("fulltext", FULLTEXT_BUILDER_VERSION).needs_rebuild());
}
