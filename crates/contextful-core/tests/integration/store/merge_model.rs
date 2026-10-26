//! `store.merge` as a state machine: three writers write, delete, pull and push against one
//! bucket manifest while the clock moves and forged tombstones arrive, and every push checks
//! the scoped union, owner-scoped deletion, idempotence and the tombstone TTL.

use super::at;
use contextful_core::store::sync::{merge_verified, owner_of, BucketManifest, Entry, HeldKeys, Merged, Tombstone, TOMBSTONE_TTL_SECS};
use contextful_core::store::StoreError;
use contextful_core::time::Instant;
use proptest::prelude::*;
use proptest_state_machine::{prop_state_machine, ReferenceStateMachine, StateMachineTest};
use std::collections::{BTreeMap, BTreeSet};

const NODES: [&str; 3] = ["ingest-a", "ingest-b", "ingest-c"];
const UNOWNED: &str = "research/tables/filings/schema.json";

/// Two run-directory keys per writer and one unowned key.
fn keys() -> Vec<String> {
    let mut out: Vec<String> = NODES.iter().flat_map(|n| (0..2).map(move |r| format!("research/tables/filings/data/runs/run-{r}/{n}/part-00000.parquet"))).collect();
    out.push(UNOWNED.to_string());
    out
}

fn entry(key: &str, sha: u8) -> Entry {
    Entry { sha256: format!("{sha:02x}"), size: 1, owner: owner_of(key).unwrap_or_default() }
}

#[derive(Clone, Debug)]
enum Op {
    /// Write `key` locally with content `sha`, whoever owns it.
    Write { node: usize, key: usize, sha: u8 },
    /// Delete `key` from the local store.
    Delete { node: usize, key: usize },
    /// Copy every entry another writer owns or no writer owns from the bucket, and drop each one its owner tombstoned.
    Pull { node: usize },
    /// Merge the local store into the bucket manifest and commit the result.
    Push { node: usize },
    /// A tombstone by `by` naming `key` arrives in the bucket, whoever owns the key.
    Forge { by: usize, key: usize },
    /// Move the clock forward.
    Tick { secs: u64 },
    /// Every writer pushes in turn, twice, at one `now`.
    Settle,
}

/// The reference only steers generation: the merge's expected output is checked as properties.
struct Writers;

impl ReferenceStateMachine for Writers {
    type State = ();
    type Transition = Op;

    fn init_state() -> BoxedStrategy<()> {
        Just(()).boxed()
    }

    fn transitions(_: &()) -> BoxedStrategy<Op> {
        let node = 0..NODES.len();
        let key = 0..keys().len();
        prop_oneof![
            4 => (node.clone(), key.clone(), 0u8..2).prop_map(|(node, key, sha)| Op::Write { node, key, sha }),
            2 => (node.clone(), key.clone()).prop_map(|(node, key)| Op::Delete { node, key }),
            2 => node.clone().prop_map(|node| Op::Pull { node }),
            4 => node.clone().prop_map(|node| Op::Push { node }),
            1 => (node, key).prop_map(|(by, key)| Op::Forge { by, key }),
            1 => prop_oneof![Just(0), Just(1), Just(86_400), Just(TOMBSTONE_TTL_SECS - 1), Just(TOMBSTONE_TTL_SECS)].prop_map(|secs| Op::Tick { secs }),
            1 => Just(Op::Settle),
        ]
        .boxed()
    }

    fn apply(state: (), _: &Op) {
        state
    }
}

/// One writer's store: its local entries and the owned keys its last sync held.
#[derive(Clone, Default)]
struct Store {
    local: BTreeMap<String, Entry>,
    held: BTreeSet<String>,
}

struct Bucket {
    remote: BucketManifest,
    stores: Vec<Store>,
    now: Instant,
}

impl Bucket {
    fn push(&mut self, node: usize) -> Result<Merged, StoreError> {
        let me = NODES[node];
        let store = &self.stores[node];
        let held = HeldKeys::Keys(store.held.clone());
        let merged = merge_verified(&self.remote, &store.local, &held, me, self.now, &|_, _| Ok(()))?;
        check_push(&self.remote, store, &held, me, self.now, &merged);
        self.remote = merged.manifest.clone();
        // The held record after a commit: the owned keys the store pushed.
        self.stores[node].held = store.local.keys().filter(|k| owner_of(k).as_deref() == Some(me)).cloned().collect();
        Ok(merged)
    }

    fn pull(&mut self, node: usize) {
        let me = NODES[node];
        let store = &mut self.stores[node];
        for (key, e) in &self.remote.entries {
            if e.owner != me {
                store.local.insert(key.clone(), e.clone());
            }
        }
        for (key, t) in &self.remote.tombstones {
            let owner = owner_of(key).unwrap_or_default();
            if owner != me && t.owner == owner {
                store.local.remove(key);
            }
        }
    }

