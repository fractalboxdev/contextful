//! The store's `## Shapes` section: each example names the domain type it serializes, and
//! each round-trips through that type unchanged.

use contextful_core::store::commit_log::CommitEntry;
use contextful_core::store::config::StoreConfig;
use contextful_core::store::declare::TableDecl;
use contextful_core::store::index::{IndexEntry, IndexKind};
use contextful_core::store::lay_out::{commit_log_dir, store_root, Pointer, RunManifest, SnapshotManifest, TableLayout, CONFIG_FILE};
use contextful_core::store::lease::BucketLease;
use contextful_core::store::sync::BucketManifest;
use serde::de::DeserializeOwned;
use serde::Serialize;

/// One fenced block of the section: the caption line above it, its language and its body.
struct Block {
    caption: String,
    lang: String,
    body: String,
}

fn shapes() -> Vec<Block> {
    let spec = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/../../spec/10-store.md")).unwrap();
    let section = spec.split("\n## Shapes\n").nth(1).expect("a `## Shapes` section");
    let mut blocks = Vec::new();
    let mut caption = String::new();
    let mut lines = section.lines();
    while let Some(line) = lines.next() {
        if let Some(lang) = line.strip_prefix("```") {
            let body: Vec<&str> = lines.by_ref().take_while(|l| !l.starts_with("```")).collect();
            blocks.push(Block { caption: caption.clone(), lang: lang.to_string(), body: body.join("\n") });
        } else if !line.trim().is_empty() {
            caption = line.to_string();
        }
    }
    blocks
}

/// The backticked `module::Type` names a caption carries.
fn named(caption: &str) -> Vec<String> {
    caption.split('`').skip(1).step_by(2).filter(|s| s.contains("::")).map(str::to_string).collect()
}

fn json_round_trip<T: Serialize + DeserializeOwned>(name: &str, v: &serde_json::Value) {
    let typed: T = serde_json::from_value(v.clone()).unwrap_or_else(|e| panic!("`{name}` does not read {v}: {e}"));
    assert_eq!(&serde_json::to_value(&typed).unwrap(), v, "`{name}` does not write back what it read");
}

fn toml_round_trip<T: Serialize + DeserializeOwned>(name: &str, v: &toml::Value) {
    let typed: T = v.clone().try_into().unwrap_or_else(|e| panic!("`{name}` does not read {v}: {e}"));
    assert_eq!(&toml::Value::try_from(&typed).unwrap(), v, "`{name}` does not write back what it read");
}

/// The full paths the store-root half of the tree draws, its comments column dropped.
fn tree_paths(body: &str) -> Vec<String> {
    let mut stack: Vec<(usize, String)> = Vec::new();
    let mut out = Vec::new();
    for line in body.lines() {
        let indent = line.len() - line.trim_start().len();
        let Some(entry) = line.split_whitespace().next() else { continue };
        if indent == 0 && !entry.starts_with(".contextful/") {
            stack.clear();
            continue;
        }
        while stack.last().is_some_and(|(i, _)| *i >= indent) {
            stack.pop();
        }
        let path = match stack.last() {
            Some((_, parent)) => format!("{parent}/{}", entry.trim_end_matches('/')),
            None if indent == 0 => entry.trim_end_matches('/').to_string(),
            None => continue,
        };
        out.push(path.clone());
        stack.push((indent, path));
    }
    out
}

/// Every example under `## Shapes` names the domain type it serializes, and reading it into
/// that type and writing it back yields the example unchanged.
#[test]
fn every_store_shape_example_round_trips_through_the_type_it_names() {
    let blocks = shapes();
    assert!(blocks.len() >= 5, "the section holds its examples");
    let mut seen = Vec::new();
    for b in &blocks {
        let names = named(&b.caption);
        assert!(!names.is_empty(), "the example under `{}` names no type", b.caption);
        match b.lang.as_str() {
            "json" => {
                let values: Vec<serde_json::Value> =
                    serde_json::Deserializer::from_str(&b.body).into_iter().map(|v| v.unwrap_or_else(|e| panic!("{}: {e}", b.caption))).collect();
                assert_eq!(values.len(), names.len(), "`{}` names one type per example", b.caption);
                for (name, v) in names.iter().zip(&values) {
                    match name.as_str() {
                        "lay_out::RunManifest" => json_round_trip::<RunManifest>(name, v),
                        "lay_out::SnapshotManifest" => json_round_trip::<SnapshotManifest>(name, v),
                        "lay_out::Pointer" => json_round_trip::<Pointer>(name, v),
                        "lease::BucketLease" => json_round_trip::<BucketLease>(name, v),
                        "commit_log::CommitEntry" => json_round_trip::<CommitEntry>(name, v),
                        "sync::BucketManifest" => json_round_trip::<BucketManifest>(name, v),
                        other => panic!("`{other}` is no store shape type"),
                    }
                    seen.push(name.clone());
                }
            }
            "toml" => {
                let doc: toml::Value = toml::from_str(&b.body).unwrap_or_else(|e| panic!("{}: {e}", b.caption));
                assert_eq!(names.len(), 1, "`{}` names one type", b.caption);
                match names[0].as_str() {
                    "declare::TableDecl" => {
                        let tables = doc["pipeline"]["tables"].as_array().expect("a `[[pipeline.tables]]` block");
                        for t in tables {
                            toml_round_trip::<TableDecl>(&names[0], t);
                        }
                    }
                    "config::StoreConfig" => toml_round_trip::<StoreConfig>(&names[0], &doc),
                    other => panic!("`{other}` is no store shape type"),
                }
                seen.push(names[0].clone());
            }
            _ => {
                assert_eq!(names, ["lay_out::TableLayout"], "the tree names the layout type");
                seen.push(names[0].clone());
            }
        }
    }
    for t in [
        "lay_out::TableLayout",
        "declare::TableDecl",
        "lay_out::RunManifest",
        "lay_out::SnapshotManifest",
        "lay_out::Pointer",
        "lease::BucketLease",
        "commit_log::CommitEntry",
        "sync::BucketManifest",
        "config::StoreConfig",
    ] {
        assert!(seen.iter().any(|s| s == t), "no example shows `{t}`");
    }
}

