//! `store.push`, `store.merge` and `store.lease`: prefixes, the scoped-union merge and the lease object.

use super::at;
use contextful_core::store::lease::{BucketLease, TTL_SECS};
use contextful_core::store::object::Condition;
use contextful_core::store::sync::{confine, merge, owner_of, BucketManifest, Entry, SyncConfig, Tombstone, TOMBSTONE_TTL_SECS};
use contextful_core::store::StoreError;
use std::collections::BTreeMap;

fn entry(sha: &str, owner: &str) -> Entry {
    Entry { sha256: sha.into(), size: 1, owner: owner.into() }
}

const RUN_A: &str = "research/tables/filings/data/runs/run-1/ingest-a/part-00000.parquet";
const RUN_B: &str = "research/tables/filings/data/runs/run-1/ingest-b/part-00000.parquet";
const SCHEMA: &str = "research/tables/filings/schema.json";

/// A key resolving outside the prefix raises `SyncPrefixEscape`, naming the key and the prefix.
// spec: store.push.prefix-escape@df83abd3
#[test]
fn a_key_climbing_out_of_the_prefix_is_refused() {
    assert_eq!(confine("team", "research/tables/filings/schema.json").unwrap(), "team/research/tables/filings/schema.json");
    for rel in ["../other/manifest.json", "research/../../x", "/etc/passwd", "research/./x", ""] {
        match confine("team", rel) {
            Err(StoreError::SyncPrefixEscape(m)) => assert!(m.contains("team"), "{m}"),
            other => panic!("{rel}: {other:?}"),
        }
    }
}

/// An unset variable named by `prefix_from` raises `SyncPrefixUnbound` at startup, with no bucket-root fallback.
// spec: store.push.prefix-unbound@c518a25a
#[test]
fn an_unset_prefix_variable_refuses_with_no_root_fallback() {
    let c = SyncConfig { prefix_from: Some("env:CONTEXTFUL_SYNC_PREFIX".into()), ..SyncConfig::default() };
    assert!(matches!(c.resolve_prefix(|_| None), Err(StoreError::SyncPrefixUnbound(m)) if m.contains("CONTEXTFUL_SYNC_PREFIX")));
    assert!(matches!(c.resolve_prefix(|_| Some(String::new())), Err(StoreError::SyncPrefixUnbound(_))));
    assert_eq!(c.resolve_prefix(|_| Some("team/".into())).unwrap(), "team");
}

/// Declaring `prefix` and `prefix_from` together raises `SyncPrefixOverspecified`.
// spec: store.push.prefix-overspecified@aa6f31a8
#[test]
fn prefix_and_prefix_from_together_refuse() {
    let c = SyncConfig { prefix: Some("team".into()), prefix_from: Some("env:P".into()), ..SyncConfig::default() };
    assert!(matches!(c.resolve_prefix(|_| Some("x".into())), Err(StoreError::SyncPrefixOverspecified(_))));
}

/// A key's owner is read off the key: a run directory's node segment, a request-ledger file's node, or a commit
/// log's node directory. Every other key is unowned and propagates no deletion.
// spec: store.merge.ownership@4c2db0b5
#[test]
fn a_keys_owner_is_read_off_the_key() {
    assert_eq!(owner_of(RUN_A).as_deref(), Some("ingest-a"));
    assert_eq!(owner_of("research/tables/filings/requests/run-1.ingest-b.parquet").as_deref(), Some("ingest-b"));
    assert_eq!(owner_of("research/tables/filings/requests/run.2030.01.ingest-b.parquet").as_deref(), Some("ingest-b"), "a dotted run id");
    assert_eq!(owner_of("research/cursors/feed/ingest-c/00000000000000000001.json").as_deref(), Some("ingest-c"));
    // A table named `runs`, or nesting `data/runs` in its name, reads its own run directory.
    assert_eq!(owner_of("research/tables/runs/data/runs/run-1/ingest-d/part-00000.parquet").as_deref(), Some("ingest-d"));
    assert_eq!(owner_of("runs/x"), None, "a key opening with `runs` has no underflow");
    assert_eq!(owner_of("research/tables/a/data/runs/run-1/_manifest.json"), None, "a short run path owns nothing");
    for unowned in [SCHEMA, "research/tables/filings/data/snapshots/snapshot-01/part-00000.parquet", "research/cursors/feed/00000000000000000001.json"] {
        assert_eq!(owner_of(unowned), None, "{unowned}");
    }
    // An unowned key the writer no longer holds stays listed: it propagates no deletion.
    let remote = BucketManifest { entries: [(SCHEMA.to_string(), entry("s", ""))].into(), ..Default::default() };
    let m = merge(&remote, &BTreeMap::new(), "ingest-a", at("2030-01-01T00:00:00Z")).unwrap();
    assert!(m.manifest.entries.contains_key(SCHEMA) && m.manifest.tombstones.is_empty());
}

