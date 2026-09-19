# D08 — A replica is a read-only whole-snapshot cache

**Status:** accepted

## Context

A replica serves reads offline, near the reader, on a machine that may not be trusted with every column. Accepting writes needs conflict resolution the store does not have. Answering from part of a snapshot returns results that are wrong without looking wrong.

## Decision

- `store.replicate` answers reads and holds no write path; a write verb refuses and names the canonical store.
- A replica holds every Parquet file of a snapshot or none; a snapshot held in part stays unpublished there. A replica may hold a subset of sidecars, and a query needing an absent index or partition refuses, naming the refresh that supplies it. A replica never answers from what is present and never fetches mid-query.
- A table declaring sensitive columns replicates off by default, and a refresh requesting it refuses. A consumer reads those columns through the proxying face, which verifies the credential, returns projected rows and records each read.
- `store.pull` writes a table's pointer after every object it reaches has landed, then folds arriving run and snapshot records into `derived.sqlite`, a cache rebuildable from the tree. Cursor and lease rows live in `machine.sqlite`, which no pull or rebuild touches.

## Options

| Option | Lost on | Cost |
| --- | --- | --- |
| Read-only, whole-Parquet, sensitive tables off *(chosen)* | — | A replica stores full snapshots; a sensitive read needs a round trip to the proxying face. |
| Accept local writes and push them on refresh | Merge soundness | Divergent writes need conflict resolution the store lacks. |
| Hold a partition subset and check predicates at query time | Silent wrongness | The check must know which partitions a predicate touches, and a miss returns a short answer. |
| Replicate sensitive tables and mask at the consumer | Failure direction | A misconfiguration has already moved the bytes onto the machine. |
| One catalog file holding synced and machine-local rows | Destruction of local state | A rebuild or pull would erase cursor and lease rows that exist nowhere else. |

## Consequences

- A replica's answer is complete for the snapshot it names, or a refusal.
- A caller reaching the wrong machine learns which one to reach.
- A replica's catalog is a cache; the canonical store is the source of truth for every table it serves.
