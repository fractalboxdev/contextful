# 0029 — A pull folds arriving run records into the catalog and never reconstructs it

**Status:** accepted 2026-09-18
**Decides:** `sync.pull.refusal.rebuild-inside-a-pull`

## Context

The catalog is a cache of one machine's own bookkeeping. It holds the run records that
drive the operator surface, and it holds two things that exist nowhere else on that
machine: the cursor position each connector has read to, and the lease of whatever run is
in flight. The file tree is the source of truth for data; the catalog is the source of
truth for where this machine is in its own work.

A reconstruction command exists and is the correct instrument when the cache has drifted
from the tree. It drops every cache table and rebuilds from the file tree by walking
committed run markers. That rebuild recovers run records exactly, because run records are
derivable from the tree. It does not recover cursor positions or lease state, because
neither is in the tree.

A pull brings a tree that another machine wrote. Run directories arriving from the bucket
are queryable the moment they land, since the read path discovers a committed run by
walking its marker file rather than by consulting the cache. The catalog still has to
learn about them, because the operator surface reads run records from the catalog and not
from the tree.

The tempting shape is to reuse the reconstruction: after a pull, rebuild, and the catalog
matches the tree by construction. Reaching for it inside a pull is what this record
refuses. A pull runs on every reconciliation tick where pull-before-run is on — which is
the default wherever the coordination mode is `cas` — so the reconstruction would run on
that same cadence, against a machine that is mid-run more often than not.

## Decision

After a pull the catalog is folded forward by inserting the walked run records with
conflicts ignored. A full cache reconstruction invoked as part of a pull raises
`SyncCatalogRebuildDuringPull`. Reconstruction stays an explicit command an operator runs
against a machine that is not mid-run, and a pull touches run records alone; the machine's
cursor positions and lease rows are never among the rows a pull writes.

## Options considered

| Option | What it buys | What it costs |
| --- | --- | --- |
| **Insert walked run records with conflicts ignored** *(chosen)* | Arriving runs reach the operator surface on the tick they land, and nothing this machine owns is touched. The fold is idempotent, so a repeated pull produces the same catalog. | Drift between the catalog and the tree from any other cause is not corrected by a pull. Correcting it takes the explicit reconstruction command. |
| Reconstruct the catalog after every pull | The catalog matches the tree exactly and by construction after every tick, with one code path for both cases. | Lost on destruction of unrecoverable state. Every cache table is dropped, so a per-tick rebuild resets every connector's read position and frees the lease of a run in flight. A connector resuming from a reset cursor re-reads from its source's beginning or loses records, depending on the cursor kind. |
| Pull the remote catalog and merge it row by row | Another machine's run records arrive as rows rather than by walking, which is cheaper than a tree walk. | Lost on meaning. The catalog holds one machine's own bookkeeping — its cursors, its leases, its journal — and those rows have no interpretation on a second machine. Merging them means deciding which foreign rows are meaningful, which is the same fold with extra steps. |
| Defer absorption until the next query touches the table | No work at pull time, and the cost lands only where it is needed. | Lost on visibility. Run records drive the operator surface, not only the read path, so a run that landed but was never queried would be invisible to the person watching the deployment. |

## Criteria

1. **What a reconstruction destroys** — whether the operation can lose state that exists
   nowhere else. *This criterion decided it.* Cursor positions and lease rows are not
   derivable from the tree, so dropping them is not a cache miss but data loss, and it
   would land on the cadence of the reconciler rather than at an operator's choosing. Code
   simplicity and exactness of the resulting catalog are both real advantages of the
   rebuild, and neither survives a mechanism that can silently reset a connector's read
   position mid-run.
2. **Idempotence** — whether running the pull twice produces the same catalog as running
   it once.
3. **Time to visibility** — how long after an object lands its run appears on the operator
   surface.
4. **Correction of drift** — whether the mechanism repairs a catalog that has diverged
   from the tree for an unrelated reason. The fold does not, which is the cost accepted
   below.

## Consequences

A pull is safe to run on every tick, including on a machine holding a lease and writing.
That is what makes pull-before-run viable as a default: reconciling against the bucket
ahead of landing rows costs a tree walk and some ignored inserts, and costs this machine's
position nothing.

The accepted cost is that the fold repairs nothing. It is an insert-with-conflicts-ignored
over walked records, so a catalog that has drifted from the tree — a row deleted, a record
written by an older build, a partial write — stays drifted until someone runs the explicit
reconstruction. The operator surface can therefore disagree with the tree, and the read
path will not notice, since it discovers runs by walking markers rather than by consulting
the cache.

Reversing this is cheap in code and would be expensive in incidents: the failure the
rebuild-inside-a-pull produces is a connector that silently restarts its read position,
which surfaces as duplicate or missing rows long after the pull that caused it.

## Revisit triggers

- Cursor positions and lease state move out of the catalog into artifacts derivable from
  the tree, which removes what the reconstruction destroys and makes the rebuild safe on
  any cadence.
- Catalog drift from causes other than a pull is observed often enough that a repair pass
  is needed on a schedule rather than by hand.
- The fold's tree walk becomes the dominant cost of a tick on stores with large run
  counts, which argues for an arriving-run index in the bucket rather than a walk.
