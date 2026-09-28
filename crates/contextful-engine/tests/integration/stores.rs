//! `run.journal.storage-ports`: every adapter passes the journal, blob and awakeable
//! conformance suites.

use contextful_engine::conformance;
use contextful_engine::stores::{FileAwakeableStore, FileBlobStore, FileJournalStore, MemoryAwakeableStore, MemoryBlobStore, MemoryJournalStore};
use tempfile::TempDir;

/// Fresh file-tree stores, each set under a directory `dirs` keeps alive for the test.
fn file_root(dirs: &mut Vec<TempDir>) -> std::path::PathBuf {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().to_path_buf();
    dirs.push(dir);
    root
}

/// The six suites: the blob, journal and awakeable suites over the file tree and over memory.
fn suites() -> Vec<(&'static str, Box<dyn Fn()>)> {
    vec![
        ("file blob", Box::new(|| {
            let mut dirs = Vec::new();
            conformance::blob_store("file", &mut || FileBlobStore::open(&file_root(&mut dirs)));
        })),
        ("file journal", Box::new(|| {
            let mut dirs = Vec::new();
            conformance::journal_store("file", &mut || {
                let root = file_root(&mut dirs);
                (FileJournalStore::open(&root), FileBlobStore::open(&root))
            });
        })),
        ("file awakeable", Box::new(|| {
            let mut dirs = Vec::new();
            conformance::awakeable_store("file", &mut || {
                let root = file_root(&mut dirs);
                (FileAwakeableStore::open(&root), FileJournalStore::open(&root), FileBlobStore::open(&root))
            });
        })),
        ("memory blob", Box::new(|| conformance::blob_store("memory", &mut MemoryBlobStore::new))),
        ("memory journal", Box::new(|| conformance::journal_store("memory", &mut || (MemoryJournalStore::new(), MemoryBlobStore::new())))),
        ("memory awakeable", Box::new(|| {
            conformance::awakeable_store("memory", &mut || (MemoryAwakeableStore::new(), MemoryJournalStore::new(), MemoryBlobStore::new()))
        })),
    ]
}

/// Journal rows, blobs and awakeables persist through a journal store, a blob store and an awakeable store;
/// every `run.journal` and `run.suspend` clause holds for each adapter, the file tree included.
// spec: run.journal.storage-ports@e71deba8
#[test]
fn the_file_and_memory_adapters_pass_every_store_conformance_suite() {
    let suites = suites();
    let failed: Vec<&str> = suites.iter().filter(|(_, run)| std::panic::catch_unwind(std::panic::AssertUnwindSafe(run)).is_err()).map(|(name, _)| *name).collect();
    contextful_eval::record::emit("journal-conformance", failed.len() as f64, suites.len() as u64, 0);
    assert!(failed.is_empty(), "failed conformance suites: {failed:?}");
}

#[test]
fn a_blob_file_holds_the_whole_value_and_no_staging_file_survives() {
    let dir = tempfile::tempdir().unwrap();
    let blobs = FileBlobStore::open(dir.path());
    let value = vec![b'x'; 4096];
    std::thread::scope(|s| {
        for _ in 0..8 {
            s.spawn(|| contextful_core::run::ports::BlobStore::put(&blobs, "h", &value).unwrap());
        }
    });
    let names: Vec<String> = std::fs::read_dir(blobs.dir()).unwrap().map(|e| e.unwrap().file_name().to_string_lossy().into_owned()).collect();
    assert_eq!(names, ["h"], "no staging file survives beside the blob");
    assert_eq!(std::fs::read(blobs.path("h")).unwrap(), value);
}
