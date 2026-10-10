//! `contextful context rebuild-catalog`: the rows of `derived.sqlite`, read off the
//! canonical tree and written through the `DerivedCatalog` port
//! (`store.lay-out.derived-catalog`). This package names no catalog backend.

use crate::{ContextError, Result, Store};
use contextful_core::store::catalog::{DerivedCatalog, DerivedRows, DerivedRun, DerivedSidecar, DerivedSnapshot, DerivedTable};

/// Every row the canonical tree determines: each table a `schema.json` declares with the
/// snapshot its pointer names, each snapshot the pointer's `parent` chain reaches with the
/// builder of each sidecar it holds (`store.index.identity`), and each committed run
/// manifest.
pub fn derive(store: &Store) -> Result<DerivedRows> {
    let mut rows = DerivedRows::default();
    for table in store.tables()? {
        let schema = store.schema(&table)?;
        let pointer = store.pointer(&table)?.map(|(p, _)| p);
        rows.tables.push(DerivedTable {
            table: table.clone(),
            schema: serde_json::to_string(&schema).map_err(|e| ContextError::Invalid(format!("table `{table}`: schema: {e}")))?,
            snapshot_id: pointer.as_ref().map(|p| p.snapshot_id.to_string()),
            fence: pointer.and_then(|p| p.fence),
        });
        for s in store.chain(&table)?.0 {
            for e in &s.indexes {
                let (Some(path), Some(kind), Some((builder, builder_version))) = (e.path(), e.kind_name(), e.builder()) else { continue };
                rows.sidecars.push(DerivedSidecar {
                    table: table.clone(),
                    snapshot_id: s.snapshot_id.to_string(),
                    path: path.to_string(),
                    kind: kind.to_string(),
                    builder: builder.to_string(),
                    builder_version,
                });
            }
            rows.snapshots.push(DerivedSnapshot {
                table: table.clone(),
                snapshot_id: s.snapshot_id.to_string(),
                parent: s.parent.map(|p| p.to_string()),
                created_at: s.created_at,
                row_count: s.row_count,
                parts: s.parts.len() as u64,
            });
        }
        for r in store.committed_runs(&table)? {
            rows.runs.push(DerivedRun {
                table: table.clone(),
                run_id: r.run_id,
                node_id: r.node_id,
                committed_at: r.committed_at,
                parts: r.parts.len() as u64,
                pipeline_id: r.pipeline_id,
            });
        }
    }
    Ok(rows.sorted())
}

/// Reconstruct the derived catalog from the tree, replacing every row it held.
pub fn rebuild(store: &Store, catalog: &dyn DerivedCatalog) -> Result<DerivedRows> {
    let rows = derive(store)?;
    catalog.replace(&rows).map_err(ContextError::Catalog)?;
    Ok(rows)
}
