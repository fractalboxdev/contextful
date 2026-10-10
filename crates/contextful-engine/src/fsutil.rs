//! Filesystem primitives the engine's machine-local state rests on: the snapshot
//! package's atomic replace, exclusive create and advisory lock, and the JSON and sleep
//! helpers the engine alone uses.

pub use contextful_snapshot::fs::{create_new, filesystem_kind, replace, storage, FileLock};
use contextful_core::run::{Failure, FailureTag};
use serde::de::DeserializeOwned;
use serde::Serialize;
use std::fs;
use std::io::ErrorKind;
use std::path::Path;
use std::time::Duration;

/// Serialize `value` as pretty JSON.
pub fn to_json<T: Serialize>(value: &T) -> Result<Vec<u8>, Failure> {
    serde_json::to_vec_pretty(value).map_err(|e| Failure::new(FailureTag::Storage, format!("serializing: {e}")))
}

/// Read and decode `path`, or `None` when it does not exist.
pub fn read_json<T: DeserializeOwned>(path: &Path) -> Result<Option<T>, Failure> {
    match fs::read(path) {
        Ok(bytes) => serde_json::from_slice(&bytes).map(Some).map_err(|e| storage(path, e)),
        Err(e) if e.kind() == ErrorKind::NotFound => Ok(None),
        Err(e) => Err(storage(path, e)),
    }
}

/// Sleep `total` in slices, returning `false` as soon as `stop()` reads true.
pub fn sleep_unless(total: Duration, stop: &dyn Fn() -> bool) -> bool {
    let slice = Duration::from_millis(10);
    let start = std::time::Instant::now();
    loop {
        if stop() {
            return false;
        }
        let elapsed = start.elapsed();
        if elapsed >= total {
            return true;
        }
        std::thread::sleep(slice.min(total - elapsed));
    }
}

/// Remove `path`; a file already gone is removed.
pub fn remove_file(path: &Path) -> Result<(), Failure> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == ErrorKind::NotFound => Ok(()),
        Err(e) => Err(storage(path, e)),
    }
}
