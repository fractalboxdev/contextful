//! The serving edge in front of a bucket: a read-only object store that hands one pulling
//! credential a replica holding only the tables its grants cover
//! (`authority.bound-redistribution.edge-filter`).

use crate::sync::project_table;
use contextful_core::grant::{Action, Grant, TablePattern};
use contextful_core::store::object::{CasScope, Condition, ObjectError, ObjectStore, Put};
use contextful_core::store::sync::{generation_of, BucketManifest, MANIFEST_FORMAT, MANIFEST_KEY};
use std::sync::Arc;

/// Root-level directories the edge serves under any grant: neither names a table's contents.
const OPEN_ROOT_DIRS: [&str; 2] = ["access/", "control/"];
/// Root-level files the edge serves under any grant.
const OPEN_ROOT_FILES: [&str; 1] = ["stale_memory.idx"];

/// A bucket seen through one credential's grants. A manifest it serves names only the keys the
/// grants reach; any other object reads as absent by its key and from a listing, so the edge
/// reveals no table the credential holds no grant on. It takes no write.
pub struct ServingEdge {
    inner: Arc<dyn ObjectStore>,
    prefix: String,
    project: String,
    /// The table patterns of every grant that opens a whole table
    /// (`authority.bound-redistribution.edge-whole-table`).
    tables: Vec<TablePattern>,
}

/// Whether a grant opens whole tables at the edge: a read grant carrying no tenant scope,
/// aggregate bound or horizon, each of which narrows rows a table's files hold.
fn whole_table(grant: &Grant) -> bool {
    grant.actions.contains(&Action::Read) && grant.tenant.is_none() && grant.aggregate.is_none() && grant.horizon.is_none()
}

impl ServingEdge {
    /// The edge over `inner` for the store `project` under `prefix`, serving the pulling
    /// credential's `grants`.
    pub fn new(inner: Arc<dyn ObjectStore>, prefix: &str, project: &str, grants: &[Grant]) -> ServingEdge {
        let tables = grants.iter().filter(|g| whole_table(g)).flat_map(|g| g.tables.iter().cloned()).collect();
        ServingEdge { inner, prefix: prefix.trim_end_matches('/').to_string(), project: project.to_string(), tables }
    }

    /// `key` relative to the prefix, when it lies below it.
    fn relative<'k>(&self, key: &'k str) -> Option<&'k str> {
        if self.prefix.is_empty() {
            Some(key)
        } else {
            key.strip_prefix(self.prefix.as_str())?.strip_prefix('/')
        }
    }

    /// Whether the grants reach the project-relative key `rel`: a table's object under a grant
    /// covering the table, an open root object always, any other root object (records, run
    /// states, cursor logs, catalogs) under a grant on `*`, and nothing outside the project.
    fn reaches(&self, rel: &str) -> bool {
        let Some(rest) = rel.strip_prefix(self.project.as_str()).and_then(|r| r.strip_prefix('/')) else { return false };
        match project_table(&self.project, rel) {
            Some(table) => self.tables.iter().any(|p| p.covers_name(&table)),
            None if OPEN_ROOT_DIRS.iter().any(|d| rest.starts_with(d)) || OPEN_ROOT_FILES.contains(&rest) => true,
            None => self.tables.contains(&TablePattern::All),
        }
    }

    /// `bytes`, a bucket or generation manifest, with every key the grants miss removed. A
    /// manifest of a format newer than this build parses reaches the puller as its format alone,
    /// so the puller refuses it (`store.push.format-unsupported`) and learns no key.
    fn filter(&self, rel: &str, bytes: &[u8]) -> Result<Vec<u8>, ObjectError> {
        let invalid = |e: serde_json::Error| ObjectError::Transport(format!("`{rel}` at the serving edge: {e}"));
        let value: serde_json::Value = serde_json::from_slice(bytes).map_err(invalid)?;
        let format = value.get("format").and_then(serde_json::Value::as_u64).unwrap_or(1);
        if format > u64::from(MANIFEST_FORMAT) {
            return serde_json::to_vec(&serde_json::json!({ "format": format })).map_err(invalid);
        }
        let mut manifest: BucketManifest = serde_json::from_value(value).map_err(invalid)?;
        manifest.entries.retain(|k, _| self.reaches(k));
        manifest.tombstones.retain(|k, _| self.reaches(k));
        manifest.pointers.retain(|k, _| self.reaches(k));
        manifest.control_heads.retain(|p, _| p == &self.project);
        serde_json::to_vec_pretty(&manifest).map_err(invalid)
    }
}

impl ObjectStore for ServingEdge {
    fn get(&self, key: &str) -> Result<Option<(Vec<u8>, String)>, ObjectError> {
        let Some(rel) = self.relative(key) else { return Ok(None) };
        if rel == MANIFEST_KEY || generation_of(rel).is_some() {
            return match self.inner.get(key)? {
                Some((bytes, etag)) => Ok(Some((self.filter(rel, &bytes)?, etag))),
                None => Ok(None),
            };
        }
        if !self.reaches(rel) {
            return Ok(None);
        }
        self.inner.get(key)
    }

    fn put(&self, key: &str, _bytes: &[u8], _condition: Condition) -> Result<Put, ObjectError> {
        Err(ObjectError::Forbidden(format!("`{key}`: the serving edge takes no write")))
    }

    fn delete(&self, key: &str) -> Result<(), ObjectError> {
        Err(ObjectError::Forbidden(format!("`{key}`: the serving edge takes no delete")))
    }

    fn list(&self, prefix: &str) -> Result<Vec<String>, ObjectError> {
        Ok(self
            .inner
            .list(prefix)?
            .into_iter()
            .filter(|k| self.relative(k).is_some_and(|rel| rel == MANIFEST_KEY || generation_of(rel).is_some() || self.reaches(rel)))
            .collect())
    }

    fn cas_scope(&self) -> CasScope {
        self.inner.cas_scope()
    }
}
