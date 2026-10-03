//! `store.lay-out`: snapshot ids, part names, manifest shapes and node identity.

use super::at;
use contextful_core::store::lay_out::{
    part_name, resolve_node_id, store_root, NodeId, NodeIdSource, Pointer, RunManifest, SnapshotId, SnapshotManifest,
    NODE_ID_MAX_LEN, SNAPSHOT_ANCESTORS_MAX, SNAPSHOT_ID_WIDTH,
};
use contextful_core::store::index::IndexEntry;
use contextful_core::store::StoreError;

/// A snapshot id is `snapshot-` followed by a nanosecond value zero-padded to 20 chars, equal to the greater of the commit instant and the previous id plus one.
// spec: store.lay-out.snapshot-id@37b97c00
#[test]
fn snapshot_id_is_the_greater_of_the_commit_instant_and_the_previous_plus_one() {
    let first = SnapshotId::next(at("2025-03-15T16:00:00Z"), None);
    assert_eq!(first.to_string(), "snapshot-01742054400000000000");
    assert_eq!(first.to_string().len(), "snapshot-".len() + SNAPSHOT_ID_WIDTH);

    // A clock stepping backwards still yields a later id.
    let second = SnapshotId::next(at("2025-03-15T15:00:00Z"), Some(&first));
    assert_eq!(second.nanos(), first.nanos() + 1);
    assert!(second.to_string() > first.to_string());

    // A later clock wins over previous-plus-one.
    let third = SnapshotId::next(at("2025-03-16T00:00:00Z"), Some(&second));
    assert_eq!(third.to_string(), "snapshot-01742083200000000000");

    // The id round-trips through its JSON form, and a wrong width is no id.
    let json = serde_json::to_string(&third).unwrap();
    assert_eq!(serde_json::from_str::<SnapshotId>(&json).unwrap(), third);
    assert!(serde_json::from_str::<SnapshotId>("\"snapshot-1742083200000000000\"").is_err());
    assert!(serde_json::from_str::<SnapshotId>("\"run-01742083200000000000\"").is_err());
}

/// A data file is `part-<ordinal>.parquet`, the ordinal zero-padded to five digits and unique within its directory.
// spec: store.lay-out.part-name@50056228
#[test]
fn a_part_name_pads_its_ordinal_to_five_digits() {
    assert_eq!(part_name(0), "part-00000.parquet");
    assert_eq!(part_name(42), "part-00042.parquet");
    assert_ne!(part_name(1), part_name(2));
}

/// A project's store sits at `.contextful/context/<project>/`, holding both catalogs, `config.toml`, `cursors/` and one `tables/<t>/` directory per table.
// spec: store.lay-out.store-root@4ddea405
#[test]
fn the_store_root_sits_under_the_project() {
    use contextful_core::store::catalog::{DERIVED_CATALOG_FILE, MACHINE_CATALOG_FILE};
    use contextful_core::store::lay_out::{commit_log_dir, TableLayout, CONFIG_FILE};
    assert_eq!(store_root("research"), ".contextful/context/research");
    // Every entry sits directly under the root, each table in its own directory.
    assert_eq!((DERIVED_CATALOG_FILE, MACHINE_CATALOG_FILE, CONFIG_FILE), ("derived.sqlite", "machine.sqlite", "config.toml"));
    assert!(commit_log_dir("filings-sync", "ingest-a").starts_with("cursors/"));
    assert_eq!(TableLayout::new("filings").dir(), "tables/filings");
    assert_ne!(TableLayout::new("filings").dir(), TableLayout::new("issuers").dir());
}

