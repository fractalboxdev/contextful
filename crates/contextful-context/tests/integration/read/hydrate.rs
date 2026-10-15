//! Mapping the current snapshot set a read resolves against into memory.

use super::*;
use contextful_context::fold::fold;
use std::path::PathBuf;

/// Every data file a read of `table` resolves against, absolute and sorted.
fn read_set(r: &Reads, table: &str) -> Vec<PathBuf> {
    let state = r.store.state(&r.face.decl(table)).unwrap();
    let dir = r.store.table_dir(table).unwrap();
    state.resolve(None).unwrap().files().iter().map(|f| dir.join(f)).collect()
}

/// The retrieval container syncs the current snapshot set to local disk and memory-maps it; object storage stays out of the per-query path.
// spec: read.cache.hot-local-parquet@44bdb0bf
#[test]
fn the_face_maps_exactly_the_current_snapshot_set_it_reads() {
    let r = Reads::new();
    let decl = TableDecl::parse_pipeline(MANIFEST).unwrap().into_iter().find(|d| d.name == "research/notes").unwrap();
    fold(&r.store, &decl, at("2030-01-11T00:00:00Z")).unwrap();
    land_rows(&r.store, "research/notes", "run-0002", json!([{ "note_id": "n5", "tenant": "acme", "title": "Later note" }]));
    let folded = r.store.table_dir("research/notes").unwrap().join("data/runs/run-0001/ingest-a/part-00000.parquet");
    assert!(folded.is_file(), "the folded run stays on disk beside its snapshot");

    let mapped = r.face.map_snapshot_set().unwrap();

    let mut expected: Vec<PathBuf> = r.face.tables().unwrap().iter().flat_map(|t| read_set(&r, t)).collect();
    expected.sort();
    let files: Vec<PathBuf> = mapped.files().map(|(path, _)| path.to_path_buf()).collect();
    assert_eq!(files, expected, "the mapped set is the set a read resolves against");
    assert!(files.iter().any(|f| f.to_string_lossy().contains("/data/snapshots/")), "the folded snapshot maps: {files:?}");
    assert!(files.iter().any(|f| f.to_string_lossy().contains("/run-0002/")), "an unfolded run maps: {files:?}");
    assert!(!files.contains(&folded), "a run the snapshot folded stays unmapped");
    assert!(!files.iter().any(|f| f.to_string_lossy().contains("hr/salaries/data/snapshots")), "a table without a snapshot maps its runs alone");

    let mut total = 0;
    for (path, bytes) in mapped.files() {
        assert_eq!(bytes, std::fs::read(path).unwrap().as_slice(), "{} maps its bytes on disk", path.display());
        total += bytes.len() as u64;
    }
    assert_eq!(mapped.bytes(), total);
    assert!(total > 0);

    // A mapped file stays readable from memory after its path leaves the disk.
    if cfg!(windows) {
        return;
    }
    let first = std::fs::read(&files[0]).unwrap();
    std::fs::remove_file(&files[0]).unwrap();
    assert_eq!(mapped.files().next().unwrap().1, first.as_slice());
}

/// Paths of `/proc/self/maps` entries naming a deleted file; empty off Linux.
fn deleted_mappings() -> Vec<String> {
    std::fs::read_to_string("/proc/self/maps")
        .map(|maps| maps.lines().filter(|l| l.ends_with("(deleted)")).map(str::to_string).collect())
        .unwrap_or_default()
}

/// A refresh follows the set a read resolves against: a fold drops the runs it folded, a
/// land adds its run, and a collected file's mapping is released with its inode.
#[test]
fn a_refresh_unmaps_what_left_the_set_and_maps_what_joined_it() {
    let r = Reads::new();
    let decl = TableDecl::parse_pipeline(MANIFEST).unwrap().into_iter().find(|d| d.name == "research/notes").unwrap();
    let mut mapped = r.face.map_snapshot_set().unwrap();
    let run = r.store.table_dir("research/notes").unwrap().join("data/runs/run-0001/ingest-a/part-00000.parquet");
    assert!(mapped.files().any(|(p, _)| p == run), "the unfolded run maps");

    fold(&r.store, &decl, at("2030-01-11T00:00:00Z")).unwrap();
    land_rows(&r.store, "research/notes", "run-0002", json!([{ "note_id": "n5", "tenant": "acme", "title": "Later note" }]));
    let refreshed = r.face.refresh_snapshot_set(&mut mapped).unwrap();
    let files: Vec<PathBuf> = mapped.files().map(|(p, _)| p.to_path_buf()).collect();
    let mut expected: Vec<PathBuf> = r.face.tables().unwrap().iter().flat_map(|t| read_set(&r, t)).collect();
    expected.sort();
    assert_eq!(files, expected, "the refreshed set is the set a read resolves against");
    assert!(!files.contains(&run), "the folded run is unmapped");
    assert!(files.iter().any(|f| f.to_string_lossy().contains("/run-0002/")), "the landed run maps");
    assert!(refreshed.released >= 1 && refreshed.mapped >= 2, "{refreshed:?}");

    // A file collected from disk is released at the next refresh, its inode with it.
    let before = deleted_mappings();
    let gone = files.iter().find(|f| f.to_string_lossy().contains("/run-0002/")).unwrap().clone();
    std::fs::remove_file(&gone).unwrap();
    let with_deleted = deleted_mappings();
    if cfg!(target_os = "linux") {
        assert!(with_deleted.iter().any(|l| l.contains(&*gone.to_string_lossy())), "the unlinked file stays mapped until the refresh");
    }
    let refreshed = r.face.refresh_snapshot_set(&mut mapped);
    // The pointer still names the removed run, so the refresh refuses to map it.
    assert!(refreshed.is_err(), "a resolved file missing from disk refuses the refresh");
    let after: Vec<PathBuf> = mapped.files().map(|(p, _)| p.to_path_buf()).collect();
    assert_eq!(after.len(), files.len() - 1, "every other file stays mapped: {after:?}");
    assert!(!after.contains(&gone), "the removed file's mapping is released");
    assert_eq!(deleted_mappings(), before, "no mapping of a deleted file survives the refresh");
}
