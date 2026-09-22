//! `store.lay-out`: the tree a landing and a fold write, and what a read resolves from it.

use crate::support::{at, decl, s, Fixture};
use contextful_context::fold::{fold, prepare, Prepared};
use contextful_context::node::{persisted_node_id, state_dir};
use contextful_context::ContextError;
use contextful_core::store::bound_time::Bounds;
use contextful_core::store::lay_out::RunManifest;
use contextful_core::store::StoreError;
use serde_json::json;
use std::fs;

fn store_err(e: ContextError) -> StoreError {
    e.store().cloned().unwrap_or_else(|| panic!("expected a store refusal, got {e}"))
}

/// A table directory holds `schema.json`, `_pointer.json`, `data/snapshots/<id>/`, `data/runs/<run-id>/<node-id>/` and `requests/`.
#[test]
fn a_landing_and_a_fold_write_the_table_directory() {
    let f = Fixture::new();
    let d = decl("name = \"filings\"");
    f.land(&d, "run-1", json!([{"id": "a"}]), "2030-01-01T00:00:00Z").unwrap();
    fold(&f.store, &d, at("2030-01-01T01:00:00Z")).unwrap();
    let t = f.table_dir("filings");
    assert!(t.join("schema.json").is_file());
    assert!(t.join("_pointer.json").is_file());
    assert!(t.join("data/runs/run-1/ingest-a/part-00000.parquet").is_file());
    assert!(t.join("data/runs/run-1/ingest-a/_manifest.json").is_file());
    let snaps: Vec<_> = fs::read_dir(t.join("data/snapshots")).unwrap().map(|e| e.unwrap().file_name()).collect();
    assert_eq!(snaps.len(), 1);
    assert!(f.store.root().ends_with(".contextful/context/research"));
}

/// A run commits by conditionally creating `_manifest.json` in its node directory, carrying `{run_id, table, node_id, parts, committed_at, pipeline_id?, cursor?, fence?}`, where `node_id` equals the enclosing segment.
// spec: store.lay-out.run-manifest@69595222
#[test]
fn a_run_commits_by_creating_its_manifest_once() {
    let f = Fixture::new();
    let d = decl("name = \"filings\"");
    let m = f.land(&d, "run-1", json!([{"id": "a"}]), "2030-01-01T00:00:00Z").unwrap();
    let path = f.table_dir("filings").join("data/runs/run-1/ingest-a/_manifest.json");
    let on_disk: RunManifest = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    assert_eq!(on_disk, m);
    assert_eq!((on_disk.run_id.as_str(), on_disk.node_id.as_str(), on_disk.table.as_str()), ("run-1", "ingest-a", "filings"));
    assert_eq!(on_disk.parts[0].name, "part-00000.parquet");

    // The create is conditional: a second commit of the run is refused and changes nothing.
    let before = fs::read(&path).unwrap();
    let again = f.land(&d, "run-1", json!([{"id": "b"}]), "2030-01-01T00:01:00Z").unwrap_err();
    assert!(again.to_string().contains("already committed"), "{again}");
    assert_eq!(fs::read(&path).unwrap(), before);

    // A manifest whose `node_id` disagrees with its segment is refused.
    let mut moved = on_disk.clone();
    moved.node_id = "ingest-b".into();
    fs::write(&path, serde_json::to_vec(&moved).unwrap()).unwrap();
    assert!(matches!(store_err(f.scan(&d, Bounds::default()).unwrap_err()), StoreError::StoreManifestUnreadable(_)));
}

