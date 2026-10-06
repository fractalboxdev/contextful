---
contract: store
---

# The store and its sync

## What it is for

The store is the canonical corpus of **Contextful**: Parquet any SQL tool opens, JSON manifests naming which files count, and one pointer per table naming the current snapshot ({{store.lay-out.components}}). No reader sees a torn state and no crash loses or duplicates a committed batch, with nothing stronger than a conditional write underneath.

## How it works

`contextful init` names the project ({{store.init.declaration-file}}), gives its store a synced identity ({{store.init.store-identity}}), and declares its authoring posture ({{store.init.posture}}); commands below it find it ({{store.init.discovery}}).

Files never change once written ({{store.lay-out.immutable-files}}). A write adds a run directory and commits by conditionally creating that run's manifest ({{store.lay-out.run-manifest}}); until the manifest exists, the run's parts join no read ({{store.lay-out.uncommitted-run}}). The pipeline's cursor rides inside the same commit, so rows and position land together ({{store.lay-out.cursor-in-commit}}).

A read resolves an explicit sorted file list from the pointer and the manifests, never a glob ({{store.reconcile.explicit-file-list}}). An unkeyed table reads as the union of its committed runs ({{store.declare.unkeyed-union}}); a keyed table reads through a view that keeps one survivor per key ({{store.declare.dedup-view}}), picked by `order_by` ({{store.declare.order-by-default}}). Schemas grow additively: a new column joins and older files read it as null ({{store.reconcile.additive}}). The type lattice admits one promotion ({{store.reconcile.lattice}}), and any other clash refuses at the write ({{store.reconcile.incompatible}}).

A complete empty replacement marks a frontier with no files ({{store.declare.empty-replacement}}).

Runs accumulate until a fold compacts them. A pass writes Parquet and every sidecar into staging ({{store.fold.pass}}), then publishes by replacing `_pointer.json` conditioned on the ETag it read at the start ({{store.fold.pointer-commit}}). Readers see a snapshot and its sidecars together or not at all ({{store.fold.partial-snapshot}}), and a statement in flight keeps the snapshot it started on ({{store.fold.non-blocking}}).

Two SQLite catalogs sit beside the files. `derived.sqlite` is a disposable cache rebuilt from the tree ({{store.lay-out.derived-catalog}}); `machine.sqlite` holds one machine's journal, cursor cache and lease rows, and no pull or rebuild touches it ({{store.lay-out.machine-catalog}}). Both sit behind ports ({{store.lay-out.catalog-ports}}).

A bucket mirrors the tree. A push uploads changed files, commits the bucket manifest by compare-and-set, and re-merges a lost race within a bound ({{store.merge.retries}}). A pull checks every digest ({{store.pull.digest-mismatch}}), refuses a different store identity ({{store.pull.identity-conflict}}), and writes no pointer unless it converges ({{store.pull.unconverged}}). Leases keep writers on different machines apart, with a fence the storage itself checks on commit ({{store.lease.stale-fence}}). A replica is read-only ({{store.replicate.write-refused}}) and holds a snapshot whole or not at all ({{store.replicate.partial-parquet}}).

## Worked example

Take the keyed table `filings`, with `primary_key = ["document_id", "page"]` and `order_by = "revised_at"`.

- Node `ingest-a` resolves its node id once at start ({{store.lay-out.node-id-order}}), takes the pipeline's lease, and renews it ({{store.lease.renewal}}).
- A run lands parts under `data/runs/<run-id>/ingest-a/`, and the engine injects `_ingested_at` and `_run_id` ({{store.reserve.injected}}). One batch sends `page` as a float; it refuses before any Parquet, because a key never widens ({{store.reconcile.key-widening}}).
- The run commits under the lease fence. A second machine that paused, lost its lease and then woke up tries to commit; the higher fence beats it, and its run stays unreadable ({{store.lease.stale-fence}}).
- A query now gets the current snapshot plus the committed runs it omits, deduped per key ({{store.bound-time.unbounded-latest}}).
- A fold trigger fires ({{store.fold.triggers}}). The pass holds the compaction lease and stamps its fence into the manifest and the pointer ({{store.fold.compaction-lease}}); the new snapshot's `includes_runs` names the folded runs ({{store.fold.includes-runs}}). The previous snapshot stays reachable to bounded reads ({{store.fold.supersedes}}) until retention collects it ({{store.fold.retention}}).
- An auditor reads `filings` as of last week. The read resolves the newest snapshot at or before that instant plus the runs it omits ({{store.bound-time.as-of}}), so the answer is identical before and after the fold. An instant older than retained history refuses and names the oldest answerable instant ({{store.bound-time.as-of-unretained}}).
- The node pushes, and a laptop replica refreshes. A query needing a sidecar the replica lacks refuses and names the refresh that supplies it ({{store.replicate.missing-index}}).

## Where to look

- File layout and visibility: `store.lay-out`.
- How keys, ordering and write modes shape a read: `store.declare`.
- How schemas merge: `store.reconcile`.
- When a snapshot publishes: `store.fold`.
- Time-bounded reads: `store.bound-time`.
- Bucket sync and single-writer exclusion: `store.endpoint`, `store.push`, `store.pull`, `store.merge`, `store.lease`.
