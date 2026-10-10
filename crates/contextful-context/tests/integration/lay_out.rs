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

/// A run commits by conditionally creating `_manifest.json` in its node directory, holding a `lay_out::RunManifest` whose `node_id` equals the enclosing segment.
// spec: store.lay-out.run-manifest@0afb7e8e
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
    assert_eq!(on_disk.commit_seq, Some(1));

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

/// Every file a table directory holds, with its bytes.
fn tree(dir: &std::path::Path) -> Vec<(std::path::PathBuf, Vec<u8>)> {
    let mut out = Vec::new();
    for e in fs::read_dir(dir).unwrap() {
        let p = e.unwrap().path();
        if p.is_dir() {
            out.extend(tree(&p));
        } else {
            out.push((p.clone(), fs::read(&p).unwrap()));
        }
    }
    out.sort();
    out
}

/// A landing of an unlogged run already committed on its node, carrying equal rows and position, writes nothing and answers the committed manifest as a replay.
// spec: store.lay-out.run-replay@be05ec54
#[cfg(feature = "read")]
#[test]
fn a_relanded_run_with_equal_rows_answers_the_committed_manifest() {
    let f = Fixture::new();
    let d = decl("name = \"filings\"\nprimary_key = [\"id\"]");
    let rows = json!([{"id": "a", "n": 1}, {"id": "b", "body": {"k": [1, 2]}}]);
    let first = f.land(&d, "run-1", rows.clone(), "2030-01-01T00:00:00Z").unwrap();
    let before = tree(&f.table_dir("filings"));

    // A retry after a lost acknowledgement carries a later clock and the same rows.
    let again = f.land(&d, "run-1", rows, "2030-01-01T00:05:00Z").unwrap();
    assert_eq!(again, first);
    assert_eq!(tree(&f.table_dir("filings")), before);
    assert_eq!(f.query(&d, Bounds::default(), "SELECT count(*) FROM t"), vec![vec![s("2")]]);
}

/// Any other landing of a run id already committed on its node raises `StoreRunConflict` and changes nothing.
// spec: store.lay-out.run-conflict@b9255d81
#[test]
fn a_relanded_run_with_other_rows_or_a_logged_position_conflicts() {
    use contextful_context::land::{land_batches, Batch, Position, RunContext};
    use contextful_core::store::lay_out::NodeId;
    use contextful_core::store::reserve::Injection;
    let f = Fixture::new();
    let d = decl("name = \"filings\"");
    f.land(&d, "run-1", json!([{"id": "a"}]), "2030-01-01T00:00:00Z").unwrap();
    let before = tree(&f.table_dir("filings"));
    for rows in [json!([{"id": "b"}]), json!([{"id": "a"}, {"id": "a"}]), json!([{"id": "a", "extra": 1}])] {
        let e = f.land(&d, "run-1", rows, "2030-01-01T00:01:00Z").unwrap_err();
        assert!(matches!(store_err(e), StoreError::StoreRunConflict(_)));
        assert_eq!(tree(&f.table_dir("filings")), before);
    }

    // A logged run's readability rides its commit-log entry, so its re-landing conflicts even with equal rows.
    let ctx = RunContext {
        node: NodeId::parse("ingest-a").unwrap(),
        injection: Injection { run_id: "run-l".into(), site_id: "site-a".into(), batch_seq: None, authored_by: None, taint: None },
        committed_at: at("2030-01-01T00:02:00Z"),
    };
    let position = Position { pipeline_id: Some("feed".into()), cursor: Some(json!("p1")), fence: Some(1), logged: true, replace_frontier: false };
    let batch = Batch { rows: vec![json!({"id": "c"}).as_object().unwrap().clone()], types: Default::default() };
    land_batches(&f.store, &d, std::slice::from_ref(&batch), &ctx, &position, &|| Ok(())).unwrap();
    let e = land_batches(&f.store, &d, std::slice::from_ref(&batch), &ctx, &position, &|| Ok(())).unwrap_err();
    assert!(matches!(store_err(e), StoreError::StoreRunConflict(_)));

    // An unlogged run re-landed at another cursor conflicts.
    let position = Position { pipeline_id: Some("feed".into()), cursor: Some(json!("p1")), fence: None, logged: false, replace_frontier: false };
    let ctx = RunContext { injection: Injection { run_id: "run-u".into(), ..ctx.injection.clone() }, ..ctx };
    land_batches(&f.store, &d, std::slice::from_ref(&batch), &ctx, &position, &|| Ok(())).unwrap();
    land_batches(&f.store, &d, std::slice::from_ref(&batch), &ctx, &position, &|| Ok(())).unwrap();
    let moved = Position { cursor: Some(json!("p2")), ..position };
    let e = land_batches(&f.store, &d, std::slice::from_ref(&batch), &ctx, &moved, &|| Ok(())).unwrap_err();
    assert!(matches!(store_err(e), StoreError::StoreRunConflict(_)));
}

