# 0022 — A snapshot and every declared sidecar become visible in one step

**Status:** accepted 2026-09-18
**Decides:** `store.fold.refusal.partial-snapshot`

## Context

A fold produces two kinds of artifact for one table: the snapshot's Parquet, and the
sidecars declared over it — a sorted primary-key map, a vector graph, a full-text index,
bloom filters. The data is the canonical corpus; the sidecars sit outside SQL and yield
candidate identifiers that re-join through the enforced relation.

Publishing the two separately opens a window. Between a snapshot becoming readable and its
sidecars finishing, a retrieval finds Parquet with no matching index. The optimistic
outcome is slow — the query degrades to a scan. The pessimistic outcome is wrong: a
retrieval that draws candidates from the previous snapshot's index re-joins them against
the new snapshot's rows, and identifiers that the fold dropped as superseded return nothing
while identifiers it added are never proposed. The result is a plausible, incomplete
answer, with nothing in it marking the state it was computed in.

The window cannot be closed by ordering alone. Building indexes first and data second moves
the same inconsistency to the other side. Closing it requires that no moment exists in
which one is current and the other is not.

Placement decides whether that is achievable cheaply. A sidecar that lives inside the
snapshot directory it indexes is staged and published by the same rename as the data, so a
single filesystem operation moves both. A sidecar held in a shared tier outside the
snapshot directory has a visibility moment of its own, and an index kept in a separate
database is not a file beside the Parquet at all — it does not replicate by manifest diff,
so a replica that pulls the tree gets data with no index.

## Decision

A pass stages Parquet and every declared sidecar under a staging directory mirroring the
published shape, then publishes both with one rename, so a snapshot becomes visible once
its data and its indexes are durable. Publishing data whose declared sidecars are absent,
or sidecars whose data is absent, raises `StorePartialSnapshot`; no half-indexed snapshot
is readable at any moment. A statement running while a pass builds continues against the
prior snapshot and picks up the new one after publication.

## Options considered

| Option | What it buys | What it costs |
| --- | --- | --- |
| **Stage both, publish with one rename** *(chosen)* | No moment exists in which a snapshot is current and its indexes are not | A crash mid-pass leaves a staging directory to collect, the commit cannot be incremental, and one slow sidecar delays the whole fold |
| Publish the snapshot and build indexes behind it | Data becomes readable as early as possible; index cost is amortized | Lost on the inconsistency window: retrieval finds Parquet with no matching index, and candidates drawn from the prior index re-join the new rows into a plausible incomplete answer |
| Hold sidecars in a shared tier outside the snapshot directory | Sidecars are reusable across snapshots and cheaper to rebuild | Lost on the inconsistency window: the index acquires a visibility moment of its own and forfeits the single rename that removes it |
| Keep indexes in a separate database | Index updates are transactional and queryable on their own | Lost on replication: an index that is not a file beside the Parquet does not replicate by manifest diff, so a replica pulls data with no index and the window reappears per replica |

## Criteria

1. **The window between a snapshot being readable and its indexes being usable** — whether
   such a moment exists at all. *(the one that decided it)* Inside that window a retrieval
   returns a plausible incomplete answer that carries no marker of the state it was
   computed in, so no consumer can detect or correct it. Time-to-first-read and incremental
   commit were the competing criteria and lost, because they buy latency, and latency is
   observable and tunable where an undetectable wrong answer is neither.
2. **Replication by manifest diff** — whether a replica that pulls the tree gets a usable
   index.
3. **Crash recoverability** — what an interrupted pass leaves behind.
4. **Time to first read** — how soon new rows are visible after a pass starts.

## Consequences

Retrieval never sees a half-indexed table, on the writing site or on a replica, and a
snapshot's sidecars die with it — collecting a snapshot collects its indexes in the same
step, so no index points at Parquet that is gone. A reader is never blocked: it continues
against the prior snapshot until publication.

The cost accepted is coarse-grained, all-or-nothing publication. A crash mid-pass leaves a
staging directory for the catalog to collect on its next sweep; the commit cannot be
incremental, so a large table republishes wholly; and one slow sidecar — a vector build
over many rows is the usual one — delays the visibility of data that was ready much
earlier.

On object storage the atomicity is borrowed rather than native. There is no atomic rename,
so publication is a content-hash-keyed copy followed by a delete and the catalog row
decides visibility. A reader that lists directories instead of consulting the catalog can
therefore observe uncommitted state — the guarantee holds for readers that go through the
catalog and not for those that do not.

## Revisit triggers

- A sidecar kind appears whose build time is a large multiple of the data write, making
  whole-snapshot delay the dominant cost of a pass.
- A backend offers a native atomic multi-object commit, removing the borrowed atomicity and
  with it the directory-listing exposure.
- Incremental snapshots are needed for table sizes where republishing the whole table per
  pass stops being affordable.
