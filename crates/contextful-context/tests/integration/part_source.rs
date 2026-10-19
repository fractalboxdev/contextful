//! A store whose reads fetch each absent part through a part source (`store.pull.lazy-parts`).

use crate::support::{decl, Fixture};
use contextful_context::rows::table_rows;
use contextful_context::scan::scan;
use contextful_context::Store;
use contextful_core::store::bound_time::Bounds;
use contextful_core::store::object::{PartHold, PartSource};
use contextful_core::store::StoreError;
use serde_json::json;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

/// A source restoring parts from a stash, and counting the holds still alive.
#[derive(Debug, Default)]
struct Stash {
    root: PathBuf,
    parts: Mutex<Vec<(String, Vec<u8>)>>,
    asked: Mutex<Vec<String>>,
    live: Arc<AtomicUsize>,
}

struct Live(Arc<AtomicUsize>);

impl Drop for Live {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}

impl PartSource for Stash {
    fn hold(&self, files: &[String]) -> Result<PartHold, StoreError> {
        self.asked.lock().unwrap().extend(files.iter().cloned());
        for (rel, bytes) in self.parts.lock().unwrap().iter() {
            if files.contains(rel) && !self.root.join(rel).exists() {
                std::fs::write(self.root.join(rel), bytes).unwrap();
            }
        }
        self.live.fetch_add(1, Ordering::SeqCst);
        Ok(PartHold(Arc::new(Live(self.live.clone()))))
    }
}

/// Every Parquet part of `table`, relative to the store root, removed from disk with its bytes.
fn remove_parts(f: &Fixture, table: &str) -> Vec<(String, Vec<u8>)> {
    let root = f.store.root().to_path_buf();
    let mut out = Vec::new();
    let mut stack = vec![f.table_dir(table)];
    while let Some(dir) = stack.pop() {
        for path in std::fs::read_dir(&dir).unwrap().flatten().map(|e| e.path()) {
            if path.is_dir() {
                stack.push(path);
            } else if path.extension().is_some_and(|e| e == "parquet") {
                let rel = path.strip_prefix(&root).unwrap().to_string_lossy().into_owned();
                out.push((rel, std::fs::read(&path).unwrap()));
                std::fs::remove_file(&path).unwrap();
            }
        }
    }
    out
}

/// A scan hands its file list to the store's part source before anything opens a part, holds
/// it while the scan lives, and a store opened at a registered root reads through that source.
#[test]
fn a_scan_holds_its_parts_through_the_part_source_and_open_finds_a_registered_one() {
    let f = Fixture::new();
    let d = decl("name = \"filings\"");
    f.land(&d, "run-1", json!([{ "id": 1 }, { "id": 2 }]), "2030-01-01T00:00:00Z").unwrap();
    let parts = remove_parts(&f, "filings");
    assert_eq!(parts.len(), 1);
    let stash = Arc::new(Stash { root: f.store.root().to_path_buf(), parts: Mutex::new(parts.clone()), ..Stash::default() });

    let lazy = f.store.clone().with_part_source(stash.clone());
    {
        let s = scan(&lazy, &d, Bounds::default()).unwrap();
        assert_eq!(s.files, [parts[0].0.clone()]);
        assert!(s.hold.is_some());
        assert_eq!(stash.live.load(Ordering::SeqCst), 1, "the scan holds its parts");
    }
    assert_eq!(stash.live.load(Ordering::SeqCst), 0, "the hold drops with the scan");

    std::fs::remove_file(f.store.root().join(&parts[0].0)).unwrap();
    let project_dir = f.store.root().ancestors().nth(3).unwrap().to_path_buf();
    contextful_context::store::register_part_source(f.store.root(), stash.clone());
    let reopened = Store::open(&project_dir, "research").unwrap();
    let mut ids: Vec<i64> = table_rows(&reopened, &d, &["id"]).unwrap().iter().map(|r| r["id"].as_i64().unwrap()).collect();
    ids.sort_unstable();
    assert_eq!(ids, [1, 2]);
    assert_eq!(stash.asked.lock().unwrap().len(), 2, "both reads asked the source");
}
