//! The current snapshot set held in memory (`read.cache.hot-local-parquet`): every data
//! file a read resolves against, mapped read-only and faulted in once, so a statement
//! reads local pages and never waits on object storage or a cold disk. A refresh
//! reconciles the mappings with the set a read resolves against now: it maps what a land
//! or fold added and unmaps every file that left the set, so a file an erasure or a
//! retention collection removes keeps no inode or page alive past the next refresh.

use super::{Face, ReadFault};
use crate::error::ContextError;
use std::path::{Path, PathBuf};

/// Bytes between the touches that fault a mapping in: one per page of the smallest page
/// size the supported targets use.
const PAGE: usize = 4096;

/// One data file of the snapshot set and its read-only mapping; an empty file maps nothing.
struct Mapped {
    path: PathBuf,
    map: Option<memmap2::Mmap>,
}

/// The current snapshot set, memory-mapped. The mappings live as long as this value.
pub struct MappedSnapshotSet {
    files: Vec<Mapped>,
}

impl MappedSnapshotSet {
    /// Each mapped file's path and bytes, sorted by path.
    pub fn files(&self) -> impl Iterator<Item = (&Path, &[u8])> {
        self.files.iter().map(|f| (f.path.as_path(), f.map.as_deref().unwrap_or_default()))
    }

    /// The mapped bytes in total.
    pub fn bytes(&self) -> u64 {
        self.files().map(|(_, b)| b.len() as u64).sum()
    }
}

/// What one refresh changed: files newly mapped and mappings released.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Refreshed {
    pub mapped: usize,
    pub released: usize,
}

impl Face {
    /// Map every data file a read of each table resolves against now — the snapshot the
    /// pointer names and the runs it has not folded — and touch each page once.
    pub fn map_snapshot_set(&self) -> Result<MappedSnapshotSet, ReadFault> {
        let files = self.resolved_paths()?.into_iter().map(map_file).collect::<Result<_, _>>()?;
        Ok(MappedSnapshotSet { files })
    }

    /// Reconcile `set` with the snapshot set a read resolves against now: keep each mapping
    /// still in it, map each file it gained, and unmap each file it lost or whose path has
    /// left the disk. A file that fails to map stays unmapped and the first such fault
    /// returns once every other file is reconciled.
    pub fn refresh_snapshot_set(&self, set: &mut MappedSnapshotSet) -> Result<Refreshed, ReadFault> {
        let current = self.resolved_paths()?;
        let mut held: std::collections::BTreeMap<PathBuf, Mapped> =
            std::mem::take(&mut set.files).into_iter().map(|m| (m.path.clone(), m)).collect();
        let mut files = Vec::with_capacity(current.len());
        let mut mapped = 0;
        let mut failed = None;
        for path in current {
            match held.remove(&path) {
                Some(kept) if path.exists() => files.push(kept),
                _ => match map_file(path) {
                    Ok(m) => {
                        mapped += 1;
                        files.push(m);
                    }
                    Err(fault) => {
                        failed.get_or_insert(fault);
                    }
                },
            }
        }
        // A mapping whose file left the set is dropped here, unmapping it.
        let released = held.len();
        drop(held);
        set.files = files;
        match failed {
            Some(fault) => Err(fault),
            None => Ok(Refreshed { mapped, released }),
        }
    }

    fn resolved_paths(&self) -> Result<Vec<PathBuf>, ReadFault> {
        let mut paths = Vec::new();
        for table in self.tables()? {
            let state = self.store.state(&self.decl(&table))?;
            let dir = self.store.table_dir(&table)?;
            let resolved = state.resolve(None).map_err(ContextError::from)?;
            paths.extend(resolved.files().iter().map(|f| dir.join(f)));
        }
        paths.sort();
        Ok(paths)
    }
}

fn map_file(path: PathBuf) -> Result<Mapped, ReadFault> {
    let io = |source| ReadFault::Store(ContextError::Io { path: path.clone(), source });
    let file = std::fs::File::open(&path).map_err(io)?;
    if file.metadata().map_err(io)?.len() == 0 {
        return Ok(Mapped { path, map: None });
    }
    // SAFETY: the file belongs to a committed snapshot or run, which no writer rewrites in
    // place; a fold or a run stages new files beside it.
    let map = unsafe { memmap2::Mmap::map(&file) }.map_err(io)?;
    let mut touched = 0u8;
    for at in (0..map.len()).step_by(PAGE) {
        touched ^= map[at];
    }
    std::hint::black_box(touched);
    Ok(Mapped { path, map: Some(map) })
}
