//! The serving edge in front of a bucket: a read-only object store that hands one pulling
//! credential a replica holding only the tables its grants cover
//! (`authority.bound-redistribution.edge-filter`).

use crate::sync::{project_table, Redistribution};
use contextful_core::grant::{Action, Grant, TablePattern};
use contextful_core::store::object::{CasScope, Condition, ObjectError, ObjectStore, Put};
use contextful_core::store::sync::{admit_format, generation_of, BucketManifest, MANIFEST_KEY};
use std::sync::Arc;

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
    /// covering the table, a root object Shapes withholds under a grant on `*`, any other root
    /// object of the project always, and nothing outside the project.
    fn reaches(&self, rel: &str) -> bool {
        if !rel.starts_with(&format!("{}/", self.project)) {
            return false;
        }
        match project_table(&self.project, rel) {
            Some(table) => self.tables.iter().any(|p| p.covers_name(&table)),
            None if Redistribution::root_withheld(&self.project, rel) => self.tables.contains(&TablePattern::All),
            None => true,
        }
    }

    /// `bytes`, a bucket or generation manifest, with every key the grants miss removed.
    fn filter(&self, rel: &str, bytes: &[u8]) -> Result<Vec<u8>, ObjectError> {
        let invalid = |e: String| ObjectError::Transport(format!("`{rel}` at the serving edge: {e}"));
        let mut manifest: BucketManifest = serde_json::from_slice(bytes).map_err(|e| invalid(e.to_string()))?;
        admit_format(rel, manifest.format).map_err(|e| invalid(e.to_string()))?;
        manifest.entries.retain(|k, _| self.reaches(k));
        manifest.tombstones.retain(|k, _| self.reaches(k));
        manifest.pointers.retain(|k, _| self.reaches(k));
        manifest.control_heads.retain(|p, _| p == &self.project);
        serde_json::to_vec_pretty(&manifest).map_err(|e| invalid(e.to_string()))
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
