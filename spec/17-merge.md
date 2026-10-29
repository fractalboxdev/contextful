---
contract: store
owns:
  - merge
---

# Bucket manifest merges

A push commits the bucket manifest by merging its own entries into the remote ones under a conditional replace.

## merge

Merging bucket manifests across writers, verifying tombstones and retrying conditional manifest commits.

- `tombstone-owner` — A tombstone whose owner differs from the owner of the entry it names raises `SyncTombstoneForeign`, and the merge keeps the entry.
  *A-store#bucket-index-commits*
- `tombstone-signed` — Push signs each {{type:contextful_core::store::sync::Tombstone}} with the node's issuer key over the key it names, its owner and its deletion instant. A node without an issuer key writes unsigned tombstones.
  *A-store#bucket-index-commits*
- `tombstone-unverified` — Unsigned tombstones, invalid signatures, or keys absent from the key-set ledger's verifying set at `deleted_at` raise `SyncTombstoneUnverified`. Merge preserves the named entry; pull deletes nothing.
  *A-store#bucket-index-commits*
- `tombstone-ttl` — A tombstone leaves the manifest 30 d after its `deleted_at`.
- `retries` — `[sync] push_retries`, default 5 attempts, bounds consecutive re-commit rounds whose re-read manifest carries no newer generation than the round before.
  *A-store#bucket-index-commits*
- `round-ceiling` — A push stops its manifest commit loop after 64 rounds, whatever generations those rounds meet.
  *A-store#bucket-index-commits*
- `exhausted` — Exhausting {{store.merge.retries}} or {{store.merge.round-ceiling}} raises `SyncManifestRebaseExhausted`, reports every uploaded object as in the bucket and asks for a re-run.
  *A-store#bucket-index-commits*
- `cursor-recency` — Resolving a cursor by last-write time raises `SyncCursorConflict`. A cursor resolves through its commit.
  *A-run#incremental-position-bound*
- `scoped-union` — A merge takes local entries owned by this writer or unowned, and all other remote entries. It tombstones a writer-owned remote entry only when {{store.merge.held-record}} lists the key and the store lacks the file.
  *A-store#bucket-index-commits*
- `held-record` — Push records locally held owned keys under its node id in `.sync-held.json`, adding before commit and replacing that node's keys after; a pull holding the push guard adds each such key the bucket manifest lists.
  *A-store#bucket-index-commits*
- `held-absent` — With no keys for the node in `.sync-held.json`, a surviving `.push.lock` means the store holds every remote key the node owns, and none without it.
  *A-store#bucket-index-commits*
- `held-unreadable` — An unreadable `.sync-held.json` raises `SyncHeldRecordUnreadable` without granting deletion authority or replacing the record.
  *because corruption establishes no ownership and a replacement discards other nodes' entries*
- `collected-directory` — A merge keeps out each key under a snapshot directory whose `_collected.json` the bucket lists, save its `_manifest.json`, and each run file {{store.collect.folded-runs}} reclaims, whatever any record says.
  *because a lapsed record otherwise lets a node holding a copy list a reclaimed key again*
- `collected-index` — A push finds the snapshot folding each candidate run of {{store.merge.collected-directory}} in the pointer's valid bucket index, reading that snapshot's manifest for its parent; without a valid index it walks the chain.
  *A-store#index-walk*
- `collected` — A merge drops from its entries each key a collected record names, a local copy included, and carries the record, unsigned, until it reaches the age {{store.merge.tombstone-ttl}} sets.
  *A-store#bucket-collect*
- `ownership` — A key names its owner by the node in its run directory, request-ledger filename, commit-log directory or run-state directory; any other key is unowned and propagates no deletion.
  *A-store#bucket-index-commits*
