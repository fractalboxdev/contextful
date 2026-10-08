//! `run.journal.storage-ports`: every adapter passes the journal, blob and awakeable
//! conformance suites.

use contextful_core::run::journal::EntryKey;
use contextful_core::run::ports::{BlobStore, Cancellation, JournalStore};
use contextful_core::run::{Failure, FailureTag};
use contextful_engine::conformance;
use contextful_engine::Journal;
use contextful_engine::stores::{FileAwakeableStore, FileBlobStore, FileJournalStore, MemoryAwakeableStore, MemoryBlobStore, MemoryJournalStore};
use tempfile::TempDir;

struct Never;

impl Cancellation for Never {
    fn requested(&self) -> bool {
        false
    }
}

/// Fresh file-tree stores, each set under a directory `dirs` keeps alive for the test.
fn file_root(dirs: &mut Vec<TempDir>) -> std::path::PathBuf {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().to_path_buf();
    dirs.push(dir);
    root
}

/// The blob, journal and awakeable suites over the file tree and over memory, and the
/// restart check over the file tree, the one durable adapter here.
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
        ("file awakeable restart", Box::new(|| {
            let mut dirs = Vec::new();
            let root = file_root(&mut dirs);
            conformance::awakeable_restart("file", &mut || (FileAwakeableStore::open(&root), FileJournalStore::open(&root), FileBlobStore::open(&root)));
        })),
        ("memory blob", Box::new(|| conformance::blob_store("memory", &mut MemoryBlobStore::new))),
        ("memory journal", Box::new(|| conformance::journal_store("memory", &mut || (MemoryJournalStore::new(), MemoryBlobStore::new())))),
        ("shared dynamic journal", Box::new(|| conformance::journal_store("shared dynamic", &mut || {
            let rows: std::sync::Arc<dyn JournalStore> = std::sync::Arc::new(MemoryJournalStore::new());
            let blobs: std::sync::Arc<dyn BlobStore> = std::sync::Arc::new(MemoryBlobStore::new());
            (rows, blobs)
        }))),
        ("shared dynamic blob", Box::new(|| conformance::blob_store("shared dynamic", &mut || {
            let blobs: std::sync::Arc<dyn BlobStore> = std::sync::Arc::new(MemoryBlobStore::new());
            blobs
        }))),
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

/// `JournalStore::executions` lists only an execution holding a row: a released claim and
/// an unjournaled step leave none behind, over the file tree and over memory.
#[test]
fn an_execution_whose_every_claim_was_released_lists_under_no_adapter() {
    fn check<J: JournalStore + Clone, B: BlobStore + Clone>(adapter: &str, rows: J, blobs: B) {
        let j = Journal::over(rows, blobs);
        let key = |execution: &str| EntryKey::new(execution, "pull-0", b"null");
        j.step(&key("x-1"), "run-a", &|_: &str| Ok(true), &Never, &|_: &[u8]| false, &mut || Ok(b"{}".to_vec())).unwrap();
        assert!(j.step(&key("x-2"), "run-a", &|_: &str| Ok(true), &Never, &|_: &[u8]| true, &mut || Err(Failure::new(FailureTag::Transient, "reset"))).is_err());
        assert_eq!(j.row_store().executions().unwrap(), Vec::<String>::new(), "{adapter}: an execution holding no row lists");
    }
    let dir = tempfile::tempdir().unwrap();
    check("file", FileJournalStore::open(dir.path()), FileBlobStore::open(dir.path()));
    check("memory", MemoryJournalStore::new(), MemoryBlobStore::new());
}