#[cfg(feature = "read")]
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
            ("_row_seq", "int"),
            ("_commit_seq", "int"),
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

#[cfg(feature = "read")]
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

/// The host id, a random `node-<8 hex>` generated once, persists under `$CONTEXTFUL_STATE_DIR`, else `$XDG_STATE_HOME/contextful`, else the platform user-state directory, outside the store root.
// spec: store.lay-out.node-id-state-path@83cd89ab
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
    assert!(!id[5..].is_empty(), "the exclusion below ranges over no element");
    assert!(id.starts_with("node-") && id.len() == 13 && id[5..].bytes().all(|b| b.is_ascii_hexdigit()), "{id}");
    assert_eq!(persisted_node_id(dir.path()).unwrap(), id);

    // A state directory inside the store root is no source; the configured id wins there.
    let f = Fixture::new();
    let inside = f.store.root().join("state").to_string_lossy().into_owned();
    let (node, _) = contextful_context::node::resolve(&f.store, |k| (k == "CONTEXTFUL_STATE_DIR").then(|| inside.clone())).unwrap();
    assert_eq!(node.as_str(), "local");
    assert!(!f.store.root().join("state").exists());
}

/// `node::resolve` derives a stable `node-<8 hex>` per store root, distinct from the host id, and a declared `[node] id` wins over it.
#[test]
fn two_store_roots_on_one_host_take_distinct_node_ids() {
    let host = tempfile::tempdir().unwrap();
    let state = host.path().to_string_lossy().into_owned();
    let env = |k: &str| (k == "CONTEXTFUL_STATE_DIR").then(|| state.clone());
    let a = tempfile::tempdir().unwrap();
    let b = tempfile::tempdir().unwrap();
    let id = |dir: &std::path::Path, project: &str| {
        let store = contextful_context::store::Store::open(dir, project).unwrap();
        let (node, src) = contextful_context::node::resolve(&store, env).unwrap();
        assert_eq!(src, contextful_core::store::lay_out::NodeIdSource::StateDirectory);
        node.as_str().to_string()
    };
    let research = id(a.path(), "research");
    assert!(!research[5..].is_empty(), "the exclusion below ranges over no element");
    assert!(research.starts_with("node-") && research.len() == 13 && research[5..].bytes().all(|b| b.is_ascii_hexdigit()), "{research}");
    // Stable across resolutions, distinct per project and per checkout, none equal to the host id.
    assert_eq!(id(a.path(), "research"), research);
    fs::create_dir_all(a.path().join(".contextful/context/research")).unwrap();
    assert_eq!(id(a.path(), "research"), research);
    let others = [id(a.path(), "filings"), id(b.path(), "research")];
    let host_id = persisted_node_id(host.path()).unwrap();
    assert!(!others.is_empty(), "the exclusion below ranges over no element");
    assert!(others.iter().all(|o| *o != research && *o != host_id), "{research} {others:?} {host_id}");
    assert_ne!(others[0], others[1]);

    // A declared `[node] id` still wins over the derived one.
    let declared = a.path().join(".contextful/context/declared");
    fs::create_dir_all(&declared).unwrap();
    fs::write(declared.join("config.toml"), "[node]\nid = \"ingest-a\"\n").unwrap();
    let store = contextful_context::store::Store::open(a.path(), "declared").unwrap();
    assert_eq!(contextful_context::node::resolve(&store, env).unwrap().0.as_str(), "ingest-a");
}

