//! The local catalog: a directory owned by one machine, every write serialized under one
//! advisory lock and published by an atomic rename, so each conditional update is
//! linearizable on that machine (`topology.coordinate.backends`).
//!
//! ```text
//! <root>/catalog.lock
//! <root>/runs/<run_id>.json                  one run row
//! <root>/scopes/<pipeline>/<table>.json      the cursor row and the pending owner, together
//! <root>/leases/<key>.json                   one lease row
//! ```

use crate::fsutil::{read_json, replace, to_json, FileLock};
use contextful_core::coordinate::{Cas, Catalog, CursorRow, Lease, LeaseKey, LeaseRow};
use contextful_core::ports::Clock;
use contextful_core::run::own::ExecutionOwner;
use contextful_core::run::record::RunRow;
use contextful_core::run::{Failure, RunError};
use contextful_core::store::StoreError;
use contextful_core::time::Instant;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// A table's cursor row and pending owner share one file, so retiring the owner and
/// caching the position it committed is one rename.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct ScopeRow {
    #[serde(default)]
    cursor: CursorRow,
    #[serde(default)]
    owner: Option<ExecutionOwner>,
}

/// The single-node catalog backend behind the `Catalog` port.
#[derive(Clone)]
pub struct LocalCatalog {
    root: PathBuf,
    clock: Arc<dyn Clock + Send + Sync>,
}

fn segment(s: &str) -> String {
    s.replace('%', "%25").replace('/', "%2F")
}

impl LocalCatalog {
    /// A catalog rooted at `root`, reading `clock` as its own.
    pub fn open(root: &Path, clock: Arc<dyn Clock + Send + Sync>) -> LocalCatalog {
        LocalCatalog { root: root.to_path_buf(), clock }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    fn lock(&self) -> Result<FileLock, Failure> {
        FileLock::acquire(&self.root.join("catalog.lock"))
    }

    fn run_path(&self, run_id: &str) -> PathBuf {
        self.root.join("runs").join(format!("{}.json", segment(run_id)))
    }

    fn scope_path(&self, pipeline_id: &str, table: &str) -> PathBuf {
        self.root.join("scopes").join(segment(pipeline_id)).join(format!("{}.json", segment(table)))
    }

    fn lease_path(&self, key: &LeaseKey) -> PathBuf {
        self.root.join("leases").join(format!("{}.json", key.spelling()))
    }

    /// The refusal a commit carrying `fence` meets when a later acquisition moved the
    /// lease past it; `None` when the fence is current or the commit carries none.
    fn fenced(&self, pipeline_id: &str, table: &str, fence: Option<&Lease>) -> Result<Option<Cas>, Failure> {
        let Some(lease) = fence else { return Ok(None) };
        let row: LeaseRow = read_json(&self.root.join("leases").join(format!("{}.json", lease.key)))?.unwrap_or_default();
        Ok((!row.admits(lease.fence)).then(|| {
            Cas::Fenced(StoreError::LeaseFenced(format!(
                "cursor commit for `{pipeline_id}`/`{table}` carries fence {} and lease `{}` is at fence {}",
                lease.fence, lease.key, row.fence
            )))
        }))
    }

    fn scope(&self, pipeline_id: &str, table: &str) -> Result<ScopeRow, Failure> {
        Ok(read_json(&self.scope_path(pipeline_id, table))?.unwrap_or_default())
    }
}

impl Catalog for LocalCatalog {
    fn now(&self) -> Result<Instant, Failure> {
        Ok(self.clock.now())
    }

    fn acquire(&self, key: &LeaseKey, holder: &str, ttl_secs: u64) -> Result<Option<Lease>, Failure> {
        let _lock = self.lock()?;
        let path = self.lease_path(key);
        let mut row: LeaseRow = read_json(&path)?.unwrap_or_default();
        let lease = row.acquire(&key.spelling(), holder, self.clock.now(), ttl_secs);
        if lease.is_some() {
            replace(&path, &to_json(&row)?)?;
        }
        Ok(lease)
    }

    fn release(&self, lease: &Lease) -> Result<(), Failure> {
        let _lock = self.lock()?;
        let path = self.root.join("leases").join(format!("{}.json", lease.key));
        let mut row: LeaseRow = read_json(&path)?.unwrap_or_default();
        row.release(lease);
        replace(&path, &to_json(&row)?)
    }

