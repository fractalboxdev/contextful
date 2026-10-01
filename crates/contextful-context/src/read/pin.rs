//! `read.resolve-pin` over the store: the snapshot a pinned table resolves to, and the
//! `contextful.resolved` entries of the published models a response touches.

use super::fault::ReadFault;
use crate::error::ContextError;
use crate::store::Store;
use contextful_core::read::pin::{resolved_block, Resolved};
use contextful_core::read::ReadError;
use contextful_core::store::bound_time::Bound;
use contextful_core::store::lay_out::{SnapshotManifest, MANIFEST_FILE};
use contextful_policy::enforce::session::Session;
use serde_json::Value;
use std::collections::BTreeMap;

/// The snapshot `table` pinned to `build_id` resolves to under `as_of`: the build's
/// committed manifest, or `None` where `as_of` admits no instant at or after the build's
/// start, so the transaction-time bound is the earlier one and resolves as it does
/// unpinned (`read.resolve-pin.earlier-bound-wins`). An identifier no committed manifest
/// on disk publishes refuses, naming the oldest one that does
/// (`read.resolve-pin.unknown-build`).
pub(crate) fn pinned(store: &Store, table: &str, build_id: &str, as_of: Option<Bound>) -> Result<Option<SnapshotManifest>, ReadFault> {
    let published: Vec<SnapshotManifest> = crate::build::manifests(store, table)?.into_iter().filter(|m| m.publish.is_some()).collect();
    let Some(found) = published.iter().find(|m| m.publish.as_ref().is_some_and(|p| p.build_id == build_id)) else {
        let oldest = match published.first().and_then(|m| m.publish.as_ref()) {
            Some(p) => format!("the oldest pinnable build is `{}`", p.build_id),
            None => "no build is pinnable".to_string(),
        };
        return Err(ReadError::PinnedBuildUnavailable(format!("table `{table}`: build `{build_id}` is unknown or collected; {oldest}")).into());
    };
    if as_of.is_some_and(|b| !b.admits(found.created_at)) {
        return Ok(None);
    }
    Ok(Some(found.clone()))
}

/// The snapshot id a registered relation reads, where it reads one snapshot's parts and
/// no run part: the shape every model build publishes.
fn snapshot_of(files: &[String]) -> Option<String> {
    const SNAPSHOTS: &str = "/data/snapshots/";
    let mut ids = files.iter().map(|f| f.split_once(SNAPSHOTS).and_then(|(_, rest)| rest.split('/').next()));
    let first = ids.next()??;
    ids.all(|id| id == Some(first)).then(|| first.to_string())
}

/// The `contextful.resolved` block over every touched table whose registered relation
/// reads a published build, pinned or not; `None` where the read touches none
/// (`read.resolve-pin.resolved-echo`). The build is the one the session registered, so the
/// echo names exactly the state the rows came from.
pub(crate) fn resolved<'t>(store: &Store, session: &Session, touched: impl IntoIterator<Item = &'t str>) -> Result<Option<Value>, ReadFault> {
    let mut entries = BTreeMap::new();
    for table in touched {
        let Some(id) = session.relation(table).and_then(|r| snapshot_of(r.files())) else { continue };
        let path = store.table_dir(table)?.join("data").join("snapshots").join(&id).join(MANIFEST_FILE);
        let text = match std::fs::read_to_string(&path) {
            Ok(t) => t,
            // Collected after the session registered it: the read answered from no file of it.
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(e) => return Err(ContextError::Io { path, source: e }.into()),
        };
        let manifest: SnapshotManifest = serde_json::from_str(&text).map_err(|e| {
            contextful_core::store::StoreError::StoreManifestUnreadable(format!("table `{table}`: file `{}`: {e}", path.display()))
        })?;
        if let Some(section) = &manifest.publish {
            entries.insert(table.to_string(), Resolved::of(section));
        }
    }
    Ok(resolved_block(&entries))
}