#[cfg(feature = "read")]
/// A run is its run id on one node: a second node committing a folded run id lands a run no snapshot holds.
#[test]
fn a_run_id_folded_on_one_node_leaves_the_other_nodes_run_unfolded() {
    let f = Fixture::new();
    let d = decl("name = \"filings\"\nretain_runs = \"1d\"");
    f.land_on(&d, "run-1", "ingest-a", json!([{"id": "a"}]), "2030-01-01T00:00:00Z", &[]).unwrap();
    fold(&f.store, &d, at("2030-01-01T01:00:00Z")).unwrap();
    f.land_on(&d, "run-1", "ingest-b", json!([{"id": "b"}]), "2030-01-01T02:00:00Z", &[]).unwrap();
    assert_eq!(f.query(&d, Bounds::default(), "SELECT id FROM t ORDER BY id"), [[s("a")], [s("b")]]);
    fold(&f.store, &d, at("2030-01-01T03:00:00Z")).unwrap();
    assert_eq!(f.query(&d, Bounds::default(), "SELECT id FROM t ORDER BY id"), [[s("a")], [s("b")]]);
    // Retention collects what was folded, and the rows stay readable.
    f.land_on(&d, "run-2", "ingest-a", json!([{"id": "c"}]), "2030-01-03T00:00:00Z", &[]).unwrap();
    fold(&f.store, &d, at("2030-01-03T04:00:00Z")).unwrap();
    assert_eq!(f.query(&d, Bounds::default(), "SELECT id FROM t ORDER BY id"), [[s("a")], [s("b")], [s("c")]]);
}

/// A manifest whose `parent` returns to a snapshot the walk already passed refuses the
/// table with `StoreManifestUnreadable`, rather than walking the chain without end.
#[test]
fn a_parent_chain_that_does_not_terminate_refuses_the_table() {
    let f = Fixture::new();
    let d = decl("name = \"events\"");
    f.land(&d, "run-1", json!([{"e": 1}]), "2030-01-01T00:00:00Z").unwrap();
    fold(&f.store, &d, at("2030-01-01T01:00:00Z")).unwrap();
    let (chain, _) = f.store.chain("events").unwrap();
    let id = chain[0].snapshot_id.clone();

    // The snapshot becomes its own parent.
    let path = f.store.snapshot_dir("events", &id).unwrap().join("_manifest.json");
    let mut m: serde_json::Value = serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
    m["parent"] = json!(id.to_string());
    fs::write(&path, serde_json::to_vec_pretty(&m).unwrap()).unwrap();

    // The walk terminates, so the answer arrives within the wait rather than never.
    let (tx, rx) = std::sync::mpsc::channel();
    let store = f.store.clone();
    std::thread::spawn(move || {
        let _ = tx.send(store.chain("events").map_err(store_err));
    });
    match rx.recv_timeout(std::time::Duration::from_secs(10)) {
        Ok(Err(StoreError::StoreManifestUnreadable(_))) => {}
        Ok(other) => panic!("expected StoreManifestUnreadable, got {other:?}"),
        Err(_) => panic!("the parent chain did not terminate within 10 s"),
    }
}

/// A table name carries `/`, so a table directory still holds tables below it: every
/// landed table appears in the listing, whatever nests under whatever.
#[test]
fn a_table_nested_under_another_table_is_still_listed() {
    let f = Fixture::new();
    for name in ["a", "a/b", "a/b/c", "plain"] {
        f.land(&decl(&format!("name = \"{name}\"")), "run-1", json!([{"e": 1}]), "2030-01-01T00:00:00Z").unwrap();
    }
    assert_eq!(f.store.tables().unwrap(), ["a", "a/b", "a/b/c", "plain"]);
}

