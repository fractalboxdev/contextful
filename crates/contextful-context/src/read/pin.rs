//! `read.resolve-pin` over the store: the snapshot a pinned table resolves to, and the
//! `contextful.resolved` entries of the published models a response touches.

use super::fault::ReadFault;
use crate::store::Store;
use contextful_core::read::pin::resolved_block;
use contextful_core::read::ReadError;
use contextful_core::store::bound_time::Bound;
use contextful_core::store::lay_out::SnapshotManifest;
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
    let published: Vec<SnapshotManifest> = retained(store, table)?.into_iter().filter(|m| m.publish.is_some()).collect();
    let Some(found) = published.iter().find(|m| m.publish.as_ref().is_some_and(|p| p.build_id == build_id)) else {
        let oldest = published.first().and_then(|m| m.publish.as_ref()).map(|p| p.build_id.as_str());
        return Err(unavailable(table, build_id, oldest));
    };
    if as_of.is_some_and(|b| !b.admits(found.created_at)) {
        return Ok(None);
    }
    Ok(Some(found.clone()))
}

/// The store snapshot a pin on a snapshot corpus names (`store.snapshot.capture`); an id
/// naming no snapshot of `table` refuses as an unknown build.
pub(crate) fn capture(store: &Store, table: &str, snapshot_id: &str) -> Result<SnapshotManifest, ReadFault> {
    retained(store, table)?.into_iter().find(|m| m.snapshot_id.to_string() == snapshot_id).ok_or_else(|| unavailable(table, snapshot_id, None))
}

/// Every snapshot manifest of `table` [`crate::build::manifests`] reads, save each a collection
/// reduced to its manifest: a pin to one is a pin to a collected build
/// (`read.resolve-pin.unknown-build`).
fn retained(store: &Store, table: &str) -> Result<Vec<SnapshotManifest>, ReadFault> {
    let mut out = Vec::new();
    for m in crate::build::manifests(store, table)? {
        if !store.snapshot_collected(table, &m)? {
            out.push(m);
        }
    }
    Ok(out)
}

/// The refusal of a pin on `table` to `build_id`, naming `oldest`, the oldest build still
/// pinnable. A table publishing no build, an absent table and one outside the grants all
/// refuse with `oldest` unnamed, in one text.
pub(crate) fn unavailable(table: &str, build_id: &str, oldest: Option<&str>) -> ReadFault {
    let oldest = match oldest {
        Some(id) => format!("the oldest pinnable build is `{id}`"),
        None => "no build is pinnable".to_string(),
    };
    ReadError::PinnedBuildUnavailable(format!("table `{table}`: build `{build_id}` is unknown or collected; {oldest}")).into()
}

/// The `contextful.resolved` block over every touched table whose registered relation
/// reads a published build, pinned or not; `None` where the read touches none
/// (`read.resolve-pin.resolved-echo`). Each entry is the one the session built when it
/// registered the relation, so the echo names exactly the state the rows came from,
/// though a later build collects it.
pub(crate) fn resolved<'t>(session: &Session, touched: impl IntoIterator<Item = &'t str>) -> Option<Value> {
    let entries: BTreeMap<String, _> =
        touched.into_iter().filter_map(|table| session.resolved(table).map(|r| (table.to_string(), r.clone()))).collect();
    resolved_block(&entries)
}

/// Under `horizon`, the build `table` resolves to: `None` for a table publishing no build,
/// else the newest published build whose recorded horizon is at or before it, or
/// `Some(None)` when none is (`read.horizon.build-resolution`). A build recording no
/// horizon read current inputs and serves no horizon.
pub(crate) fn within_horizon(store: &Store, table: &str, horizon: contextful_core::time::Instant) -> Result<Option<Option<String>>, ReadFault> {
    let published: Vec<SnapshotManifest> = retained(store, table)?.into_iter().filter(|m| m.publish.is_some()).collect();
    if published.is_empty() {
        return Ok(None);
    }
    Ok(Some(
        published
            .iter()
            .filter_map(|m| m.publish.as_ref())
            .filter(|p| p.watermark.horizon.is_some_and(|h| h <= horizon))
            .max_by_key(|p| (p.watermark.horizon, p.build_started_at))
            .map(|p| p.build_id.clone()),
    ))
}

/// The refusal of a read touching `table` under a horizon no build of it falls within.
pub(crate) fn no_build_within(table: &str, horizon: contextful_core::time::Instant) -> ReadFault {
    ReadError::ReadNoBuildWithinHorizon(format!("table `{table}` holds no build whose horizon is at or before {}", horizon.to_rfc3339_nanos())).into()
}
