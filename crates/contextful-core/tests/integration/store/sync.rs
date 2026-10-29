//! `store.push`, `store.merge` and `store.lease`: prefixes, the scoped-union merge and the lease object.

use super::at;
use contextful_core::store::lease::{BucketLease, CLOCK_SKEW_SECS, RENEWAL_SECS, TTL_SECS};
use contextful_core::store::object::Condition;
use contextful_core::connector::reference::SecretName;
use contextful_core::store::sync::{
    confine, merge, merge_verified, owner_of, BucketManifest, HeldKeys, ControlHead, CredentialRef, Endpoint, Entry, Signing, SyncConfig, Tombstone, DEFAULT_REGION, TOMBSTONE_TTL_SECS,
};
use contextful_core::store::StoreError;
use std::collections::BTreeMap;

fn entry(sha: &str, owner: &str) -> Entry {
    Entry { sha256: sha.into(), size: 1, owner: owner.into() }
}

const RUN_A: &str = "research/tables/filings/data/runs/run-1/ingest-a/part-00000.parquet";
const RUN_B: &str = "research/tables/filings/data/runs/run-1/ingest-b/part-00000.parquet";
const SCHEMA: &str = "research/tables/filings/schema.json";

#[test]
fn merge_keeps_each_projects_control_head_and_legacy_manifests_read_empty() {
    let old: BucketManifest = serde_json::from_str(r#"{"entries":{}}"#).unwrap();
    assert!(old.control_heads.is_empty());
    let head = ControlHead { version: 2, receipt_sha256: "a".repeat(64) };
    let remote = BucketManifest { control_heads: [("research".into(), head.clone())].into(), ..Default::default() };
    let merged = merge(&remote, &BTreeMap::new(), "ingest-a", at("2030-01-01T00:00:00Z")).unwrap();
    assert_eq!(merged.manifest.control_heads["research"], head);
    let round_trip: BucketManifest = serde_json::from_slice(&serde_json::to_vec(&merged.manifest).unwrap()).unwrap();
    assert_eq!(round_trip.control_heads, remote.control_heads);
}

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

/// A key names its owner by the node in its run directory, request-ledger filename, commit-log directory or run-state
/// directory; any other key is unowned and propagates no deletion.
// spec: store.merge.ownership@cd44833c
#[test]
fn a_keys_owner_is_read_off_the_key() {
    assert_eq!(owner_of(RUN_A).as_deref(), Some("ingest-a"));
    assert_eq!(owner_of("research/tables/filings/requests/run-1.ingest-b.parquet").as_deref(), Some("ingest-b"));
    assert_eq!(owner_of("research/tables/filings/requests/run.2030.01.ingest-b.parquet").as_deref(), Some("ingest-b"), "a dotted run id");
    assert_eq!(owner_of("research/cursors/feed/ingest-c/00000000000000000001.json").as_deref(), Some("ingest-c"));
    assert_eq!(owner_of("research/nodes/ingest-e/run-state.json").as_deref(), Some("ingest-e"));
    assert_eq!(owner_of("acme/research/nodes/ingest-e/run-state.json").as_deref(), Some("ingest-e"), "a project of two segments");
    // A table named `runs`, or nesting `data/runs` in its name, reads its own run directory.
    assert_eq!(owner_of("research/tables/runs/data/runs/run-1/ingest-d/part-00000.parquet").as_deref(), Some("ingest-d"));
    assert_eq!(owner_of("runs/x"), None, "a key opening with `runs` has no underflow");
    assert_eq!(owner_of("research/tables/a/data/runs/run-1/_manifest.json"), None, "a short run path owns nothing");
    for unowned in [SCHEMA, "research/tables/filings/data/snapshots/snapshot-01/part-00000.parquet", "research/cursors/feed/00000000000000000001.json", "research/nodes/run-state.json", "research/nodes/ingest-e/other.json"] {
        assert_eq!(owner_of(unowned), None, "{unowned}");
    }
    // An unowned key the writer no longer holds stays listed: it propagates no deletion.
    let remote = BucketManifest { entries: [(SCHEMA.to_string(), entry("s", ""))].into(), ..Default::default() };
    let m = merge(&remote, &BTreeMap::new(), "ingest-a", at("2030-01-01T00:00:00Z")).unwrap();
    assert!(m.manifest.entries.contains_key(SCHEMA) && m.manifest.tombstones.is_empty());
}

/// A merge takes each local entry this writer owns or no node owns, and every other remote entry; a remote entry it
/// owns leaves with a tombstone only where the held record lists it and the store no longer holds it.
// spec: store.merge.scoped-union@fc9577a4
#[test]
fn a_merge_tombstones_only_the_owned_entries_its_store_held() {
    let now = at("2030-01-01T00:00:00Z");
    let gone = "research/tables/filings/data/runs/run-0/ingest-a/part-00000.parquet";
    let sibling = "research/tables/filings/data/runs/run-9/ingest-a/part-00000.parquet";
    let remote = BucketManifest {
        entries: [(gone.to_string(), entry("old", "ingest-a")), (sibling.to_string(), entry("sib", "ingest-a")), (RUN_B.to_string(), entry("b", "ingest-b"))].into(),
        ..Default::default()
    };
    let local: BTreeMap<String, Entry> = [(RUN_A.to_string(), entry("a", "ingest-a"))].into();
    let held = HeldKeys::Keys([gone.to_string(), RUN_A.to_string()].into());
    let m = merge_verified(&remote, &local, &held, "ingest-a", now, &|_, _| Ok(())).unwrap().manifest;
    assert_eq!(m.entries.keys().collect::<Vec<_>>(), [RUN_A, RUN_B, sibling], "an owned entry the store never held stays listed");
    assert_eq!(m.entries[sibling].sha256, "sib");
    assert_eq!(m.tombstones.keys().collect::<Vec<_>>(), [gone], "the held entry the store lost leaves");
    // A store holding every owned key the remote lists tombstones each it lacks.
    let every = merge_verified(&remote, &local, &HeldKeys::Every, "ingest-a", now, &|_, _| Ok(())).unwrap().manifest;
    assert_eq!(every.tombstones.keys().collect::<Vec<_>>(), [gone, sibling]);
}

/// [`merge`] reads the store as holding every owned key the remote lists.
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
    assert_eq!(m.tombstones.get(gone), Some(&Tombstone::unsigned("ingest-a", now)));
}