/// A merge takes each local entry this writer owns or no node owns, and from the remote every other entry; a
/// remote entry it owns and no longer holds leaves with a tombstone.
// spec: store.merge.scoped-union@d0d3b811
#[test]
fn a_merge_keeps_every_local_entry_and_only_the_remote_entries_it_does_not_own() {
    let now = at("2030-01-01T00:00:00Z");
    let gone = "research/tables/filings/data/runs/run-0/ingest-a/part-00000.parquet";
    let remote = BucketManifest {
        entries: [(RUN_B.to_string(), entry("b", "ingest-b")), (gone.to_string(), entry("old", "ingest-a")), (SCHEMA.to_string(), entry("s1", ""))].into(),
        ..Default::default()
    };
    let local: BTreeMap<String, Entry> = [(RUN_A.to_string(), entry("a", "ingest-a")), (SCHEMA.to_string(), entry("s2", ""))].into();
    let m = merge(&remote, &local, "ingest-a", now).unwrap().manifest;
    assert_eq!(m.entries.keys().collect::<Vec<_>>(), [RUN_A, RUN_B, SCHEMA]);
    // A local copy of another node's key never replaces the owner's entry.
    let stale: BTreeMap<String, Entry> = [(RUN_B.to_string(), entry("stale", "ingest-b"))].into();
    assert_eq!(merge(&remote, &stale, "ingest-a", now).unwrap().manifest.entries[RUN_B].sha256, "b");
    assert_eq!(m.entries[SCHEMA].sha256, "s2", "the local copy wins");
    assert_eq!(m.tombstones.get(gone), Some(&Tombstone { owner: "ingest-a".into(), deleted_at: now }));
}

/// A tombstone whose owner differs from the owner of the entry it names raises `SyncTombstoneForeign`, and the
/// merge keeps the entry.
// spec: store.merge.tombstone-owner@df322a2c
#[test]
fn a_tombstone_naming_another_owners_entry_refuses_and_the_entry_stays() {
    let now = at("2030-01-01T00:00:00Z");
    let remote = BucketManifest {
        entries: [(RUN_B.to_string(), entry("b", "ingest-b"))].into(),
        tombstones: [(RUN_B.to_string(), Tombstone { owner: "ingest-c".into(), deleted_at: now })].into(),
    };
    let m = merge(&remote, &BTreeMap::new(), "ingest-a", now).unwrap();
    assert!(m.manifest.entries.contains_key(RUN_B));
    assert!(matches!(&m.refused[..], [StoreError::SyncTombstoneForeign(msg)] if msg.contains("ingest-c") && msg.contains("ingest-b")));
    // The owner's own tombstone removes a copy another node still lists.
    let owned = BucketManifest { tombstones: [(RUN_B.to_string(), Tombstone { owner: "ingest-b".into(), deleted_at: now })].into(), ..Default::default() };
    let local: BTreeMap<String, Entry> = [(RUN_B.to_string(), entry("b", "ingest-b"))].into();
    assert!(!merge(&owned, &local, "ingest-a", now).unwrap().manifest.entries.contains_key(RUN_B));
}

/// A tombstone leaves the manifest 30 d after its `deleted_at`.
// spec: store.merge.tombstone-ttl@30121692
#[test]
fn a_tombstone_leaves_the_manifest_after_30_days() {
    assert_eq!(TOMBSTONE_TTL_SECS, 30 * 86_400);
    let deleted = at("2030-01-01T00:00:00Z");
    let remote = BucketManifest { tombstones: [(RUN_B.to_string(), Tombstone { owner: "ingest-b".into(), deleted_at: deleted })].into(), ..Default::default() };
    let kept = merge(&remote, &BTreeMap::new(), "ingest-a", deleted.plus_secs(TOMBSTONE_TTL_SECS - 1)).unwrap();
    assert!(kept.manifest.tombstones.contains_key(RUN_B));
    let dropped = merge(&remote, &BTreeMap::new(), "ingest-a", deleted.plus_secs(TOMBSTONE_TTL_SECS)).unwrap();
    assert!(dropped.manifest.tombstones.is_empty());
}