    fn lease_row(&self, key: &LeaseKey) -> Result<LeaseRow, Failure> {
        Ok(read_json(&self.lease_path(key))?.unwrap_or_default())
    }

    fn cursor(&self, pipeline_id: &str, table: &str) -> Result<CursorRow, Failure> {
        Ok(self.scope(pipeline_id, table)?.cursor)
    }

    fn cursor_cas(&self, pipeline_id: &str, table: &str, expected_version: u64, next: CursorRow, fence: Option<&Lease>) -> Result<Cas, Failure> {
        let _lock = self.lock()?;
        let mut scope = self.scope(pipeline_id, table)?;
        if scope.cursor.version != expected_version {
            return Ok(Cas::VersionMoved);
        }
        if let Some(fenced) = self.fenced(pipeline_id, table, fence)? {
            return Ok(fenced);
        }
        scope.cursor = CursorRow { version: expected_version + 1, ..next };
        replace(&self.scope_path(pipeline_id, table), &to_json(&scope)?)?;
        Ok(Cas::Applied)
    }

    fn owner(&self, pipeline_id: &str, table: &str) -> Result<Option<ExecutionOwner>, Failure> {
        Ok(self.scope(pipeline_id, table)?.owner)
    }

    fn put_owner(&self, owner: &ExecutionOwner) -> Result<(), Failure> {
        let _lock = self.lock()?;
        let mut scope = self.scope(&owner.pipeline_id, &owner.table)?;
        scope.owner = Some(owner.clone());
        replace(&self.scope_path(&owner.pipeline_id, &owner.table), &to_json(&scope)?)
    }

    fn retire(&self, pipeline_id: &str, table: &str, execution_id: &str, cursor: CursorRow, fence: Option<&Lease>) -> Result<Cas, Failure> {
        let _lock = self.lock()?;
        if let Some(fenced) = self.fenced(pipeline_id, table, fence)? {
            return Ok(fenced);
        }
        let mut scope = self.scope(pipeline_id, table)?;
        if scope.owner.as_ref().is_some_and(|o| o.execution_id == execution_id) {
            scope.owner = None;
        }
        scope.cursor = CursorRow { version: scope.cursor.version + 1, ..cursor };
        replace(&self.scope_path(pipeline_id, table), &to_json(&scope)?)?;
        Ok(Cas::Applied)
    }

    fn put_run(&self, row: &RunRow) -> Result<(), Failure> {
        let _lock = self.lock()?;
        replace(&self.run_path(&row.run_id), &to_json(row)?)
    }

    fn run(&self, run_id: &str) -> Result<Option<RunRow>, Failure> {
        read_json(&self.run_path(run_id))
    }

    fn runs(&self, pipeline_id: Option<&str>) -> Result<Vec<RunRow>, Failure> {
        let dir = self.root.join("runs");
        let entries = match std::fs::read_dir(&dir) {
            Ok(e) => e,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(crate::fsutil::storage(&dir, e)),
        };
        let mut rows = Vec::new();
        for entry in entries {
            let path = entry.map_err(|e| crate::fsutil::storage(&dir, e))?.path();
            if path.extension().is_some_and(|x| x == "json") && !path.file_name().is_some_and(|n| n.to_string_lossy().starts_with('.')) {
                if let Some(row) = read_json::<RunRow>(&path)? {
                    if pipeline_id.is_none_or(|p| row.pipeline_id == p) {
                        rows.push(row);
                    }
                }
            }
        }
        Ok(rows)
    }

    fn update_run(&self, run_id: &str, f: &mut dyn FnMut(&mut RunRow) -> Result<(), RunError>) -> Result<Option<Result<RunRow, RunError>>, Failure> {
        let _lock = self.lock()?;
        let path = self.run_path(run_id);
        let Some(mut row) = read_json::<RunRow>(&path)? else { return Ok(None) };
        if let Err(e) = f(&mut row) {
            return Ok(Some(Err(e)));
        }
        replace(&path, &to_json(&row)?)?;
        Ok(Some(Ok(row)))
    }
}