/// A node directory holding no `_manifest.json` is in flight and its parts join no file list; a leased pipeline's run also waits for its commit-log entry.
// spec: store.lay-out.uncommitted-run@4dc939ec
#[test]
fn a_run_without_its_manifest_joins_no_file_list() {
    let f = Fixture::new();
    let d = decl("name = \"filings\"");
    f.land(&d, "run-1", json!([{"id": "a"}]), "2030-01-01T00:00:00Z").unwrap();
    f.land(&d, "run-2", json!([{"id": "b"}]), "2030-01-01T00:01:00Z").unwrap();
    // A run in flight: its part is written, its manifest is not.
    fs::remove_file(f.table_dir("filings").join("data/runs/run-2/ingest-a/_manifest.json")).unwrap();
    assert!(f.table_dir("filings").join("data/runs/run-2/ingest-a/part-00000.parquet").is_file());
    let files = f.scan(&d, Bounds::default()).unwrap().files;
    assert_eq!(files, ["tables/filings/data/runs/run-1/ingest-a/part-00000.parquet"]);
    assert_eq!(f.query(&d, Bounds::default(), "SELECT id FROM t"), [[s("a")]]);
}

/// A fold in flight writes under `data/snapshots/<id>.staging/`, and no file list resolves inside it.
// spec: store.lay-out.staging@86b15bd9
#[test]
fn a_staged_snapshot_joins_no_file_list() {
    let f = Fixture::new();
    let d = decl("name = \"filings\"");
    f.land(&d, "run-1", json!([{"id": "a"}]), "2030-01-01T00:00:00Z").unwrap();
    let Prepared::Staged(staged) = prepare(&f.store, &d, at("2030-01-01T01:00:00Z")).unwrap() else { panic!("nothing staged") };
    assert!(staged.staging.to_string_lossy().ends_with(".staging"));
    assert!(staged.staging.join("part-00000.parquet").is_file());
    let files = f.scan(&d, Bounds::default()).unwrap().files;
    assert_eq!(files, ["tables/filings/data/runs/run-1/ingest-a/part-00000.parquet"]);
}

/// `tables/<t>/_pointer.json` names the table's current snapshot and the fence that published it. A snapshot is readable only when the pointer or a chain of `parent` links from it reaches it.
// spec: store.lay-out.table-pointer@800a0cf4
#[test]
fn only_the_pointer_chain_makes_a_snapshot_readable() {
    let f = Fixture::new();
    let d = decl("name = \"filings\"");
    f.land(&d, "run-1", json!([{"id": "a"}]), "2030-01-01T00:00:00Z").unwrap();
    fold(&f.store, &d, at("2030-01-01T01:00:00Z")).unwrap();
    let t = f.table_dir("filings");
    let pointer: serde_json::Value = serde_json::from_slice(&fs::read(t.join("_pointer.json")).unwrap()).unwrap();
    let current = pointer["snapshot_id"].as_str().unwrap().to_string();
    assert!(pointer.as_object().unwrap().contains_key("fence"));

    // A complete snapshot directory the pointer does not reach is read by nobody.
    let orphan = t.join("data/snapshots/snapshot-09999999999999999999");
    fs::create_dir_all(&orphan).unwrap();
    for entry in fs::read_dir(t.join("data/snapshots").join(&current)).unwrap() {
        let p = entry.unwrap().path();
        fs::copy(&p, orphan.join(p.file_name().unwrap())).unwrap();
    }
    let files = f.scan(&d, Bounds::default()).unwrap().files;
    assert_eq!(files, [format!("tables/filings/data/snapshots/{current}/part-00000.parquet")]);

    // Without the pointer, no snapshot is read at all.
    fs::remove_file(t.join("_pointer.json")).unwrap();
    let files = f.scan(&d, Bounds::default()).unwrap().files;
    assert_eq!(files, ["tables/filings/data/runs/run-1/ingest-a/part-00000.parquet"]);
}

