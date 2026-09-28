//! The file-tree adapters: machine-local files beside the catalog.
//!
//! ```text
//! <root>/journal/<execution_id>/<entry digest>.json   a pending claim or a recorded value
//! <root>/blobs/<sha256>                               a value above the inline cutoff
//! <root>/blobs/.swept                                 the instant the last sweep ran
//! <root>/awakeables/<token>.json                      one awakeable registry row
//! ```
//!
//! A claim is an exclusive create; every other change to a row runs under an OS advisory
//! lock on a sibling `.lock` file and lands in one rename, so a reader sees the old row
//! or the new one.

use super::is_token;
use crate::fsutil::{self, create_new, read_json, replace, storage, to_json, FileLock};
use contextful_core::run::journal::{sweepable, EntryKey, Row, Stored};
use contextful_core::run::ports::{AwakeableStore, BlobStore, JournalStore};
use contextful_core::run::suspend::Awakeable;
use contextful_core::run::Failure;
use std::path::{Path, PathBuf};

/// Every entry name under `dir`; a missing directory holds none.
fn entries(dir: &Path) -> Result<Vec<(String, PathBuf)>, Failure> {
    let listing = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(storage(dir, e)),
    };
    let mut out = Vec::new();
    for entry in listing {
        let entry = entry.map_err(|e| storage(dir, e))?;
        out.push((entry.file_name().to_string_lossy().into_owned(), entry.path()));
    }
    Ok(out)
}

/// Journal rows as one JSON file per entry.
#[derive(Debug, Clone)]
pub struct FileJournalStore {
    root: PathBuf,
}

impl FileJournalStore {
    pub fn open(root: &Path) -> FileJournalStore {
        FileJournalStore { root: root.join("journal") }
    }

    fn execution_dir(&self, execution_id: &str) -> PathBuf {
        self.root.join(execution_id)
    }

    fn row_path(&self, key: &EntryKey) -> PathBuf {
        self.execution_dir(&key.execution_id).join(format!("{}.json", key.digest()))
    }

    fn lock(&self, key: &EntryKey) -> Result<FileLock, Failure> {
        FileLock::acquire(&self.row_path(key).with_extension("lock"))
    }
}

impl JournalStore for FileJournalStore {
    fn create_pending(&self, key: &EntryKey, run_id: &str) -> Result<bool, Failure> {
        create_new(&self.row_path(key), &to_json(&Row::Pending { key: key.clone(), run_id: run_id.to_string() })?)
    }

    fn read(&self, key: &EntryKey) -> Result<Option<Row>, Failure> {
        read_json(&self.row_path(key))
    }

    fn replace_if_pending(&self, key: &EntryKey, holder: &str, run_id: &str) -> Result<bool, Failure> {
        let path = self.row_path(key);
        let _lock = self.lock(key)?;
        match read_json::<Row>(&path)? {
            Some(Row::Pending { run_id: h, .. }) if h == holder => {
                replace(&path, &to_json(&Row::Pending { key: key.clone(), run_id: run_id.to_string() })?)?;
                Ok(true)
            }
            _ => Ok(false),
        }
    }

    fn record(&self, key: &EntryKey, value: &Stored) -> Result<Option<Stored>, Failure> {
        let path = self.row_path(key);
        let _lock = self.lock(key)?;
        if let Some(Row::Recorded { value: standing, .. }) = read_json::<Row>(&path)? {
            return Ok(Some(standing));
        }
        replace(&path, &to_json(&Row::Recorded { key: key.clone(), value: value.clone() })?)?;
        Ok(None)
    }

    fn release(&self, key: &EntryKey, run_id: &str) -> Result<(), Failure> {
        let path = self.row_path(key);
        let _lock = self.lock(key)?;
        if let Some(Row::Pending { run_id: h, .. }) = read_json::<Row>(&path)? {
            if h == run_id {
                fsutil::remove_file(&path)?;
            }
        }
        // The lock file stays: unlinking it under the lock lets a later caller lock a new
        // inode while this holder still holds the old one.
        Ok(())
    }

    fn rows(&self, execution_id: &str) -> Result<Vec<Row>, Failure> {
        let mut rows = Vec::new();
        for (name, path) in entries(&self.execution_dir(execution_id))? {
            if name.ends_with(".json") && !name.starts_with('.') {
                rows.extend(read_json(&path)?);
            }
        }
        Ok(rows)
    }

