//! Object transfers held to a fixed width (`store.pull.transfer-width`), and the
//! machine-local digest record that spares an unchanged file its rehash
//! (`store.pull.digest-record`).

use crate::sync::Result;
use contextful_core::run::journal::sha256_hex;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Mutex;

/// Object requests a pull or a push keeps in flight at once: 16 requests (`store.pull.transfer-width`).
pub const TRANSFER_WIDTH: usize = 16;

/// Run `f` over `items` with up to `TRANSFER_WIDTH` in flight, answering in item order. After
/// the first refusal no item starts, each one in flight finishes, and the refusal of the
/// earliest item returns.
pub(crate) fn each<T: Sync, R: Send>(items: &[T], f: impl Fn(&T) -> Result<R> + Sync) -> Result<Vec<R>> {
    if items.len() <= 1 {
        return items.iter().map(f).collect();
    }
    let next = AtomicUsize::new(0);
    let stop = AtomicBool::new(false);
    let done: Mutex<Vec<(usize, Result<R>)>> = Mutex::new(Vec::with_capacity(items.len()));
    std::thread::scope(|scope| {
        for _ in 0..TRANSFER_WIDTH.min(items.len()) {
            scope.spawn(|| {
                while !stop.load(Ordering::SeqCst) {
                    let i = next.fetch_add(1, Ordering::SeqCst);
                    let Some(item) = items.get(i) else { break };
                    let outcome = f(item);
                    if outcome.is_err() {
                        stop.store(true, Ordering::SeqCst);
                    }
                    done.lock().unwrap_or_else(|e| e.into_inner()).push((i, outcome));
                }
            });
        }
    });
    let mut done = done.into_inner().unwrap_or_else(|e| e.into_inner());
    done.sort_by_key(|(i, _)| *i);
    done.into_iter().map(|(_, r)| r).collect()
}

/// What marks a file unchanged: its size, modification and change times, inode and device.
/// Setting a file's modification time moves its change time, so a rewrite that restores
/// the size and modification time still reads as changed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
struct Stamp {
    size: u64,
    modified: (i64, i64),
    changed: (i64, i64),
    inode: u64,
    device: u64,
}

/// A file's stamp and the sha256 it hashed to.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
struct Recorded {
    stamp: Stamp,
    sha256: String,
}

/// A local file's digest and size, and the bytes read to compute it: none when its record stood.
pub(crate) struct Digest {
    pub(crate) sha256: String,
    pub(crate) size: u64,
    pub(crate) read: u64,
}

/// The digests of a store's files as last hashed or written on this machine, keyed by path
/// under the store root. It lives beside the store in `.contextful/sync/<project>/digests.json`
/// and never syncs.
pub(crate) struct DigestRecord {
    path: PathBuf,
    entries: Mutex<BTreeMap<String, Recorded>>,
    dirty: AtomicBool,
}

/// The stamp of the file at `path`: `Ok(None)` where none is there, `Ok(Some(None))` on a
/// platform without change times and inodes, where no record stands for a file.
fn stamp(path: &Path) -> std::io::Result<Option<Option<Stamp>>> {
    let meta = match std::fs::metadata(path) {
        Ok(m) => m,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e),
    };
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        Ok(Some(Some(Stamp {
            size: meta.len(),
            modified: (meta.mtime(), meta.mtime_nsec()),
            changed: (meta.ctime(), meta.ctime_nsec()),
            inode: meta.ino(),
            device: meta.dev(),
        })))
    }
    #[cfg(not(unix))]
    {
        let _ = meta;
        Ok(Some(None))
    }
}

impl DigestRecord {
    /// The record of the store at `root` under `project`; an absent or unreadable record
    /// opens empty, so every file hashes once.
    pub(crate) fn open(root: &Path, project: &str) -> DigestRecord {
        let path = crate::sync::sync_dir(root, project).join("digests.json");
        let entries = std::fs::read(&path).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default();
        DigestRecord { path, entries: Mutex::new(entries), dirty: AtomicBool::new(false) }
    }

    fn entries(&self) -> std::sync::MutexGuard<'_, BTreeMap<String, Recorded>> {
        self.entries.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// The sha256 and size of the file at `path`, kept under `rel`: the recorded digest when
    /// the file's stamp matches the record's, else the file hashed and recorded.
    pub(crate) fn digest(&self, rel: &str, path: &Path) -> std::io::Result<Option<Digest>> {
        let Some(before) = stamp(path)? else {
            self.forget(rel);
            return Ok(None);
        };
        if let Some(stamp) = before {
            if let Some(r) = self.entries().get(rel).filter(|r| r.stamp == stamp) {
                return Ok(Some(Digest { sha256: r.sha256.clone(), size: stamp.size, read: 0 }));
            }
        }
        let bytes = match std::fs::read(path) {
            Ok(b) => b,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(e),
        };
        let sha256 = sha256_hex(&bytes);
        // A file rewritten between the stamp and the read records under the next hash.
        if let Some(read) = before.filter(|s| s.size == bytes.len() as u64) {
            if stamp(path)? == Some(Some(read)) {
                self.keep(rel, Recorded { stamp: read, sha256: sha256.clone() });
            }
        }
        Ok(Some(Digest { sha256, size: bytes.len() as u64, read: bytes.len() as u64 }))
    }

    /// Record `sha256` for the file just written at `path`.
    pub(crate) fn note(&self, rel: &str, path: &Path, sha256: String) {
        if let Ok(Some(Some(stamp))) = stamp(path) {
            self.keep(rel, Recorded { stamp, sha256 });
        }
    }

    fn keep(&self, rel: &str, recorded: Recorded) {
        let mut entries = self.entries();
        if entries.get(rel) != Some(&recorded) {
            entries.insert(rel.to_string(), recorded);
            self.dirty.store(true, Ordering::SeqCst);
        }
    }

    /// Drop the record of a file no longer there.
    pub(crate) fn forget(&self, rel: &str) {
        if self.entries().remove(rel).is_some() {
            self.dirty.store(true, Ordering::SeqCst);
        }
    }

    /// Keep only the records `keep` accepts.
    pub(crate) fn retain(&self, keep: impl Fn(&str) -> bool) {
        let mut entries = self.entries();
        let before = entries.len();
        entries.retain(|k, _| keep(k));
        if entries.len() != before {
            self.dirty.store(true, Ordering::SeqCst);
        }
    }

    /// Write the record back when it changed. A failed write costs the next pull or push a
    /// rehash and refuses nothing.
    pub(crate) fn save(&self) {
        if !self.dirty.swap(false, Ordering::SeqCst) {
            return;
        }
        let Ok(bytes) = serde_json::to_vec(&*self.entries()) else { return };
        if self.path.parent().is_some_and(|dir| std::fs::create_dir_all(dir).is_ok()) {
            let _ = contextful_context::store::replace_file(&self.path, &bytes);
        }
    }
}