/// A run manifest or a reachable snapshot manifest that fails to parse raises `StoreManifestUnreadable`, naming the table and the file, and the table answers no read until it parses.
// spec: store.lay-out.manifest-unreadable@90d0d3f5
#[test]
fn an_unparseable_manifest_refuses_the_table_until_it_parses() {
    let f = Fixture::new();
    let d = decl("name = \"filings\"");
    f.land(&d, "run-1", json!([{"id": "a"}]), "2030-01-01T00:00:00Z").unwrap();
    let run_manifest = f.table_dir("filings").join("data/runs/run-1/ingest-a/_manifest.json");
    let good = fs::read(&run_manifest).unwrap();
    fs::write(&run_manifest, b"{ \"run_id\": ").unwrap();
    match store_err(f.scan(&d, Bounds::default()).unwrap_err()) {
        StoreError::StoreManifestUnreadable(m) => {
            assert!(m.contains("filings") && m.contains("run-1/ingest-a/_manifest.json"), "{m}")
        }
        other => panic!("expected StoreManifestUnreadable, got {other:?}"),
    }
    assert!(fold(&f.store, &d, at("2030-01-01T01:00:00Z")).is_err());
    fs::write(&run_manifest, good).unwrap();

    // A reachable snapshot manifest refuses the same way.
    fold(&f.store, &d, at("2030-01-01T01:00:00Z")).unwrap();
    let (chain, _) = f.store.chain("filings").unwrap();
    let snap_manifest = f.store.snapshot_dir("filings", &chain[0].snapshot_id).unwrap().join("_manifest.json");
    fs::write(&snap_manifest, b"not json").unwrap();
    match store_err(f.scan(&d, Bounds::default()).unwrap_err()) {
        StoreError::StoreManifestUnreadable(m) => assert!(m.contains("filings") && m.contains("_manifest.json"), "{m}"),
        other => panic!("expected StoreManifestUnreadable, got {other:?}"),
    }
}

/// A table's schema is `schema.json` in Arrow JSON form; each arriving batch's schema merges into it and the merged result replaces it.
// spec: store.lay-out.schema-file@14b75c3f
#[test]
fn each_batch_merges_into_schema_json() {
    let f = Fixture::new();
    let d = decl("name = \"filings\"");
    f.land(&d, "run-1", json!([{"id": "a", "n": 1}]), "2030-01-01T00:00:00Z").unwrap();
    f.land(&d, "run-2", json!([{"id": "b", "n": 1.5, "title": "t"}]), "2030-01-01T00:01:00Z").unwrap();
    let doc: serde_json::Value =
        serde_json::from_slice(&fs::read(f.table_dir("filings").join("schema.json")).unwrap()).unwrap();
    let fields: Vec<(&str, &str)> = doc["fields"]
        .as_array()
        .unwrap()
        .iter()
        .map(|f| (f["name"].as_str().unwrap(), f["type"]["name"].as_str().unwrap()))
        .collect();
    assert_eq!(
        fields,
        [
            ("id", "utf8"),
            ("n", "floatingpoint"),
            ("_ingested_at", "timestamp"),
            ("_run_id", "utf8"),
            ("_batch_seq", "int"),
            ("_site_id", "utf8"),
            ("title", "utf8"),
        ]
    );
}

/// A run file is written once and never edited, a snapshot directory is immutable, and a fold writes a new snapshot.
// spec: store.lay-out.immutable-files@bc233544
#[test]
fn a_fold_writes_a_new_snapshot_and_edits_nothing_published() {
    let f = Fixture::new();
    let d = decl("name = \"filings\"");
    f.land(&d, "run-1", json!([{"id": "a"}]), "2030-01-01T00:00:00Z").unwrap();
    let run_part = f.table_dir("filings").join("data/runs/run-1/ingest-a/part-00000.parquet");
    let run_bytes = fs::read(&run_part).unwrap();
    fold(&f.store, &d, at("2030-01-01T01:00:00Z")).unwrap();
    let (first, _) = f.store.chain("filings").unwrap();
    let first_dir = f.store.snapshot_dir("filings", &first[0].snapshot_id).unwrap();
    let first_bytes = fs::read(first_dir.join("part-00000.parquet")).unwrap();

    f.land(&d, "run-2", json!([{"id": "b"}]), "2030-01-01T02:00:00Z").unwrap();
    fold(&f.store, &d, at("2030-01-01T03:00:00Z")).unwrap();
    let (second, _) = f.store.chain("filings").unwrap();
    assert_ne!(second[0].snapshot_id, first[0].snapshot_id);
    assert_eq!(second[0].parent.as_ref(), Some(&first[0].snapshot_id));
    assert_eq!(fs::read(first_dir.join("part-00000.parquet")).unwrap(), first_bytes);
    assert_eq!(fs::read(&run_part).unwrap(), run_bytes);
}