/// Resolving a cursor by whichever copy was written last raises `SyncCursorConflict`; a cursor resolves through
/// its commit.
// spec: store.merge.cursor-recency@3e5813c0
#[test]
fn differing_copies_of_a_commit_log_entry_refuse() {
    let key = "research/cursors/feed/00000000000000000003.json";
    let remote = BucketManifest { entries: [(key.to_string(), entry("theirs", ""))].into(), ..Default::default() };
    let local: BTreeMap<String, Entry> = [(key.to_string(), entry("mine", ""))].into();
    assert!(matches!(merge(&remote, &local, "ingest-a", at("2030-01-01T00:00:00Z")), Err(StoreError::SyncCursorConflict(_))));
    let same: BTreeMap<String, Entry> = [(key.to_string(), entry("theirs", ""))].into();
    assert!(merge(&remote, &same, "ingest-a", at("2030-01-01T00:00:00Z")).is_ok());
}

/// A lease is granted for 10 min.
// spec: store.lease.ttl@e6b8444c
#[test]
fn a_lease_is_granted_for_10_minutes() {
    assert_eq!(TTL_SECS, 600);
    let now = at("2030-01-01T00:00:00Z");
    let (lease, _) = BucketLease::acquire(None, "ingest-a", now).unwrap();
    assert_eq!(lease.expires_at, Some(now.plus_secs(600)));
}

/// Acquisition creates the lease object under `If-None-Match`, or replaces an expired or released one under `If-
/// Match` on its ETag, the fence one past the object's.
// spec: store.lease.acquire@f2b819c5
#[test]
fn acquisition_creates_or_replaces_on_the_etag_with_the_next_fence() {
    let now = at("2030-01-01T00:00:00Z");
    let (first, cond) = BucketLease::acquire(None, "ingest-a", now).unwrap();
    assert_eq!((first.fence, cond), (1, Condition::IfNoneMatch));
    let released = BucketLease { holder: None, acquired_at: None, expires_at: None, fence: 1 };
    let (second, cond) = BucketLease::acquire(Some((&released, "etag-1")), "ingest-b", now).unwrap();
    assert_eq!((second.fence, cond, second.holder.as_deref()), (2, Condition::IfMatch("etag-1".into()), Some("ingest-b")));
    // An expired grant, past the skew bound, is taken over on its ETag.
    let (third, _) = BucketLease::acquire(Some((&second, "etag-2")), "ingest-a", now.plus_secs(600 + 31)).unwrap();
    assert_eq!(third.fence, 3);
    assert!(matches!(BucketLease::acquire(Some((&second, "etag-2")), "ingest-a", now.plus_secs(600 + 29)), Err(StoreError::LeaseHeld(_))));
}

/// Releasing a lease another node holds raises `LeaseNotHeld` and leaves the object untouched.
// spec: store.lease.not-held@9d654472
#[test]
fn releasing_another_nodes_lease_refuses() {
    let now = at("2030-01-01T00:00:00Z");
    let (held, _) = BucketLease::acquire(None, "ingest-a", now).unwrap();
    assert!(matches!(BucketLease::release(&held, "e", "ingest-b"), Err(StoreError::LeaseNotHeld(m)) if m.contains("ingest-a")));
    let (released, cond) = BucketLease::release(&held, "e", "ingest-a").unwrap();
    assert_eq!((released.holder, released.fence, cond), (None, 1, Condition::IfMatch("e".into())));
}

/// A bucket lease attempted under the node id `local` raises `LeaseNodeIdLocal`, logging the variable that sets a
/// node id; that machine keeps the machine lease.
// spec: store.lease.local-node@f57272a9
#[test]
fn a_bucket_lease_under_the_local_node_id_refuses_naming_the_variable() {
    match BucketLease::acquire(None, "local", at("2030-01-01T00:00:00Z")) {
        Err(StoreError::LeaseNodeIdLocal(m)) => assert!(m.contains("CONTEXTFUL_NODE_ID"), "{m}"),
        other => panic!("{other:?}"),
    }
}