/// A snapshot's `_manifest.json` holds a `lay_out::SnapshotManifest`, and each entry of `parts` and `indexes` carries its `key_version`.
// spec: store.lay-out.snapshot-manifest@cffbf3df
#[test]
fn the_snapshot_manifest_and_pointer_decode_in_their_documented_shape() {
    let snap: SnapshotManifest = serde_json::from_str(
        r#"{ "snapshot_id": "snapshot-01742054400000000000", "parent": "snapshot-01741968000000000000",
  "table": "filings", "created_at": "2025-03-15T16:00:00Z",
  "includes_runs": ["run-4812/ingest-a", "run-4813/ingest-a", "run-4814/ingest-b"],
  "primary_key": ["document_id", "page"], "order_by": "revised_at", "row_count": 128400,
  "valid_time": { "from": "effective_from", "to": "effective_to" }, "fence": 12, "commit_seq": 41,
  "parts": [{ "name": "part-00000.parquet", "key_version": 3 }],
  "indexes": [{ "kind": "vector", "path": "indexes/vec-embedding-e5-small/zone=all", "table": "filings",
                "snapshot_id": "snapshot-01742054400000000000", "column": "embedding", "id_column": "passage_id",
                "model": "e5-small", "dim": 384, "metric": "cosine", "m": 16, "ef_construction": 200,
                "builder": "contextful-hnsw", "builder_version": 1, "row_count": 128400, "key_version": 3 }] }"#,
    )
    .unwrap();
    assert_eq!(snap.includes_runs.len(), 3);
    assert_eq!(snap.parts[0].key_version, 3);
    match &snap.indexes[0] {
        IndexEntry::Vector(e) => assert_eq!(e.key_version, 3),
        other => panic!("a vector entry reads as {other:?}"),
    }
    assert_eq!(snap.valid_time.as_ref().unwrap().to.as_deref(), Some("effective_to"));
    assert_eq!(snap.fence, Some(12));
    assert_eq!(snap.commit_seq, Some(41));

    let written = serde_json::to_value(&snap).unwrap();
    for key in ["snapshot_id", "parent", "table", "created_at", "includes_runs", "primary_key", "order_by", "row_count", "valid_time", "indexes", "fence", "commit_seq", "parts"] {
        assert!(written.get(key).is_some(), "the manifest writes no `{key}`");
    }

    let ptr: Pointer = serde_json::from_str(r#"{ "snapshot_id": "snapshot-01742054400000000000", "fence": 12 }"#).unwrap();
    assert_eq!(ptr.snapshot_id, snap.snapshot_id);
}

/// A snapshot's `_manifest.json` carries `ancestors`: its parent, then the parent's `ancestors`, at most 256 entries; a
/// root carries none, and a parent without `ancestors` leaves the field absent.
// spec: store.lay-out.ancestors@24f6ae22
#[test]
fn a_snapshot_records_its_ancestors_nearest_first_up_to_the_bound() {
    assert_eq!(SNAPSHOT_ANCESTORS_MAX, 256);
    let root = super::snapshot("2030-01-01T00:00:00Z", None, &[]);
    assert_eq!(SnapshotManifest::ancestors_after(None), Some(vec![]), "a root records an empty ancestry");
    let mut head = SnapshotManifest { ancestors: SnapshotManifest::ancestors_after(None), ..root };
    let first = head.snapshot_id.clone();
    for _ in 0..SNAPSHOT_ANCESTORS_MAX {
        let next = SnapshotManifest {
            snapshot_id: SnapshotId::next(head.created_at, Some(&head.snapshot_id)),
            parent: Some(head.snapshot_id.clone()),
            ancestors: SnapshotManifest::ancestors_after(Some(&head)),
            ..head.clone()
        };
        let recorded = next.ancestors.as_ref().unwrap();
        assert_eq!(recorded[0], head.snapshot_id, "the parent comes first");
        assert!(recorded.len() <= SNAPSHOT_ANCESTORS_MAX);
        head = next;
    }
    let recorded = head.ancestors.as_ref().unwrap();
    assert_eq!(recorded.len(), SNAPSHOT_ANCESTORS_MAX);
    assert_eq!(recorded.last(), Some(&first), "256 ancestors still reach the root");
    let past = SnapshotManifest::ancestors_after(Some(&head)).unwrap();
    assert_eq!(past.len(), SNAPSHOT_ANCESTORS_MAX, "the oldest falls off past the bound");
    assert!(!past.contains(&first));

    // A parent written without the field gives its child none.
    let legacy = super::snapshot("2030-01-02T00:00:00Z", Some(&first), &[]);
    assert_eq!(legacy.ancestors, None);
    assert_eq!(SnapshotManifest::ancestors_after(Some(&legacy)), None);
    let written = serde_json::to_value(&legacy).unwrap();
    assert!(written.get("ancestors").is_none(), "an absent ancestry writes no field");
}