    fn retire(&self, execution_id: &str) -> Result<(), Failure> {
        let dir = self.execution_dir(execution_id);
        match std::fs::remove_dir_all(&dir) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(storage(&dir, e)),
        }
    }

    fn executions(&self) -> Result<Vec<String>, Failure> {
        Ok(entries(&self.root)?.into_iter().filter(|(name, path)| !name.starts_with('.') && path.is_dir()).map(|(name, _)| name).collect())
    }
}

/// Blobs as one file per sha256, each written through a private temporary file renamed
/// over the destination.
#[derive(Debug, Clone)]
pub struct FileBlobStore {
    dir: PathBuf,
}

impl FileBlobStore {
    pub fn open(root: &Path) -> FileBlobStore {
        FileBlobStore { dir: root.join("blobs") }
    }

    /// The directory holding every blob.
    pub fn dir(&self) -> PathBuf {
        self.dir.clone()
    }

    /// The file a blob lives in.
    pub fn path(&self, sha256: &str) -> PathBuf {
        self.dir.join(sha256)
    }

    fn stamp(&self) -> PathBuf {
        self.dir.join(".swept")
    }
}

impl BlobStore for FileBlobStore {
    fn put(&self, sha256: &str, bytes: &[u8]) -> Result<(), Failure> {
        replace(&self.path(sha256), bytes)
    }

    fn get(&self, sha256: &str) -> Result<Option<Vec<u8>>, Failure> {
        let path = self.path(sha256);
        match std::fs::read(&path) {
            Ok(bytes) => Ok(Some(bytes)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(storage(&path, e)),
        }
    }

    fn sweep(&self, referenced: &[String], now_unix: i64) -> Result<Vec<String>, Failure> {
        let mut deleted = Vec::new();
        for (name, path) in entries(&self.dir)? {
            if name.starts_with('.') {
                continue;
            }
            let modified = std::fs::metadata(&path)
                .and_then(|m| m.modified())
                .map_err(|e| storage(&path, e))?
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs() as i64)
                .unwrap_or(0);
            let age = u64::try_from(now_unix - modified).unwrap_or(0);
            if sweepable(referenced.contains(&name), age) {
                fsutil::remove_file(&path)?;
                deleted.push(name);
            }
        }
        Ok(deleted)
    }

    fn swept_at(&self) -> Result<Option<i64>, Failure> {
        read_json(&self.stamp())
    }

    fn mark_swept(&self, at: i64) -> Result<(), Failure> {
        replace(&self.stamp(), &to_json(&at)?)
    }
}

/// Awakeable registry rows as one JSON file per token.
#[derive(Debug, Clone)]
pub struct FileAwakeableStore {
    dir: PathBuf,
}

impl FileAwakeableStore {
    pub fn open(root: &Path) -> FileAwakeableStore {
        FileAwakeableStore { dir: root.join("awakeables") }
    }

    fn path(&self, token: &str) -> Option<PathBuf> {
        is_token(token).then(|| self.dir.join(format!("{token}.json")))
    }
}

impl AwakeableStore for FileAwakeableStore {
    fn insert(&self, row: &Awakeable) -> Result<(), Failure> {
        let path = self.path(&row.token).ok_or_else(|| storage(&self.dir, format!("`{}` is no token", row.token)))?;
        replace(&path, &to_json(row)?)
    }

    fn get(&self, token: &str) -> Result<Option<Awakeable>, Failure> {
        match self.path(token) {
            Some(path) => read_json(&path),
            None => Ok(None),
        }
    }

    fn update(&self, token: &str, edit: &mut dyn FnMut(&mut Awakeable) -> Result<bool, Failure>) -> Result<Option<Awakeable>, Failure> {
        let Some(path) = self.path(token) else { return Ok(None) };
        // An unknown token allocates nothing, the lock file included.
        if !path.exists() {
            return Ok(None);
        }
        let _lock = FileLock::acquire(&path.with_extension("lock"))?;
        let Some(current) = read_json::<Awakeable>(&path)? else { return Ok(None) };
        let mut row = current.clone();
        if edit(&mut row)? {
            replace(&path, &to_json(&row)?)?;
            return Ok(Some(row));
        }
        Ok(Some(current))
    }

    fn rows(&self) -> Result<Vec<Awakeable>, Failure> {
        let mut rows = Vec::new();
        for (name, path) in entries(&self.dir)? {
            if name.ends_with(".json") && !name.starts_with('.') {
                rows.extend(read_json(&path)?);
            }
        }
        Ok(rows)
    }
}
