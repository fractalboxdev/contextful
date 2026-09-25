//! `topology.coordinate`: lease rows, fences and the cursor compare-and-swap behind the `Catalog` port.

use crate::support::{Rig, T0};
use contextful_core::coordinate::{Cas, CursorRow, LeaseKey, CADENCE_LEASE_TTL_SECS};
use contextful_core::store::StoreError;
use serde_json::json;

fn pipeline() -> LeaseKey {
    LeaseKey::Pipeline("feed/filings".into())
}

/// A lease row is keyed by a pipeline, a source partition, a table's compaction or a deployment's cadence, and
/// carries a holder, an expiry instant and a fence taken by one conditional update.
// spec: topology.coordinate.lease-row@4ac1f061
#[test]
fn a_lease_row_carries_holder_expiry_and_fence_per_key() {
    let rig = Rig::new();
    let c = rig.catalog();
    let keys = [
        pipeline(),
        LeaseKey::Partition { pipeline: "feed".into(), partition: "2030-01".into() },
        LeaseKey::Compaction("filings".into()),
        LeaseKey::Cadence("prod".into()),
    ];
    for k in &keys {
        let lease = c.acquire(k, "holder-a", 30).unwrap().unwrap();
        assert_eq!((lease.holder.as_str(), lease.fence), ("holder-a", 1));
        let row = c.lease_row(k).unwrap();
        assert_eq!(row.holder.as_deref(), Some("holder-a"));
        assert_eq!(row.expires_at, Some(crate::support::at(T0).plus_secs(30)));
        assert_eq!(row.fence, 1);
        // The conditional update matches nothing while the holder is live.
        assert!(c.acquire(k, "holder-b", 30).unwrap().is_none(), "{k:?}");
    }
    let spellings: std::collections::BTreeSet<_> = keys.iter().map(|k| k.spelling()).collect();
    assert_eq!(spellings.len(), 4, "each key is its own row");
}

/// The catalog evaluates a lease row's expiry against its own clock, never a caller-supplied instant.
// spec: topology.coordinate.catalog-clock@258b128b
#[test]
fn expiry_is_read_on_the_catalogs_clock() {
    let rig = Rig::new();
    let c = rig.catalog();
    c.acquire(&pipeline(), "a", 30).unwrap().unwrap();
    rig.clock.advance(29);
    assert!(c.acquire(&pipeline(), "b", 30).unwrap().is_none());
    rig.clock.advance(1);
    let b = c.acquire(&pipeline(), "b", 30).unwrap().unwrap();
    assert_eq!(b.expires_at, crate::support::at(T0).plus_secs(60), "expiry counts from the catalog's now");
}

/// Each acquisition of a lease row increments its fence, and release keeps the fence, so the fence never repeats
/// for a key.
// spec: topology.coordinate.fence-advances@4d433aac
#[test]
fn every_acquisition_takes_a_new_fence() {
    let rig = Rig::new();
    let c = rig.catalog();
    let mut fences = Vec::new();
    for holder in ["a", "b", "a"] {
        let lease = c.acquire(&pipeline(), holder, 30).unwrap().unwrap();
        fences.push(lease.fence);
        c.release(&lease).unwrap();
        let row = c.lease_row(&pipeline()).unwrap();
        assert_eq!((row.holder, row.fence), (None, lease.fence), "release clears the holder and keeps the fence");
    }
    // Expiry, not release, also hands on a larger fence.
    c.acquire(&pipeline(), "c", 30).unwrap().unwrap();
    rig.clock.advance(31);
    fences.push(c.acquire(&pipeline(), "d", 30).unwrap().unwrap().fence);
    assert_eq!(fences, [1, 2, 3, 5]);
}

/// A commit under a lease is a conditional write predicated on the holder's fence; a predicate matching nothing
/// is {{store.lease.stale-fence}}.
// spec: topology.coordinate.fenced-commit@117e5e1e
#[test]
fn a_commit_carrying_a_superseded_fence_is_refused() {
    let rig = Rig::new();
    let c = rig.catalog();
    let stale = c.acquire(&pipeline(), "a", 30).unwrap().unwrap();
    rig.clock.advance(31);
    let current = c.acquire(&pipeline(), "b", 30).unwrap().unwrap();
    let next = CursorRow { position: Some(json!("p9")), ..CursorRow::default() };
    match c.cursor_cas("feed", "filings", 0, next.clone(), Some(&stale)).unwrap() {
        Cas::Fenced(StoreError::LeaseFenced(m)) => assert!(m.contains("fence 1"), "{m}"),
        other => panic!("{other:?}"),
    }
    assert_eq!(c.cursor("feed", "filings").unwrap(), CursorRow::default(), "the stale commit applied nothing");
    assert!(matches!(c.retire("feed", "filings", "x-1", next.clone(), Some(&stale)).unwrap(), Cas::Fenced(_)));
    assert_eq!(c.cursor_cas("feed", "filings", 0, next, Some(&current)).unwrap(), Cas::Applied);
    assert_eq!(c.cursor("feed", "filings").unwrap().position, Some(json!("p9")));
}

/// A cursor compare-and-swap is one conditional update predicated on the stored version, read back by its
/// affected-row count.
// spec: topology.coordinate.cursor-cas@c2be9796
#[test]
fn a_cursor_update_is_predicated_on_the_stored_version() {
    let rig = Rig::new();
    let c = rig.catalog();
    let row = |p: &str| CursorRow { position: Some(json!(p)), ..CursorRow::default() };
    assert_eq!(c.cursor_cas("feed", "filings", 0, row("p1"), None).unwrap(), Cas::Applied);
    assert_eq!(c.cursor("feed", "filings").unwrap().version, 1);
    // A writer that read version 0 matches nothing now.
    assert_eq!(c.cursor_cas("feed", "filings", 0, row("p2"), None).unwrap(), Cas::VersionMoved);
    assert_eq!(c.cursor("feed", "filings").unwrap().position, Some(json!("p1")));
    assert_eq!(c.cursor_cas("feed", "filings", 1, row("p2"), None).unwrap(), Cas::Applied);
    assert_eq!(c.cursor("feed", "filings").unwrap().version, 2);
}

/// A cadence lease is granted for 90 s.
// spec: topology.coordinate.cadence-lease-ttl@cf640cd8
#[test]
fn a_cadence_lease_lives_90_s() {
    assert_eq!(CADENCE_LEASE_TTL_SECS, 90);
    let rig = Rig::new();
    let c = rig.catalog();
    let key = LeaseKey::Cadence("prod".into());
    let lease = c.acquire(&key, "reconciler", CADENCE_LEASE_TTL_SECS).unwrap().unwrap();
    assert_eq!(lease.expires_at, crate::support::at(T0).plus_secs(90));
    rig.clock.advance(89);
    assert!(c.acquire(&key, "fallback-cron", CADENCE_LEASE_TTL_SECS).unwrap().is_none());
    rig.clock.advance(1);
    assert!(c.acquire(&key, "fallback-cron", CADENCE_LEASE_TTL_SECS).unwrap().is_some());
}
