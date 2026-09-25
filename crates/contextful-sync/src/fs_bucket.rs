//! A bucket on a filesystem directory: keys are paths under it, an object's ETag is the
//! sha256 of its bytes, and every conditional put runs under one advisory lock on the
//! bucket, so conditional writes are linearizable across the processes of one machine.

use contextful_core::run::journal::sha256_hex;
use contextful_core::store::object::{Condition, ObjectError, ObjectStore, Put};
use std::fs;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};

/// The filesystem bucket `<root>/<bucket>/`.
#[derive(Debug, Clone)]
pub struct FsBucket {
    root: PathBuf,
}

fn transport(path: &Path, e: std::io::Error) -> ObjectError {
    ObjectError::Transport(format!("{}: {e}", path.display()))
}

impl FsBucket {
    /// Open the bucket named `bucket` under the directory `root`.
    pub fn open(root: &Path, bucket: &str) -> Result<FsBucket, ObjectError> {
        if bucket.is_empty() || bucket.contains('/') || bucket.starts_with('.') {
            return Err(ObjectError::Unsupported(format!("`{bucket}` is not a bucket name")));
        }
        let root = root.join(bucket);
        fs::create_dir_all(&root).map_err(|e| transport(&root, e))?;
        Ok(FsBucket { root })
    }

    fn path(&self, key: &str) -> Result<PathBuf, ObjectError> {
        if key.is_empty() || key.starts_with('/') || key.split('/').any(|s| s.is_empty() || s == "." || s == ".." || s.starts_with(".bucket")) {
            return Err(ObjectError::Unsupported(format!("`{key}` is not an object key")));
        }
        Ok(self.root.join(key))
    }

    fn lock(&self) -> Result<fs::File, ObjectError> {
        let path = self.root.join(".bucket.lock");
        let file = fs::OpenOptions::new().read(true).write(true).create(true).truncate(false).open(&path).map_err(|e| transport(&path, e))?;
        file.lock().map_err(|e| transport(&path, e))?;
        Ok(file)
    }

    fn write(&self, path: &Path, bytes: &[u8]) -> Result<(), ObjectError> {
        if let Some(dir) = path.parent() {
            fs::create_dir_all(dir).map_err(|e| transport(dir, e))?;
        }
        let mut nonce = [0u8; 8];
        getrandom::fill(&mut nonce).map_err(|e| ObjectError::Transport(e.to_string()))?;
        let tmp = path.with_file_name(format!(".bucket-tmp-{}", nonce.iter().map(|b| format!("{b:02x}")).collect::<String>()));
        fs::write(&tmp, bytes).map_err(|e| transport(&tmp, e))?;
        fs::rename(&tmp, path).map_err(|e| transport(path, e))
    }
}

impl ObjectStore for FsBucket {
    fn get(&self, key: &str) -> Result<Option<(Vec<u8>, String)>, ObjectError> {
        let path = self.path(key)?;
        match fs::read(&path) {
            Ok(bytes) => {
                let etag = sha256_hex(&bytes);
                Ok(Some((bytes, etag)))
            }
            Err(e) if e.kind() == ErrorKind::NotFound => Ok(None),
            Err(e) => Err(transport(&path, e)),
        }
    }

    fn put(&self, key: &str, bytes: &[u8], condition: Condition) -> Result<Put, ObjectError> {
        let path = self.path(key)?;
        let _lock = self.lock()?;
        let current = match fs::read(&path) {
            Ok(b) => Some(sha256_hex(&b)),
            Err(e) if e.kind() == ErrorKind::NotFound => None,
            Err(e) => return Err(transport(&path, e)),
        };
        let holds = match &condition {
            Condition::None => true,
            Condition::IfNoneMatch => current.is_none(),
            Condition::IfMatch(etag) => current.as_deref() == Some(etag.as_str()),
        };
        if !holds {
            return Ok(Put::ConditionFailed);
        }
        self.write(&path, bytes)?;
        Ok(Put::Applied(sha256_hex(bytes)))
    }

    fn delete(&self, key: &str) -> Result<(), ObjectError> {
        let path = self.path(key)?;
        let _lock = self.lock()?;
        match fs::remove_file(&path) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == ErrorKind::NotFound => Ok(()),
            Err(e) => Err(transport(&path, e)),
        }
    }

    fn list(&self, prefix: &str) -> Result<Vec<String>, ObjectError> {
        let mut out = Vec::new();
        let mut stack = vec![self.root.clone()];
        while let Some(dir) = stack.pop() {
            let entries = match fs::read_dir(&dir) {
                Ok(e) => e,
                Err(e) if e.kind() == ErrorKind::NotFound => continue,
                Err(e) => return Err(transport(&dir, e)),
            };
            for entry in entries {
                let path = entry.map_err(|e| transport(&dir, e))?.path();
                if path.file_name().is_some_and(|n| n.to_string_lossy().starts_with(".bucket")) {
                    continue;
                }
                if path.is_dir() {
                    stack.push(path);
                } else if let Ok(rel) = path.strip_prefix(&self.root) {
                    let key = rel.to_string_lossy().replace('\\', "/");
                    if key.starts_with(prefix) {
                        out.push(key);
                    }
                }
            }
        }
        out.sort();
        Ok(out)
    }
}
