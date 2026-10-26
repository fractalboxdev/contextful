//! A table's compaction lease renewed by the process holding it.

use crate::support::{at, bucket, node};
use contextful_core::store::lease::{BucketLease, TTL_SECS};
use contextful_core::store::object::ObjectStore;
use contextful_core::store::StoreError;
use contextful_sync::SyncError;
use serde_json::json;
use std::time::Duration;

const KEY: &str = "team/research/leases/compact/filings.json";

fn object(b: &dyn ObjectStore) -> (BucketLease, String) {
    let (bytes, etag) = b.get(KEY).unwrap().unwrap();
    (serde_json::from_slice(&bytes).unwrap(), etag)
}

/// The process holding a compaction lease renews it every 200 s by `If-Match` replace on its ETag; `sync compact`
/// renews while it pulls, folds and pushes.
// spec: store.lease.renewal@176cf746
#[test]
fn a_holder_renews_its_lease_while_its_work_runs_and_stops_after() {
    let dir = tempfile::tempdir().unwrap();
    let b = bucket(dir.path());
    let a = node("ingest-a", b.clone(), "");
    a.land("run-1", json!([{"id": 1}]), "2030-01-01T00:00:00Z");
    let start = at("2030-01-01T01:00:00Z");
    let held = a.syncer.acquire("filings", start).unwrap();
    let (_, first_etag) = object(b.as_ref());

    let seen = a.syncer.renewing("filings", &held, start, Duration::from_millis(40), || {
        std::thread::sleep(Duration::from_millis(300));
        object(b.as_ref())
    });
    let (renewed, etag) = seen;
    assert_ne!(etag, first_etag, "the work saw a renewed object");
    assert_eq!((renewed.holder.as_deref(), renewed.fence), (Some("ingest-a"), held.lease.fence));
    assert!(renewed.expires_at.unwrap() > start.plus_secs(TTL_SECS), "{renewed:?}");
    // The node's held record follows the renewal, so a later release or `--held` fold reads it.
    assert_eq!(a.syncer.held("filings").unwrap().unwrap().lease.fence, held.lease.fence);

    let (_, after) = object(b.as_ref());
    std::thread::sleep(Duration::from_millis(200));
    assert_eq!(object(b.as_ref()).1, after, "renewal stops once the work returns");
    a.syncer.release("filings").unwrap();
}

/// A holder renews a lease it still holds, and a lease another acquisition fenced past refuses the renewal.
#[test]
fn a_renewal_of_a_lease_a_successor_took_is_fenced() {
    let dir = tempfile::tempdir().unwrap();
    let b = bucket(dir.path());
    let a = node("ingest-a", b.clone(), "");
    let c = node("ingest-b", b.clone(), "");
    let start = at("2030-01-01T01:00:00Z");
    let held = a.syncer.acquire("filings", start).unwrap();
    let renewed = a.syncer.renew("filings", &held, start.plus_secs(200)).unwrap();
    assert_eq!(renewed.lease.expires_at, Some(start.plus_secs(200 + TTL_SECS)));
    assert_eq!(object(b.as_ref()).0, renewed.lease);

    // The renewed grant lapses past the skew bound, and the second node takes the next fence.
    c.syncer.acquire("filings", start.plus_secs(200 + TTL_SECS + 31)).unwrap();
    match a.syncer.renew("filings", &renewed, start.plus_secs(200 + TTL_SECS + 40)) {
        Err(SyncError::Store(StoreError::LeaseFenced(_))) => {}
        other => panic!("{other:?}"),
    }
}