/// A tombstone whose owner differs from the owner of the entry it names raises `SyncTombstoneForeign`, and the
/// merge keeps the entry.
// spec: store.merge.tombstone-owner@df322a2c
#[test]
fn a_tombstone_naming_another_owners_entry_refuses_and_the_entry_stays() {
    let now = at("2030-01-01T00:00:00Z");
    let remote = BucketManifest {
        entries: [(RUN_B.to_string(), entry("b", "ingest-b"))].into(),
        tombstones: [(RUN_B.to_string(), Tombstone::unsigned("ingest-c", now))].into(),
        ..Default::default()
    };
    let m = merge(&remote, &BTreeMap::new(), "ingest-a", now).unwrap();
    assert!(m.manifest.entries.contains_key(RUN_B));
    assert!(matches!(&m.refused[..], [StoreError::SyncTombstoneForeign(msg)] if msg.contains("ingest-c") && msg.contains("ingest-b")));
    // The owner's own tombstone removes a copy another node still lists.
    let owned = BucketManifest { tombstones: [(RUN_B.to_string(), Tombstone::unsigned("ingest-b", now))].into(), ..Default::default() };
    let local: BTreeMap<String, Entry> = [(RUN_B.to_string(), entry("b", "ingest-b"))].into();
    assert!(!merge(&owned, &local, "ingest-a", now).unwrap().manifest.entries.contains_key(RUN_B));
}

/// A tombstone leaves the manifest 30 d after its `deleted_at`.
// spec: store.merge.tombstone-ttl@30121692
#[test]
fn a_tombstone_leaves_the_manifest_after_30_days() {
    assert_eq!(TOMBSTONE_TTL_SECS, 30 * 86_400);
    let deleted = at("2030-01-01T00:00:00Z");
    let remote = BucketManifest { tombstones: [(RUN_B.to_string(), Tombstone::unsigned("ingest-b", deleted))].into(), ..Default::default() };
    let kept = merge(&remote, &BTreeMap::new(), "ingest-a", deleted.plus_secs(TOMBSTONE_TTL_SECS - 1)).unwrap();
    assert!(kept.manifest.tombstones.contains_key(RUN_B));
    let dropped = merge(&remote, &BTreeMap::new(), "ingest-a", deleted.plus_secs(TOMBSTONE_TTL_SECS)).unwrap();
    assert!(dropped.manifest.tombstones.is_empty());
}

