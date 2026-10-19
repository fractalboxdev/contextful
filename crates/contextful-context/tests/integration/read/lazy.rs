//! A read face over a store fetching its parts on first read (`store.pull.lazy-parts`).

use super::*;
use contextful_core::store::object::{PartHold, PartSource};
use contextful_core::store::StoreError;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

/// A source restoring parts from a stash, counting the files it was asked for and the holds alive.
#[derive(Debug, Default)]
struct Stash {
    root: PathBuf,
    parts: Mutex<Vec<(String, Vec<u8>)>>,
    asked: AtomicUsize,
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
        self.asked.fetch_add(files.len(), Ordering::SeqCst);
        for (rel, bytes) in self.parts.lock().unwrap().iter() {
            if files.contains(rel) && !self.root.join(rel).exists() {
                std::fs::write(self.root.join(rel), bytes).unwrap();
            }
        }
        self.live.fetch_add(1, Ordering::SeqCst);
        Ok(PartHold(Arc::new(Live(self.live.clone()))))
    }
}

/// `r`'s face replaced by one over a store fetching through a stash of every part of
/// `research/notes`, each removed from disk.
fn lazy(r: &mut Reads) -> Arc<Stash> {
    let root = r.store.root().to_path_buf();
    let mut parts = Vec::new();
    let mut stack = vec![r.store.table_dir("research/notes").unwrap()];
    while let Some(dir) = stack.pop() {
        for path in std::fs::read_dir(&dir).unwrap().flatten().map(|e| e.path()) {
            if path.is_dir() {
                stack.push(path);
            } else if path.extension().is_some_and(|e| e == "parquet") {
                parts.push((path.strip_prefix(&root).unwrap().to_string_lossy().into_owned(), std::fs::read(&path).unwrap()));
                std::fs::remove_file(&path).unwrap();
            }
        }
    }
    assert!(!parts.is_empty());
    let stash = Arc::new(Stash { root, parts: Mutex::new(parts), ..Stash::default() });
    r.face = Face::open(r.store.clone().with_part_source(stash.clone()), MANIFEST, pepper()).unwrap();
    stash
}

/// A session holds the parts its relations read for as long as it lives, so the cache
/// removes none a registered view still names.
#[test]
fn a_session_holds_its_parts_while_it_lives() {
    let mut r = Reads::new();
    let stash = lazy(&mut r);
    let session = r.session(&["research/notes"], Some(("tenant", "acme")), Some("on-prem:hq"));
    assert!(stash.live.load(Ordering::SeqCst) >= 1, "the open session holds its parts");
    let rows = r.query(&session, "SELECT note_id FROM \"research/notes\"").unwrap();
    assert!(!rows.rows.is_empty());
    assert!(stash.live.load(Ordering::SeqCst) >= 1, "the session still holds them after its statement");
}

/// A lazy face's hydration maps the parts on disk and fetches none.
#[test]
fn a_lazy_face_maps_only_the_parts_on_disk() {
    let mut r = Reads::new();
    let stash = lazy(&mut r);
    let mapped = r.face.map_snapshot_set().unwrap();
    assert_eq!(stash.asked.load(Ordering::SeqCst), 0, "hydration fetches nothing");
    assert!(mapped.files().count() > 0, "every other table's parts are on disk and map");
    assert!(mapped.files().all(|(path, _)| path.exists()));
    assert!(mapped.files().all(|(path, _)| !path.to_string_lossy().contains("research/notes")), "an absent part stays unmapped");
}