/// A table directory holds `schema.json`, `_pointer.json`, `data/snapshots/<id>/`, `data/runs/<run-id>/<node-id>/` and `requests/`.
// spec: store.lay-out.table-directory@9779ac78
#[test]
fn the_table_layout_draws_every_path_of_the_tree() {
    let tree = shapes().into_iter().find(|b| b.lang.is_empty()).expect("the store tree");
    let drawn = tree_paths(&tree.body);
    let root = store_root("<project>");
    let t = TableLayout::new("<t>");
    let id = "snapshot-01742054400000000000";
    for path in [
        t.dir(),
        t.schema(),
        t.pointer(),
        t.run("<run-id>", "<node-id>"),
        t.run_manifest("<run-id>", "<node-id>"),
        t.snapshot(id),
        t.snapshot_manifest(id),
        t.staging("<id>"),
        t.requests(),
        commit_log_dir("<pipeline-id>", "<node-id>"),
        CONFIG_FILE.to_string(),
    ] {
        let full = format!("{root}/{path}");
        assert!(drawn.contains(&full), "the tree draws no `{full}`; it draws {drawn:#?}");
    }
    assert_eq!(t.staging(id), format!("tables/<t>/data/snapshots/{id}.staging"));
}

/// An `indexes` entry whose kind or fields this build does not read parses as unrecognised: the manifest stays readable, the entry answers no probe, and a rewrite keeps it byte for byte.
// spec: store.lay-out.unrecognised-index-entry@0fb5a1dc
#[test]
fn snapshot_index_entries_are_typed_and_an_unrecognised_entry_survives() {
    let v = serde_json::json!({
        "snapshot_id": "snapshot-01742054400000000000", "table": "filings", "created_at": "2025-03-15T16:00:00Z",
        "indexes": [
            { "kind": "fulltext", "path": "indexes/fts-body-cjk", "table": "filings",
              "snapshot_id": "snapshot-01742054400000000000", "column": "body", "id_column": "passage_id",
              "tokenizer": "cjk", "builder": "contextful-postings", "builder_version": 1, "row_count": 10,
              "term_count": 40, "key_version": 0 },
            { "kind": "sparse", "path": "indexes/sparse-body", "column": "body", "key_version": 2 }
        ]
    });
    let m: SnapshotManifest = serde_json::from_value(v.clone()).unwrap();
    match &m.indexes[0] {
        IndexEntry::Fulltext(e) => assert_eq!((e.column.as_str(), e.term_count), ("body", 40)),
        other => panic!("a full-text entry reads as {other:?}"),
    }
    assert_eq!(m.indexes[0].kind(), Some(IndexKind::Fulltext));
    assert!(matches!(m.indexes[1], IndexEntry::Unrecognized(_)));
    assert_eq!(m.indexes[1].kind(), None);
    assert_eq!((m.indexes[1].path(), m.indexes[1].column()), (Some("indexes/sparse-body"), Some("body")));
    assert_eq!(serde_json::to_value(&m).unwrap()["indexes"], v["indexes"]);

    // The entry's keys run out of sorted order, so only a verbatim copy keeps them.
    let unrecognised = r#"{"kind":"sparse","path":"indexes/sparse-body","column":"body","key_version":2}"#;
    let text = format!(
        r#"{{"snapshot_id":"snapshot-01742054400000000000","table":"filings","created_at":"2025-03-15T16:00:00Z","indexes":[{unrecognised}]}}"#
    );
    let m: SnapshotManifest = serde_json::from_str(&text).unwrap();
    assert!(matches!(m.indexes[0], IndexEntry::Unrecognized(_)));
    let written = serde_json::to_string(&m).unwrap();
    assert!(written.contains(unrecognised), "a rewrite reorders the entry: {written}");
    let pretty = String::from_utf8(serde_json::to_vec_pretty(&m).unwrap()).unwrap();
    assert!(pretty.contains(unrecognised), "a pretty rewrite reorders the entry: {pretty}");
}