/// A table name no `schema.json` in the tree declares raises `StoreUnknownTable`, never an empty result.
// spec: store.lay-out.unknown-table@68360e2d
#[test]
fn a_table_no_schema_declares_is_unknown() {
    let f = Fixture::new();
    let d = decl("name = \"filings\"");
    f.land(&d, "run-1", json!([{"id": "a"}]), "2030-01-01T00:00:00Z").unwrap();
    f.scan(&d, Bounds::default()).unwrap();
    // A directory without a schema is no table either.
    fs::create_dir_all(f.table_dir("filling").join("data/runs")).unwrap();
    for name in ["filling", "research/filings"] {
        match store_err(f.scan(&decl(&format!("name = \"{name}\"")), Bounds::default()).unwrap_err()) {
            StoreError::StoreUnknownTable(m) => assert!(m.contains(name), "{m}"),
            other => panic!("expected StoreUnknownTable, got {other:?}"),
        }
    }
    assert_eq!(f.store.tables().unwrap(), ["filings"]);
}

/// The `<node-id>` run-path segment and the node id in a ledger filename keep two machines writing one logical run id in disjoint files.
// spec: store.lay-out.node-segment@8cdda228
#[test]
fn two_nodes_writing_one_run_id_land_in_disjoint_files() {
    let f = Fixture::new();
    let d = decl("name = \"filings\"");
    f.land_on(&d, "run-1", "ingest-a", json!([{"id": "a"}]), "2030-01-01T00:00:00Z", &[]).unwrap();
    f.land_on(&d, "run-1", "ingest-b", json!([{"id": "b"}]), "2030-01-01T00:00:01Z", &[]).unwrap();
    let files = f.scan(&d, Bounds::default()).unwrap().files;
    assert_eq!(
        files,
        [
            "tables/filings/data/runs/run-1/ingest-a/part-00000.parquet",
            "tables/filings/data/runs/run-1/ingest-b/part-00000.parquet",
        ]
    );
    assert_eq!(f.query(&d, Bounds::default(), "SELECT id FROM t ORDER BY id"), [[s("a")], [s("b")]]);
}

/// A generated node id persists under `$CONTEXTFUL_STATE_DIR`, else `$XDG_STATE_HOME/contextful`, else the platform user-state directory, outside the store root.
// spec: store.lay-out.node-id-state-path@8e582a0b
#[test]
fn a_generated_node_id_persists_in_the_state_directory() {
    let env = |pairs: &'static [(&'static str, &'static str)]| move |k: &str| pairs.iter().find(|(n, _)| *n == k).map(|(_, v)| v.to_string());
    assert_eq!(
        state_dir(env(&[("CONTEXTFUL_STATE_DIR", "/s"), ("XDG_STATE_HOME", "/x"), ("HOME", "/h")])).unwrap(),
        std::path::Path::new("/s")
    );
    assert_eq!(state_dir(env(&[("XDG_STATE_HOME", "/x"), ("HOME", "/h")])).unwrap(), std::path::Path::new("/x/contextful"));
    let platform = state_dir(env(&[("HOME", "/h")])).unwrap();
    assert!(platform.starts_with("/h") && platform.ends_with("contextful"), "{}", platform.display());

    // Generated once, as `node-<8 hex>`, then read back.
    let dir = tempfile::tempdir().unwrap();
    let id = persisted_node_id(dir.path()).unwrap();
    assert!(id.starts_with("node-") && id.len() == 13 && id[5..].bytes().all(|b| b.is_ascii_hexdigit()), "{id}");
    assert_eq!(persisted_node_id(dir.path()).unwrap(), id);

    // A state directory inside the store root is no source; the configured id wins there.
    let f = Fixture::new();
    let inside = f.store.root().join("state").to_string_lossy().into_owned();
    let (node, _) = contextful_context::node::resolve(&f.store, |k| (k == "CONTEXTFUL_STATE_DIR").then(|| inside.clone())).unwrap();
    assert_eq!(node.as_str(), "local");
    assert!(!f.store.root().join("state").exists());
}
