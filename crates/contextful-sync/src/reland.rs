//! `sync reland`: the runs a local snapshot off the bucket's chain alone holds, committed
//! again as new runs, and the local pointer moved to the bucket pointer's snapshot, so the
//! next pull proceeds (`store.pull.reland`).

// `SyncError` is the crate-wide refusal every `Syncer` operation returns.
#![allow(clippy::result_large_err)]

use crate::sync::{ancestry, missing_parts, write, Ancestry, Result, SyncError, Syncer};
use contextful_context::reland::{reland, Relanded};
use contextful_core::store::declare::TableDecl;
use contextful_core::store::lay_out::{NodeId, Pointer, SnapshotId, MANIFEST_FILE, POINTER_FILE, SNAPSHOTS_DIR};
use contextful_core::store::StoreError;
use contextful_context::ContextError;

/// What a reland did to one table.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct RelandReport {
    pub table: String,
    /// The snapshot the local pointer named.
    pub from: String,
    /// The bucket pointer's snapshot, which the local pointer now names.
    pub onto: String,
    /// The bucket pointer's fence, which the local pointer now carries.
    pub fence: u64,
    /// One entry per run the pull refusal names (`store.pull.local-fold-unrecoverable`), in its order.
    pub runs: Vec<Relanded>,
}

impl Syncer {
    /// Reland `decl`'s table (`store.pull.reland`).
    pub fn reland(&self, decl: &TableDecl) -> Result<RelandReport> {
        self.reland_with(decl, &|| Ok(()))
    }

    /// [`Syncer::reland`], running `before_pointer` once every run is committed and synced to
    /// disk and before the pointer write; a refusal there leaves the pointer where it stood
    /// (`store.pull.reland-order`).
    pub fn reland_with(&self, decl: &TableDecl, before_pointer: &dyn Fn() -> Result<()>) -> Result<RelandReport> {
        let table = decl.name.as_str();
        let unneeded = |why: String| -> SyncError { StoreError::SyncRelandUnneeded(format!("table `{table}`: {why}; nothing is relanded")).into() };
        let rel_key = format!("{}/tables/{table}/{POINTER_FILE}", self.project);
        let pointer_path = self.local_path(&rel_key).ok_or_else(|| SyncError::Context(ContextError::Invalid(format!("table `{table}` is outside the project"))))?;
        let table_dir = pointer_path.parent().map(std::path::Path::to_path_buf).unwrap_or_default();
        let Some((held, _)) = self.store.pointer(table)? else { return Err(unneeded("the store holds no local pointer".into())) };
        let (bucket, _) = self.read_pointer(table)?;
        let Some(head) = bucket.snapshot_id.clone() else { return Err(unneeded("the bucket names no snapshot".into())) };
        let local = held.snapshot_id.to_string();
        // mirrors: store.pull.pointer-adopt
        // A pointer on, behind or ahead on the bucket's chain is one the pull keeps or advances.
        let head_id: SnapshotId = serde_json::from_value(serde_json::Value::String(head.clone())).map_err(|e| SyncError::Context(ContextError::Invalid(format!("`{rel_key}`: {e}"))))?;
        let ahead = (held.fence.unwrap_or(0), &held.snapshot_id) >= (bucket.fence, &head_id);
        let behind = matches!(ancestry(&self.store, &table_dir, &head, &local), Ancestry::Descends);
        let descends = matches!(ancestry(&self.store, &table_dir, &local, &head), Ancestry::Descends);
        if local == head || behind || (ahead && descends) {
            return Err(unneeded(format!("the local pointer's `{local}` is on the bucket's chain at `{head}`, so a pull keeps or advances it")));
        }
        // The head's chain decides which runs it already folds (`store.pull.reland-head-absent`).
        let missing = match self.config.lazy_parts() {
            true => (!table_dir.join(SNAPSHOTS_DIR).join(&head).join(MANIFEST_FILE).exists()).then(|| MANIFEST_FILE.to_string()),
            false => missing_parts(&self.store, &table_dir, &head)?,
        };
        if let Some(missing) = missing {
            return Err(StoreError::SyncRelandHeadAbsent(format!(
                "the bucket's pointer names snapshot `{head}`, which lacks `{missing}` on disk; `contextful sync pull` brings it home before a reland of `{table}`"
            ))
            .into());
        }
        let (manifest, _) = self.manifest()?;
        let runs = match self.unrecoverable_runs(table, &table_dir, &local, &head, &manifest) {
            Err(absent) => {
                return Err(StoreError::SyncLocalFoldUnrecoverable(format!(
                    "`{rel_key}` names snapshot `{local}`, whose branch reaches snapshot `{absent}`, whose manifest is not on disk, so no reland names the runs it folds"
                ))
                .into())
            }
            Ok(runs) if runs.is_empty() => return Err(unneeded(format!("every run `{local}`'s branch folds is on disk, listed in the bucket or folded on `{head}`'s chain, so a pull replaces the pointer"))),
            Ok(runs) => runs,
        };
        let node = NodeId::parse(&self.node)?;
        let runs = runs.iter().map(|run| reland(&self.store, decl, run, &node)).collect::<contextful_context::Result<Vec<_>>>()?;
        before_pointer()?;
        let pointer = Pointer { snapshot_id: head_id, fence: Some(bucket.fence) };
        let bytes = serde_json::to_vec_pretty(&pointer).map_err(|e| SyncError::Context(ContextError::Invalid(e.to_string())))?;
        write(&pointer_path, &self.store.seal_metadata_bytes(&pointer_path, &bytes)?)?;
        Ok(RelandReport { table: table.to_string(), from: local, onto: head, fence: bucket.fence, runs })
    }
}