#[cfg(feature = "read")]
/// Two landings of one run id on one node leave a run whose part holds exactly the rows
/// its manifest accounts for: one commits, the other is refused, and neither rewrites
/// the part the committed manifest names.
#[test]
fn two_landings_of_one_run_on_one_node_leave_the_committed_rows_intact() {
    let f = std::sync::Arc::new(Fixture::new());
    let d = decl("name = \"events\"");
    for round in 0..16 {
        let run = format!("run-{round}");
        let handles: Vec<_> = [1_i64, 2]
            .into_iter()
            .map(|tag| {
                let (f, d, run) = (f.clone(), d.clone(), run.clone());
                std::thread::spawn(move || {
                    let rows = json!([{"id": "x", "tag": tag}, {"id": "y", "tag": tag}]);
                    f.land(&d, &run, rows, "2030-01-01T00:00:00Z").map(|m| (tag, m))
                })
            })
            .collect();
        let landed: Vec<_> = handles.into_iter().filter_map(|h| h.join().unwrap().ok()).collect();
        assert_eq!(landed.len(), 1, "round {round}: both landings of `{run}` committed");

        // The committed manifest's rows are the rows on disk, so the read is its tag alone.
        let (tag, manifest) = &landed[0];
        assert_eq!(manifest.parts.len(), 1);
        let rows = f.query(&d, Bounds::default(), &format!("SELECT DISTINCT tag FROM t WHERE _run_id = '{run}'"));
        assert_eq!(rows, [[s(&tag.to_string())]], "round {round}: the part holds rows its manifest does not account for");
    }
}

/// A table name segment the table directory's own layout uses refuses, so no table nests
/// inside another table's data, ledger or files, and a nested table stays listed.
#[test]
fn a_table_name_colliding_with_the_table_layout_refuses() {
    let f = Fixture::new();
    f.land(&decl("name = \"a\""), "run-1", json!([{"e": 1}]), "2030-01-01T00:00:00Z").unwrap();
    for name in ["a/data", "a/data/runs", "a/requests", "data", "a/schema.json", "a/_pointer.json", "a/_pointer.json.lock", "a/.x.tmp"] {
        let err = f.store.table_dir(name).expect_err(name);
        assert!(matches!(err, ContextError::Invalid(_)), "{name}: {err}");
        let landed = f.land(&decl(&format!("name = \"{name}\"")), "run-2", json!([{"e": 1}]), "2030-01-01T00:00:00Z");
        assert!(landed.is_err(), "{name} landed");
    }
    assert_eq!(f.store.tables().unwrap(), ["a"]);
    assert_eq!(f.store.committed_runs("a").unwrap().len(), 1);
    assert!(f.store.table_dir("a/database").is_ok());
}

/// A body a source lands by reference is the file `blobs/<sha256>` under the store root, named by the SHA-256 of its bytes and written whole through a rename before the run naming it commits.
// spec: store.lay-out.landed-blob@17566f61
#[test]
fn a_landed_body_is_one_file_under_blobs_named_by_its_digest() {
    use sha2::Digest;
    let f = Fixture::new();
    let body = b"%PDF-1.4 a quarterly plan".to_vec();
    let sha: String = sha2::Sha256::digest(&body).iter().map(|b| format!("{b:02x}")).collect();
    assert_eq!(f.store.blob(&sha).unwrap(), None, "no body holds the digest before it lands");
    f.store.land_blob(&sha, &body).unwrap();
    assert_eq!(f.store.blob_path(&sha), f.store.root().join("blobs").join(&sha));
    assert_eq!(fs::read(f.store.blob_path(&sha)).unwrap(), body);
    assert_eq!(f.store.blob(&sha).unwrap(), Some(body.clone()));
    // A second landing of one body leaves the stored file as it stands.
    f.store.land_blob(&sha, &body).unwrap();
    let held: Vec<String> = fs::read_dir(f.store.root().join("blobs")).unwrap().map(|e| e.unwrap().file_name().to_string_lossy().into_owned()).collect();
    assert_eq!(held, std::slice::from_ref(&sha), "no staging file survives beside the blob");
    // A name other than the bytes' digest lands nothing.
    let wrong = "0".repeat(64);
    assert!(f.store.land_blob(&wrong, &body).is_err());
    assert!(!f.store.blob_path(&wrong).exists());
}