/// A merge drops every key a standing collected record names, a local copy and a remote entry alike, tombstones
/// none of them, and carries the record until 30 d after its instant.
// spec: store.merge.collected@ae4c5844
#[test]
fn a_collected_record_keeps_its_key_out_of_every_merge_until_it_ages_out() {
    use contextful_core::store::sync::Collected;
    const PART: &str = "research/tables/filings/data/snapshots/s1/part-00000.parquet";
    let collected_at = at("2030-01-01T00:00:00Z");
    let record = || Collected { collected_at, collected_by: "ingest-b".into() };
    let remote = BucketManifest {
        entries: [(SCHEMA.to_string(), entry("s", "")), (RUN_A.to_string(), entry("r", "ingest-a"))].into(),
        collected: [(PART.to_string(), record()), (RUN_A.to_string(), record())].into(),
        ..Default::default()
    };
    let local: BTreeMap<String, Entry> = [(PART.to_string(), entry("p", "")), (RUN_A.to_string(), entry("r", "ingest-a"))].into();
    let merged = merge(&remote, &local, "ingest-a", collected_at.plus_secs(TOMBSTONE_TTL_SECS - 1)).unwrap().manifest;
    assert!(merged.entries.contains_key(SCHEMA));
    assert!(!merged.entries.contains_key(PART) && !merged.entries.contains_key(RUN_A), "{:?}", merged.entries.keys().collect::<Vec<_>>());
    assert!(merged.tombstones.is_empty(), "a collected key propagates no deletion");
    assert_eq!(merged.collected, remote.collected);
    let aged = merge(&remote, &local, "ingest-a", collected_at.plus_secs(TOMBSTONE_TTL_SECS)).unwrap().manifest;
    assert!(aged.collected.is_empty());
    assert!(aged.entries.contains_key(PART) && aged.entries.contains_key(RUN_A), "a local copy lists again once the record leaves");
}