    /// Whether every writer holding the unowned key holds the same copy: otherwise the last
    /// push's copy wins, and alternate pushes move the bucket without end.
    fn unowned_copies_agree(&self) -> bool {
        let copies: BTreeSet<&str> = self.stores.iter().filter_map(|s| s.local.get(UNOWNED)).map(|e| e.sha256.as_str()).collect();
        copies.len() <= 1
    }
}

/// The properties of one push from `remote` to `merged` by `me`.
fn check_push(remote: &BucketManifest, store: &Store, held: &HeldKeys, me: &str, now: Instant, merged: &Merged) {
    let out = &merged.manifest;
    // Owner-scoped deletion: an entry leaves only under a tombstone of the entry's own owner.
    for (key, e) in &remote.entries {
        if !out.entries.contains_key(key) {
            assert_eq!(out.tombstones.get(key).map(|t| t.owner.as_str()), Some(e.owner.as_str()), "`{key}` left without its owner's tombstone");
        }
    }
    for (key, t) in &out.tombstones {
        if !remote.tombstones.contains_key(key) {
            assert_eq!(t.owner, me, "`{key}`: a merge writes tombstones only for its own writer");
            assert!(held.holds(key) && !store.local.contains_key(key), "`{key}`: a tombstone for a key the store never held or still holds");
        }
    }
    // Foreign local entries are ignored: a key another writer owns lists the bucket's entry or none.
    for (key, e) in &store.local {
        if !e.owner.is_empty() && e.owner != me {
            if let Some(listed) = out.entries.get(key) {
                assert_eq!(Some(listed), remote.entries.get(key), "`{key}`: a local copy of another writer's entry was listed");
            }
        } else {
            assert_eq!(out.entries.get(key), Some(e), "`{key}`: the writer's own or unowned local entry is listed");
        }
    }
    // A tombstone naming an entry another writer owns refuses and keeps the entry.
    for (key, t) in &remote.tombstones {
        if t.deleted_at.secs_until(now) >= TOMBSTONE_TTL_SECS {
            continue;
        }
        if let Some(e) = out.entries.get(key) {
            if e.owner != t.owner {
                assert!(
                    merged.refused.iter().any(|r| matches!(r, StoreError::SyncTombstoneForeign(m) if m.contains(key.as_str()))),
                    "`{key}`: a foreign tombstone kept its entry without `SyncTombstoneForeign`"
                );
            }
        }
    }
    // The tombstone TTL: no tombstone 30 d past its `deleted_at` leaves the merge.
    for (key, t) in &out.tombstones {
        assert!(t.deleted_at.secs_until(now) < TOMBSTONE_TTL_SECS, "`{key}`: a tombstone past its TTL stayed");
    }
    // Idempotence: merging the same store into the result changes nothing.
    let again = merge_verified(out, &store.local, held, me, now, &|_, _| Ok(())).expect("a merge's own output merges");
    assert_eq!(&again.manifest, out, "a second merge of the same store moved the manifest");
}

impl StateMachineTest for Bucket {
    type SystemUnderTest = Bucket;
    type Reference = Writers;

    fn init_test(_: &()) -> Bucket {
        Bucket { remote: BucketManifest::default(), stores: vec![Store::default(); NODES.len()], now: at("2030-01-01T00:00:00Z") }
    }

    fn apply(mut bucket: Bucket, _: &(), op: Op) -> Bucket {
        let keys = keys();
        match op {
            Op::Write { node, key, sha } => {
                bucket.stores[node].local.insert(keys[key].clone(), entry(&keys[key], sha));
            }
            Op::Delete { node, key } => {
                bucket.stores[node].local.remove(&keys[key]);
            }
            Op::Pull { node } => bucket.pull(node),
            Op::Push { node } => {
                bucket.push(node).expect("no commit-log key is drawn, so a merge never refuses outright");
            }
            Op::Forge { by, key } => {
                bucket.remote.tombstones.insert(keys[key].clone(), Tombstone::unsigned(NODES[by], bucket.now));
            }
            Op::Tick { secs } => bucket.now = bucket.now.plus_secs(secs),
            Op::Settle => {
                for node in 0..NODES.len() {
                    bucket.push(node).expect("a settling push merges");
                }
                let settled = bucket.remote.clone();
                for node in 0..NODES.len() {
                    bucket.push(node).expect("a settling push merges");
                }
                // A fixed point at a fixed `now`: once every writer has pushed, a second round moves nothing.
                if bucket.unowned_copies_agree() {
                    assert_eq!(bucket.remote, settled, "a second round of pushes at one `now` moved the manifest");
                }
            }
        }
        bucket
    }
}

prop_state_machine! {
    #![proptest_config(ProptestConfig { cases: 256, failure_persistence: None, ..ProptestConfig::default() })]

    /// A merge takes each local entry its writer owns or no writer owns, and every other remote entry; an entry leaves
    /// only under its owner's tombstone, a foreign tombstone keeps its entry, a tombstone past 30 d leaves, a second merge
    /// of the same store is a no-op, and writers pushing in turn at one `now` reach a fixed point.
    #[test]
    fn writers_merging_in_any_order_keep_the_scoped_union(sequential 1..48 => Bucket);
}
