//! The SQLite adapter behind the store's catalog ports (`store.lay-out.catalog-ports`) and
//! the run path's storage ports (`run.journal.sqlite-stores`): [`MachineCatalog`] serves
//! the `Catalog` port from `machine.sqlite`, [`DerivedSqlite`] serves the `DerivedCatalog`
//! port from `derived.sqlite`, and [`SqliteRunStores`] serves the journal, blob and
//! awakeable stores from one file.
//!
//! The package links whichever SQLite build the host resolves for `libsqlite3-sys`; its
//! `bundled` feature, off by default, compiles one in (`topology.package.sqlite-adapter`).

mod derived;
mod export;
mod machine;
mod sealed;
mod stores;

pub use derived::DerivedSqlite;
pub use export::{ExportLedger, ExportPosition, ExportPublication};
pub use machine::MachineCatalog;
pub use stores::{SqliteAwakeableStore, SqliteBlobStore, SqliteJournalStore, SqliteRunStores};

use contextful_core::run::{Failure, FailureTag};
use rusqlite::Connection;
use std::path::Path;
use std::time::Duration;

/// How long a statement waits on another connection's write lock before it fails: 30 s.
const BUSY_WAIT: Duration = Duration::from_secs(30);

fn storage(path: &Path, e: impl std::fmt::Display) -> Failure {
    Failure::new(FailureTag::Storage, format!("{}: {e}", path.display()))
}

/// Connect to `path`, creating it and its parent directory.
fn connect(path: &Path) -> Result<Connection, Failure> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| storage(parent, e))?;
    }
    let conn = Connection::open(path).map_err(|e| storage(path, e))?;
    conn.busy_timeout(BUSY_WAIT).map_err(|e| storage(path, e))?;
    Ok(conn)
}

/// Open `path`, creating it and its parent directory, and apply `schema`.
fn open(path: &Path, schema: &str) -> Result<Connection, Failure> {
    let conn = connect(path)?;
    conn.execute_batch(schema).map_err(|e| storage(path, e))?;
    Ok(conn)
}