/// A manifest holding no collected record writes format 1 and one holding a record writes 2; this build parses
/// format 2, and a manifest without `format` reads as 1.
// spec: store.push.manifest-format@bd112bbd
#[test]
fn a_manifest_carries_format_two_only_with_a_collected_record() {
    use contextful_core::store::sync::MANIFEST_FORMAT;
    use contextful_core::store::sync::Collected;
    assert_eq!(MANIFEST_FORMAT, 2);
    assert_eq!(BucketManifest::default().written_format(), 1);
    let collected = BucketManifest { collected: [(RUN_A.to_string(), Collected { collected_at: at("2030-01-01T00:00:00Z"), collected_by: "ingest-b".into() })].into(), ..Default::default() };
    assert_eq!(collected.written_format(), 2);
    let legacy: BucketManifest = serde_json::from_str(r#"{"entries":{}}"#).unwrap();
    assert_eq!(legacy.format, 1);
    assert!(legacy.collected.is_empty());
}

/// `[sync] retain_generations` defaults to 64 generations; a value of 0 fails the configuration's parse.
// spec: store.collect.retain-generations@9aa2317d
#[test]
fn retain_generations_defaults_to_64_and_refuses_zero() {
    use contextful_core::store::sync::RETAIN_GENERATIONS;
    assert_eq!(RETAIN_GENERATIONS, 64);
    assert_eq!(SyncConfig::default().retain_generations(), 64);
    let parse = |n: u64| toml::from_str::<SyncConfig>(&format!("endpoint = 'file://b'\nbucket = 'b'\nretain_generations = {n}\n"));
    assert_eq!(parse(3).unwrap().retain_generations(), 3);
    assert!(parse(0).is_err(), "a store keeping no generation keeps no current state");
}

/// Resolving a cursor by last-write time raises `SyncCursorConflict`. A cursor resolves through its commit.
// spec: store.merge.cursor-recency@a551b135
#[test]
fn differing_copies_of_a_commit_log_entry_refuse() {
    let key = "research/cursors/feed/00000000000000000003.json";
    let remote = BucketManifest { entries: [(key.to_string(), entry("theirs", ""))].into(), ..Default::default() };
    let local: BTreeMap<String, Entry> = [(key.to_string(), entry("mine", ""))].into();
    assert!(matches!(merge(&remote, &local, "ingest-a", at("2030-01-01T00:00:00Z")), Err(StoreError::SyncCursorConflict(_))));
    let same: BTreeMap<String, Entry> = [(key.to_string(), entry("theirs", ""))].into();
    assert!(merge(&remote, &same, "ingest-a", at("2030-01-01T00:00:00Z")).is_ok());
}

/// A writer lease grants ownership for 10 min.
// spec: store.lease.ttl@742eaac5
#[test]
fn a_lease_is_granted_for_10_minutes() {
    assert_eq!(TTL_SECS, 600);
    let now = at("2030-01-01T00:00:00Z");
    let (lease, _) = BucketLease::acquire(None, "ingest-a", now).unwrap();
    assert_eq!(lease.expires_at, Some(now.plus_secs(600)));
}

/// Acquisition creates the lease object under `If-None-Match`, or replaces an expired or released one under `If-
/// Match` on its ETag, the fence one past the object's.
// spec: store.lease.acquire@0a8044ac
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

/// The process holding a compaction lease renews it every 200 s by `If-Match` replace on its ETag; `sync compact`
/// renews while it pulls, folds and pushes.
// spec: store.lease.renewal@176cf746
#[test]
fn a_holder_renews_every_200_seconds_on_the_etag_it_holds() {
    assert_eq!(RENEWAL_SECS, 200);
    const { assert!(RENEWAL_SECS * 2 < TTL_SECS, "one missed renewal leaves the grant standing") };
    let now = at("2030-01-01T00:00:00Z");
    let (held, _) = BucketLease::acquire(None, "ingest-a", now).unwrap();
    let later = now.plus_secs(RENEWAL_SECS);
    let (renewed, cond) = BucketLease::renew(&held, "etag-1", &held, later).unwrap();
    assert_eq!(cond, Condition::IfMatch("etag-1".into()));
    assert_eq!((renewed.fence, renewed.holder.as_deref(), renewed.expires_at), (held.fence, Some("ingest-a"), Some(later.plus_secs(TTL_SECS))));
    // A lease another acquisition moved past this holder's fence refuses the renewal.
    let taken = BucketLease { holder: Some("ingest-b".into()), fence: held.fence + 1, ..held.clone() };
    assert!(matches!(BucketLease::renew(&taken, "etag-2", &held, later), Err(StoreError::LeaseFenced(_))));
}

/// Bucket leasing assumes the clocks of two machines differ by 30 s or less, and every expiry judgment reads the
/// judging machine's monotonic clock.
// spec: store.lease.clock-skew@3f066dfc
#[test]
fn an_expiry_judgment_allows_30_seconds_of_skew_on_the_judging_clock() {
    assert_eq!(CLOCK_SKEW_SECS, 30);
    let granted = at("2030-01-01T00:00:00Z");
    let (held, _) = BucketLease::acquire(None, "ingest-a", granted).unwrap();
    let expiry = granted.plus_secs(TTL_SECS);
    // The judging machine's own reading decides: the grant stands until expiry plus the skew bound.
    assert!(held.held_against("ingest-b", expiry));
    assert!(held.held_against("ingest-b", expiry.plus_secs(CLOCK_SKEW_SECS - 1)));
    assert!(!held.held_against("ingest-b", expiry.plus_secs(CLOCK_SKEW_SECS)));
    // The holder's own lease never reads as held against it.
    assert!(!held.held_against("ingest-a", granted));
}

/// If a run finds an unexpired lease, the engine raises `LeaseHeld`, names the holder and expiry, and skips the run.
/// The engine retries the run at the next reconciliation tick.
// spec: store.lease.held@82744098
#[test]
fn an_unexpired_lease_refuses_naming_its_holder_and_expiry_and_a_later_tick_takes_it() {
    let granted = at("2030-01-01T00:00:00Z");
    let (held, _) = BucketLease::acquire(None, "ingest-a", granted).unwrap();
    let expiry = granted.plus_secs(TTL_SECS);
    match BucketLease::acquire(Some((&held, "etag-1")), "ingest-b", granted.plus_secs(60)) {
        Err(StoreError::LeaseHeld(m)) => {
            assert!(m.contains("`ingest-a`") && m.contains(&expiry.to_string()), "{m}");
            assert!(m.contains("skipped") && m.contains("next tick"), "{m}");
        }
        other => panic!("{other:?}"),
    }
    // The next attempt after the grant lapses takes the lease at the next fence.
    let (taken, _) = BucketLease::acquire(Some((&held, "etag-1")), "ingest-b", expiry.plus_secs(CLOCK_SKEW_SECS)).unwrap();
    assert_eq!((taken.holder.as_deref(), taken.fence), (Some("ingest-b"), held.fence + 1));
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

fn endpoint(e: &str) -> SyncConfig {
    SyncConfig { endpoint: e.into(), bucket: "context-team".into(), ..SyncConfig::default() }
}

/// `[sync] endpoint` maps `file://<directory>` to a filesystem bucket and `s3://<region>` to regional AWS S3.
/// `r2://<account-id>` selects that account's R2 endpoint at region `auto`. `https://<host>` uses `[sync] region`,
/// default `us-east-1`.
// spec: store.endpoint.schemes@40b78d25
#[test]
fn each_endpoint_scheme_resolves_its_adapter_address() {
    assert_eq!(endpoint("file:///srv/buckets").resolve_endpoint().unwrap(), Endpoint::File("/srv/buckets".into()));
    assert_eq!(
        endpoint("s3://eu-west-1").resolve_endpoint().unwrap(),
        Endpoint::S3 { url: "https://s3.eu-west-1.amazonaws.com".into(), region: "eu-west-1".into() }
    );
    assert_eq!(
        endpoint("r2://0123456789abcdef0123456789abcdef").resolve_endpoint().unwrap(),
        Endpoint::S3 { url: "https://0123456789abcdef0123456789abcdef.r2.cloudflarestorage.com".into(), region: "auto".into() }
    );
    assert_eq!(
        endpoint("https://objects.example.org/").resolve_endpoint().unwrap(),
        Endpoint::S3 { url: "https://objects.example.org".into(), region: DEFAULT_REGION.into() }
    );
    let regional = SyncConfig { region: Some("ap-east-1".into()), ..endpoint("https://objects.example.org:9443") };
    assert_eq!(regional.resolve_endpoint().unwrap(), Endpoint::S3 { url: "https://objects.example.org:9443".into(), region: "ap-east-1".into() });
    assert_eq!(DEFAULT_REGION, "us-east-1");
}

/// Any other scheme, or an S3 or R2 endpoint in a build without the `s3-sync` feature, raises
/// `SyncEndpointUnsupported`, naming the endpoint.
// spec: store.endpoint.unsupported-scheme@ea974bcd
#[test]
fn an_endpoint_no_adapter_answers_is_refused_by_name() {
    for e in ["ftp://objects.example.org", "objects.example.org", "s3://", "s3://EU_WEST", "r2://", "file://", "https://", "https://key:secret@objects.example.org"] {
        match endpoint(e).resolve_endpoint() {
            Err(StoreError::SyncEndpointUnsupported(m)) => assert!(m.contains(&format!("`{e}`")), "{e}: {m}"),
            other => panic!("{e}: {other:?}"),
        }
    }
}

/// An `http://` endpoint opens on a loopback host alone; any other host raises `SyncEndpointInsecure`, naming it.
// spec: store.endpoint.plaintext@2ce87d3b
#[test]
fn plaintext_http_opens_on_loopback_alone() {
    for e in ["http://127.0.0.1:9000", "http://localhost:9000/", "http://[::1]:9000"] {
        assert!(matches!(endpoint(e).resolve_endpoint(), Ok(Endpoint::S3 { .. })), "{e}");
    }
    for (e, host) in [("http://objects.example.org", "objects.example.org"), ("http://10.0.0.8:9000", "10.0.0.8")] {
        match endpoint(e).resolve_endpoint() {
            Err(StoreError::SyncEndpointInsecure(m)) => assert!(m.contains(&format!("`{host}`")), "{m}"),
            other => panic!("{e}: {other:?}"),
        }
    }
}

/// `[sync] access_key_id`, `secret_access_key` and the optional `session_token` each parse as a `secret://<name>` or
/// an `env://NAME` reference.
// spec: connector.reference.whole-value-binding@a5f6354c
#[test]
fn credential_keys_bind_secret_or_environment_references() {
    let c = SyncConfig {
        access_key_id: Some("env://SYNC_ACCESS_KEY_ID".into()),
        secret_access_key: Some("secret://sync-secret-access-key".into()),
        ..endpoint("r2://account")
    };
    let refs = c.credential_refs().unwrap();
    assert_eq!(refs.access_key_id, CredentialRef::Env("SYNC_ACCESS_KEY_ID".into()));
    assert_eq!(refs.secret_access_key, CredentialRef::Secret(SecretName::parse("sync-secret-access-key").unwrap()));
    assert_eq!(refs.session_token, None);
    let temporary = SyncConfig { session_token: Some("secret://sync-session".into()), ..c };
    assert_eq!(temporary.credential_refs().unwrap().session_token, Some(CredentialRef::Secret(SecretName::parse("sync-session").unwrap())));
}

/// An S3 or R2 `[sync]` omitting `access_key_id` or `secret_access_key`, supplying a non-reference credential, or
/// naming an unset variable raises `SyncCredentialUnbound`, naming the key.
// spec: store.endpoint.credential-unbound@769f258e
#[test]
fn an_unbound_or_literal_credential_key_is_refused_naming_the_key() {
    let bound = SyncConfig { access_key_id: Some("env://A".into()), secret_access_key: Some("env://B".into()), ..endpoint("s3://eu-west-1") };
    let cases = [
        (SyncConfig { access_key_id: None, ..bound.clone() }, "access_key_id"),
        (SyncConfig { secret_access_key: None, ..bound.clone() }, "secret_access_key"),
        (SyncConfig { secret_access_key: Some("wJalrXUtnFEMIK7MDENGbPxRfiCYEXAMPLEKEY".into()), ..bound.clone() }, "secret_access_key"),
        (SyncConfig { access_key_id: Some("secret://Not_A_Name".into()), ..bound.clone() }, "access_key_id"),
        (SyncConfig { session_token: Some("env://9LIVES".into()), ..bound.clone() }, "session_token"),
    ];
    for (c, key) in cases {
        match c.credential_refs() {
            Err(StoreError::SyncCredentialUnbound(m)) => {
                assert!(m.contains(key), "{key}: {m}");
                assert!(!m.contains("wJalrXUtnFEMI"), "the refusal never repeats material: {m}");
            }
            other => panic!("{key}: {other:?}"),
        }
    }
}

/// `credentials = "role"` beside `access_key_id`, `secret_access_key` or `session_token`, or on an R2 endpoint,
/// raises `SyncCredentialConflict`, naming the endpoint; alone on an S3 or HTTPS endpoint it selects role signing.
// spec: store.endpoint.role-conflict@ea998d83
#[test]
fn role_credentials_beside_a_key_or_on_r2_conflict_naming_the_endpoint() {
    let role = |e: &str| SyncConfig { credentials: Some("role".into()), ..endpoint(e) };
    let parsed: SyncConfig = toml::from_str("endpoint = \"s3://eu-west-1\"\nbucket = \"b\"\ncredentials = \"role\"\n").unwrap();
    assert_eq!(parsed.signing().unwrap(), Signing::Role);
    assert_eq!(role("https://objects.example.org").signing().unwrap(), Signing::Role);
    let cases = [
        (SyncConfig { access_key_id: Some("env://A".into()), ..role("s3://eu-west-1") }, "s3://eu-west-1", "access_key_id"),
        (SyncConfig { secret_access_key: Some("secret://s".into()), ..role("s3://eu-west-1") }, "s3://eu-west-1", "secret_access_key"),
        (SyncConfig { session_token: Some("env://T".into()), ..role("https://objects.example.org") }, "https://objects.example.org", "session_token"),
        (role("r2://account"), "r2://account", "R2"),
    ];
    for (c, named, why) in cases {
        match c.signing() {
            Err(StoreError::SyncCredentialConflict(m)) => assert!(m.contains(named) && m.contains(why), "{why}: {m}"),
            other => panic!("{why}: {other:?}"),
        }
    }
    // Without `credentials`, the keys sign as before.
    let keys = SyncConfig { access_key_id: Some("env://A".into()), secret_access_key: Some("env://B".into()), ..endpoint("s3://eu-west-1") };
    assert!(matches!(keys.signing().unwrap(), Signing::Keys(_)));
    assert!(matches!(SyncConfig { credentials: Some("instance".into()), ..endpoint("s3://eu-west-1") }.signing(), Err(StoreError::SyncCredentialUnbound(_))));
}

#[test]
fn push_after_run_is_an_optional_sync_key() {
    let declared: SyncConfig = toml::from_str("endpoint = \"file:///tmp/b\"\nbucket = \"b\"\npush_after_run = true\n").unwrap();
    assert_eq!(declared.push_after_run, Some(true));
    let undeclared: SyncConfig = toml::from_str("endpoint = \"file:///tmp/b\"\nbucket = \"b\"\n").unwrap();
    assert_eq!(undeclared.push_after_run, None);
}

/// `transfers` sets a sync's transfer width: 16 when absent, and never below 1.
#[test]
fn transfers_sets_the_transfer_width_from_one() {
    let parse = |extra: &str| toml::from_str::<SyncConfig>(&format!("endpoint = \"file:///tmp/b\"\nbucket = \"b\"\n{extra}")).unwrap();
    assert_eq!(parse("").transfers(), 16);
    assert_eq!(parse("transfers = 4\n").transfers(), 4);
    assert_eq!(parse("transfers = 0\n").transfers(), 1);
}

/// `lazy_parts` is off and `part_cache_bytes` reads 8 GiB unless declared.
#[test]
fn lazy_parts_is_off_and_the_part_cache_reads_eight_gib_by_default() {
    let parse = |extra: &str| toml::from_str::<SyncConfig>(&format!("endpoint = \"file:///tmp/b\"\nbucket = \"b\"\n{extra}")).unwrap();
    assert!(!parse("").lazy_parts());
    assert_eq!(parse("").part_cache_bytes(), 8 << 30);
    let declared = parse("lazy_parts = true\npart_cache_bytes = 1048576\n");
    assert!(declared.lazy_parts());
    assert_eq!(declared.part_cache_bytes(), 1 << 20);
}

/// A part source's fold hold defaults to its read hold, and an oversize fold refuses by name.
#[test]
fn a_part_sources_fold_hold_defaults_to_its_read_hold() {
    use contextful_core::store::object::{PartHold, PartSource};
    use contextful_core::store::StoreError;
    #[derive(Debug, Default)]
    struct Counting(std::sync::atomic::AtomicUsize);
    impl PartSource for Counting {
        fn hold(&self, files: &[String]) -> Result<PartHold, StoreError> {
            self.0.fetch_add(files.len(), std::sync::atomic::Ordering::SeqCst);
            Ok(PartHold(std::sync::Arc::new(())))
        }
    }
    let source = Counting::default();
    source.hold_inputs(&["tables/t/a.parquet".into(), "tables/t/b.parquet".into()]).unwrap();
    assert_eq!(source.0.load(std::sync::atomic::Ordering::SeqCst), 2);
    assert!(StoreError::FoldInputsExceedCache("x".into()).to_string().starts_with("FoldInputsExceedCache: "));
}

/// A manifest carries its commit stamp through a round trip, and one predecessing the stamp reads none.
#[test]
fn a_manifest_carries_its_commit_stamp_and_a_legacy_one_reads_none() {
    let legacy: BucketManifest = serde_json::from_str(r#"{"format":1,"generation":3,"entries":{}}"#).unwrap();
    assert_eq!((legacy.committed_at, legacy.committed_by.as_deref()), (None, None));
    let stamped = BucketManifest { committed_at: Some(at("2030-01-01T01:00:00Z")), committed_by: Some("ingest-a".into()), ..legacy };
    let text = serde_json::to_string(&stamped).unwrap();
    assert!(text.contains("\"committed_at\":\"2030-01-01T01:00:00Z\"") && text.contains("\"committed_by\":\"ingest-a\""), "{text}");
    assert_eq!(serde_json::from_str::<BucketManifest>(&text).unwrap(), stamped);
}
