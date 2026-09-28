//! In-process adapters: every row and blob in a map behind one mutex per store. A clone
//! shares its original's state, so a second journal or registry over a clone reads what
//! the first wrote, as a restarted process reads the file tree. Nothing survives the
//! process.

use contextful_core::run::journal::{sweepable, EntryKey, Row, Stored};
use contextful_core::run::ports::{AwakeableStore, BlobStore, JournalStore};
use contextful_core::run::suspend::Awakeable;
use contextful_core::run::{Failure, FailureTag};
use std::collections::HashMap;
use std::sync::{Arc, Mutex, MutexGuard};

fn locked<T>(m: &Mutex<T>) -> Result<MutexGuard<'_, T>, Failure> {
    m.lock().map_err(|_| Failure::new(FailureTag::Storage, "a memory store's lock is poisoned"))
}

fn now_unix() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0)
}

/// Journal rows in a map keyed by entry key.
#[derive(Debug, Clone, Default)]
pub struct MemoryJournalStore {
    rows: Arc<Mutex<HashMap<EntryKey, Row>>>,
}

impl MemoryJournalStore {
    pub fn new() -> MemoryJournalStore {
        MemoryJournalStore::default()
    }
}

impl JournalStore for MemoryJournalStore {
    fn create_pending(&self, key: &EntryKey, run_id: &str) -> Result<bool, Failure> {
        let mut rows = locked(&self.rows)?;
        if rows.contains_key(key) {
            return Ok(false);
        }
        rows.insert(key.clone(), Row::Pending { key: key.clone(), run_id: run_id.to_string() });
        Ok(true)
    }

    fn read(&self, key: &EntryKey) -> Result<Option<Row>, Failure> {
        Ok(locked(&self.rows)?.get(key).cloned())
    }

    fn replace_if_pending(&self, key: &EntryKey, holder: &str, run_id: &str) -> Result<bool, Failure> {
        let mut rows = locked(&self.rows)?;
        match rows.get(key) {
            Some(Row::Pending { run_id: h, .. }) if h == holder => {
                rows.insert(key.clone(), Row::Pending { key: key.clone(), run_id: run_id.to_string() });
                Ok(true)
            }
            _ => Ok(false),
        }
    }

    fn record(&self, key: &EntryKey, value: &Stored) -> Result<Option<Stored>, Failure> {
        let mut rows = locked(&self.rows)?;
        if let Some(Row::Recorded { value: standing, .. }) = rows.get(key) {
            return Ok(Some(standing.clone()));
        }
        rows.insert(key.clone(), Row::Recorded { key: key.clone(), value: value.clone() });
        Ok(None)
    }

    fn release(&self, key: &EntryKey, run_id: &str) -> Result<(), Failure> {
        let mut rows = locked(&self.rows)?;
        if matches!(rows.get(key), Some(Row::Pending { run_id: h, .. }) if h == run_id) {
            rows.remove(key);
        }
        Ok(())
    }

    fn rows(&self, execution_id: &str) -> Result<Vec<Row>, Failure> {
        Ok(locked(&self.rows)?.iter().filter(|(k, _)| k.execution_id == execution_id).map(|(_, r)| r.clone()).collect())
    }

    fn retire(&self, execution_id: &str) -> Result<(), Failure> {
        locked(&self.rows)?.retain(|k, _| k.execution_id != execution_id);
        Ok(())
    }

    fn executions(&self) -> Result<Vec<String>, Failure> {
        let mut ids: Vec<String> = locked(&self.rows)?.keys().map(|k| k.execution_id.clone()).collect();
        ids.sort();
        ids.dedup();
        Ok(ids)
    }
}

#[derive(Debug, Default)]
struct Blobs {
    /// Each blob's bytes and the Unix second of its last put.
    held: HashMap<String, (Vec<u8>, i64)>,
    swept_at: Option<i64>,
}

/// Blobs in a map keyed by sha256, each stamped with the wall-clock second of its last put.
#[derive(Debug, Clone, Default)]
pub struct MemoryBlobStore {
    blobs: Arc<Mutex<Blobs>>,
}

impl MemoryBlobStore {
    pub fn new() -> MemoryBlobStore {
        MemoryBlobStore::default()
    }
}

impl BlobStore for MemoryBlobStore {
    fn put(&self, sha256: &str, bytes: &[u8]) -> Result<(), Failure> {
        locked(&self.blobs)?.held.insert(sha256.to_string(), (bytes.to_vec(), now_unix()));
        Ok(())
    }

    fn get(&self, sha256: &str) -> Result<Option<Vec<u8>>, Failure> {
        Ok(locked(&self.blobs)?.held.get(sha256).map(|(b, _)| b.clone()))
    }

    fn sweep(&self, referenced: &[String], now_unix: i64) -> Result<Vec<String>, Failure> {
        let mut blobs = locked(&self.blobs)?;
        let doomed: Vec<String> = blobs
            .held
            .iter()
            .filter(|(sha, (_, put_at))| sweepable(referenced.contains(sha), u64::try_from(now_unix - put_at).unwrap_or(0)))
            .map(|(sha, _)| sha.clone())
            .collect();
        for sha in &doomed {
            blobs.held.remove(sha);
        }
        Ok(doomed)
    }

    fn swept_at(&self) -> Result<Option<i64>, Failure> {
        Ok(locked(&self.blobs)?.swept_at)
    }

    fn mark_swept(&self, at: i64) -> Result<(), Failure> {
        locked(&self.blobs)?.swept_at = Some(at);
        Ok(())
    }
}

/// Awakeable rows in a map keyed by token.
#[derive(Debug, Clone, Default)]
pub struct MemoryAwakeableStore {
    rows: Arc<Mutex<HashMap<String, Awakeable>>>,
}

impl MemoryAwakeableStore {
    pub fn new() -> MemoryAwakeableStore {
        MemoryAwakeableStore::default()
    }
}

impl AwakeableStore for MemoryAwakeableStore {
    fn insert(&self, row: &Awakeable) -> Result<(), Failure> {
        locked(&self.rows)?.insert(row.token.clone(), row.clone());
        Ok(())
    }

    fn get(&self, token: &str) -> Result<Option<Awakeable>, Failure> {
        Ok(locked(&self.rows)?.get(token).cloned())
    }

    fn update(&self, token: &str, edit: &mut dyn FnMut(&mut Awakeable) -> Result<bool, Failure>) -> Result<Option<Awakeable>, Failure> {
        let mut rows = locked(&self.rows)?;
        let Some(current) = rows.get(token) else { return Ok(None) };
        let mut row = current.clone();
        if edit(&mut row)? {
            rows.insert(token.to_string(), row.clone());
            return Ok(Some(row));
        }
        Ok(Some(current.clone()))
    }

    fn rows(&self) -> Result<Vec<Awakeable>, Failure> {
        Ok(locked(&self.rows)?.values().cloned().collect())
    }
}