/// A field added to a manifest carries a default value.
// spec: store.lay-out.manifest-default@e14c6d05
#[test]
fn a_manifest_missing_later_fields_decodes_with_defaults() {
    let run: RunManifest = serde_json::from_str(
        r#"{ "run_id": "run-4815", "table": "filings", "node_id": "ingest-a", "committed_at": "2025-03-15T16:00:00Z" }"#,
    )
    .unwrap();
    assert!(run.parts.is_empty());
    assert_eq!((run.pipeline_id, run.cursor, run.fence), (None, None, None));

    let snap: SnapshotManifest = serde_json::from_str(
        r#"{ "snapshot_id": "snapshot-01742054400000000000", "table": "filings", "created_at": "2025-03-15T16:00:00Z" }"#,
    )
    .unwrap();
    assert!(snap.parent.is_none() && snap.includes_runs.is_empty() && snap.parts.is_empty() && snap.indexes.is_empty());
    assert_eq!(snap.row_count, 0);
}

/// A node id longer than 64 chars or outside `^[A-Za-z0-9._-]+$` raises `StoreNodeIdInvalid` at process start, before any path, key or lease carries it.
// spec: store.lay-out.node-id-shape@9672e912
#[test]
fn a_node_id_outside_the_shape_is_invalid() {
    let longest = "a".repeat(NODE_ID_MAX_LEN);
    assert_eq!(NodeId::parse(&longest).unwrap().as_str(), longest);
    assert_eq!(NodeId::parse("ingest-a.eu_1").unwrap().as_str(), "ingest-a.eu_1");
    for bad in ["", &"a".repeat(NODE_ID_MAX_LEN + 1), "ingest/a", "ingest a", "..", "nöde", "a\n"] {
        match NodeId::parse(bad) {
            Err(StoreError::StoreNodeIdInvalid(m)) => assert!(m.contains("^[A-Za-z0-9._-]+$"), "{m}"),
            other => panic!("{bad:?}: expected StoreNodeIdInvalid, got {other:?}"),
        }
    }
    // Every source is held to the shape, the environment included.
    assert!(matches!(
        resolve_node_id(Some("../escape".into()), None, || None),
        Err(StoreError::StoreNodeIdInvalid(_))
    ));
}

/// A process resolves its node id once: `CONTEXTFUL_NODE_ID`, then `[node] id`, then the project default derived from the state directory.
// spec: store.lay-out.node-id-order@2f6bab1d
#[test]
fn the_node_id_resolves_environment_then_configuration_then_state() {
    let never = || -> Option<String> { panic!("the state directory is consulted only when neither source declares an id") };
    let (id, src) = resolve_node_id(Some("from-env".into()), Some("from-config".into()), never).unwrap();
    assert_eq!((id.as_str(), src), ("from-env", NodeIdSource::Environment));
    let (id, src) = resolve_node_id(None, Some("from-config".into()), never).unwrap();
    assert_eq!((id.as_str(), src), ("from-config", NodeIdSource::Configuration));
    let (id, src) = resolve_node_id(None, None, || Some("node-0a1b2c3d".into())).unwrap();
    assert_eq!((id.as_str(), src), ("node-0a1b2c3d", NodeIdSource::StateDirectory));
}

/// A machine with no writable state directory takes the reserved node id `local`.
// spec: store.lay-out.node-id-local@71c79ce9
#[test]
fn no_writable_state_directory_takes_local() {
    let (id, src) = resolve_node_id(None, None, || None).unwrap();
    assert_eq!((id.as_str(), src), ("local", NodeIdSource::Local));
}
