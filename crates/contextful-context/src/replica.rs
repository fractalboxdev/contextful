//! A replica's advertisement: `replica.json` beside `derived.sqlite`, naming per table the
//! snapshots the replica holds and each one's parts and sidecars on disk
//! (`store.replicate.descriptor`). A read on a replica needing a part or sidecar the
//! descriptor omits refuses, naming the pull that supplies it, instead of answering from
//! what is present (`store.replicate.missing-index`).

use crate::error::Result;
use crate::Store;
use contextful_core::store::lay_out::{SnapshotManifest, MANIFEST_FILE};
use contextful_core::store::StoreError;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// The descriptor's file name under the store root, beside `derived.sqlite`. No push
/// carries it.
pub const DESCRIPTOR_FILE: &str = "replica.json";

/// What a replica holds, per table.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Descriptor {
    pub format: u32,
    /// The canonical store the replica answers for.
    pub of: String,
    /// Per table, the snapshots its pointer's chain reaches on disk, newest first.
    pub tables: BTreeMap<String, Vec<Held>>,
}

/// One snapshot on a replica, with the parts and sidecar paths present on disk, each
/// relative to the snapshot directory.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Held {
    pub snapshot_id: String,
    pub parts: Vec<String>,
    pub sidecars: Vec<String>,
}

/// Write the descriptor from what the store holds now: per table each snapshot its
/// pointer's chain reaches, with the parts and sidecars present on disk. A store that is
/// no replica writes nothing.
pub fn advertise(store: &Store) -> Result<Option<Descriptor>> {
    let Some(of) = store.replica_of() else { return Ok(None) };
    let mut descriptor = Descriptor { format: 1, of: of.to_string(), tables: BTreeMap::new() };
    for table in store.tables()? {
        let mut held = Vec::new();
        for snapshot in store.chain(&table)?.0 {
            let dir = store.snapshot_dir(&table, &snapshot.snapshot_id)?;
            let parts = snapshot.parts.iter().filter(|p| dir.join(&p.name).is_file()).map(|p| p.name.clone()).collect();
            let sidecars = snapshot
                .indexes
                .iter()
                .filter_map(|e| e.path())
                .filter(|p| dir.join(p).join(MANIFEST_FILE).is_file())
                .map(str::to_string)
                .collect();
            held.push(Held { snapshot_id: snapshot.snapshot_id.to_string(), parts, sidecars });
        }
        if !held.is_empty() {
            descriptor.tables.insert(table, held);
        }
    }
    let bytes = serde_json::to_vec_pretty(&descriptor).expect("a descriptor serializes");
    store.metadata_files().replace(&store.root().join(DESCRIPTOR_FILE), &bytes)?;
    Ok(Some(descriptor))
}

/// The descriptor a replica last wrote, if any.
pub fn read(store: &Store) -> Result<Option<Descriptor>> {
    let path = store.root().join(DESCRIPTOR_FILE);
    let Some(bytes) = store.metadata_files().read_optional(&path)? else { return Ok(None) };
    serde_json::from_slice(&bytes).map(Some).map_err(|e| crate::ContextError::Invalid(format!("{}: {e}", path.display())))
}

/// Refuse a read on a replica that needs a part of `snapshot`, or one of `sidecars`, the
/// descriptor does not advertise, naming the pull that supplies it. A store that is no
/// replica holds whatever its writer wrote.
pub fn require(store: &Store, table: &str, snapshot: &SnapshotManifest, sidecars: &[&str]) -> Result<()> {
    if store.replica_of().is_none() {
        return Ok(());
    }
    let id = snapshot.snapshot_id.to_string();
    let missing = |what: String| -> Result<()> {
        Err(StoreError::ReplicaMissingIndex(format!(
            "table `{table}`: this replica advertises no {what} of snapshot `{id}`; `contextful sync pull --table {table}` supplies it"
        ))
        .into())
    };
    let descriptor = read(store)?;
    let Some(held) = descriptor.as_ref().and_then(|d| d.tables.get(table)).and_then(|h| h.iter().find(|h| h.snapshot_id == id)) else {
        return missing("copy".into());
    };
    if let Some(part) = snapshot.parts.iter().find(|p| !held.parts.contains(&p.name)) {
        return missing(format!("part `{}`", part.name));
    }
    if let Some(sidecar) = sidecars.iter().find(|s| !held.sidecars.iter().any(|h| h == *s)) {
        return missing(format!("sidecar `{sidecar}`"));
    }
    Ok(())
}
