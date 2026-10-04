//! Authenticated SQLite snapshots shared by the machine catalog and run stores.

use crate::storage;
use contextful_core::run::Failure;
use contextful_core::store::encrypt::FileCipher;
use rusqlite::Connection;
use std::fs;
use std::io::{ErrorKind, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;

pub(crate) struct SealedFile {
    path: PathBuf,
    lock_path: PathBuf,
    cipher: Arc<dyn FileCipher>,
}

impl SealedFile {
    pub(crate) fn new(path: &Path, cipher: Arc<dyn FileCipher>) -> Self {
        let mut lock_name = path.as_os_str().to_os_string();
        lock_name.push(".lock");
        Self { path: path.to_path_buf(), lock_path: PathBuf::from(lock_name), cipher }
    }

    pub(crate) fn lock(&self) -> Result<fs::File, Failure> {
        if let Some(parent) = self.lock_path.parent() {
            fs::create_dir_all(parent).map_err(|e| storage(parent, e))?;
        }
        let lock = fs::OpenOptions::new().read(true).write(true).create(true).truncate(false).open(&self.lock_path)
            .map_err(|e| storage(&self.lock_path, e))?;
        lock.lock().map_err(|e| storage(&self.lock_path, e))?;
        Ok(lock)
    }

    pub(crate) fn load(&self, allow_create: bool, schema: &str) -> Result<Connection, Failure> {
        let mut conn = Connection::open_in_memory().map_err(|e| storage(&self.path, e))?;
        conn.execute_batch("PRAGMA temp_store = MEMORY").map_err(|e| storage(&self.path, e))?;
        match fs::read(&self.path) {
            Ok(bytes) => {
                let plain = self.cipher.open(&bytes).map_err(|e| storage(&self.path, e))?;
                conn.deserialize_read_exact(rusqlite::MAIN_DB, plain.as_slice(), plain.len(), false)
                    .map_err(|e| storage(&self.path, e))?;
            }
            Err(e) if e.kind() == ErrorKind::NotFound && allow_create => {
                conn.execute_batch(schema).map_err(|e| storage(&self.path, e))?;
            }
            Err(e) => return Err(storage(&self.path, e)),
        }
        Ok(conn)
    }

    pub(crate) fn save(&self, conn: &Connection) -> Result<(), Failure> {
        let plain = conn.serialize(rusqlite::MAIN_DB).map_err(|e| storage(&self.path, e))?;
        let sealed = self.cipher.seal(&plain).map_err(|e| storage(&self.path, e))?;
        let tmp = contextful_fs::tmp_sibling(&self.path);
        let mut staged = fs::OpenOptions::new().write(true).create_new(true).open(&tmp).map_err(|e| storage(&tmp, e))?;
        let outcome = (|| {
            staged.write_all(&sealed).map_err(|e| storage(&tmp, e))?;
            staged.sync_all().map_err(|e| storage(&tmp, e))?;
            fs::rename(&tmp, &self.path).map_err(|e| storage(&self.path, e))?;
            if let Some(parent) = self.path.parent() {
                fs::File::open(parent).and_then(|dir| dir.sync_all()).map_err(|e| storage(parent, e))?;
            }
            Ok(())
        })();
        if outcome.is_err() { let _ = fs::remove_file(&tmp); }
        outcome
    }
}
