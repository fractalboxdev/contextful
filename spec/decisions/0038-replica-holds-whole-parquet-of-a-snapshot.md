# 0038 — A replica holds whole Parquet per snapshot and refuses a query needing an index it lacks

**Status:** accepted 2026-09-18
**Decides:** `sync.replicate.refusal.partial-parquet`, `sync.replicate.refusal.missing-sidecar`

## Context

A snapshot is a set of Parquet data files plus sidecar indexes derived from those files. A
replica refreshes by diffing the bucket index against local digests and downloading what
differs, then swapping the current-snapshot pointer in one step. The refresh selects objects
by the table patterns its pulling credential carries, so a replica routinely holds some
tables and not others.

Within one table the situation is different. Data files and sidecars fail differently. A
sidecar — a vector index, a full-text index, a partition index — is derivable from the data
files beside it, so a replica missing one is missing an accelerator and nothing else. A data
file is not derivable from anything the replica holds. A query running over a strict subset
of a snapshot's parts produces a result with rows missing, and the result carries no signal
distinguishing it from a complete one: the row count is plausible, the schema is right, and
the caller has no independent count to check it against.

The read face does not help here. It reports truncation against a published row ceiling and
reports match counts from the ranker, but neither number is computed over anything but the
files the relation was given. A relation built from a partial snapshot reports honestly about
a corpus that is silently smaller than the one the caller asked about.

That asymmetry — a missing sidecar costs a slower plan or a visible refusal, a missing data
file costs invisible rows — is what the two refusals here are built around.

## Decision

A replica holds every Parquet file of a snapshot or holds none of it. A replica holding a
strict subset raises `ReplicaPartialParquet` at refresh, and the snapshot stays unpublished
on that replica rather than becoming queryable in part. A replica legitimately holds a subset
of a snapshot's sidecars, and a query needing an index or a partition the replica does not
hold raises `ReplicaMissingIndex` naming the refresh that supplies it. Neither case answers
from what is present.

## Options considered

| Option | What it buys | What it costs |
| --- | --- | --- |
| **Whole Parquet per snapshot; refuse on a missing sidecar** *(chosen)* | Every answer a replica gives is complete over the snapshot it names. The two failure modes are both visible at the moment they occur. | A replica of a large table holds it entirely or not at all, and a caller handles a refusal rather than receiving a slower or narrower answer. |
| Allow a partition subset with a predicate check at query time | A consumer replicates the partitions it cares about and skips the rest. | The check has to know which partitions are absent, and a predicate dropped or rewritten anywhere in the plan turns a subset into what reads as the whole. |
| Degrade to a scan when a sidecar is missing | A ranked read always answers, at some latency. | A caller asking for a ranked read receives a differently shaped answer with no signal that the ranking came from elsewhere. |
| Fetch the missing object on demand mid-query | Nothing is unavailable; the first query pays for the fetch. | A replica's answers stop being offline answers, and the read path acquires a network dependency inside the query. |

## Criteria

1. **Whether an answer can be silently wrong** — whether a caller receiving rows can
   distinguish a complete result from an incomplete one. A partition subset fails this; a
   missing sidecar under a refusal does not.
2. **Legibility of a degraded mode** — whether a caller can tell that what it received is not
   what it asked for. Scanning in place of a missing index fails this.
3. **Offline guarantee** — whether a query on a replica reaches the network. On-demand fetch
   fails this.
4. **Storage cost on the consumer** — how much a replica holds to be useful. This is the
   criterion the chosen option loses on.

Criterion 1 decides it. A slower answer and a refusal are both states an operator can see
and act on. Rows absent from a result that reads as complete propagate into whatever the
consumer derives from it, and nothing downstream can detect them.

## Consequences

A replica's answers are exactly as trustworthy as the canonical store's for every snapshot it
publishes, which is what makes a replica usable as a read source at all. Refresh logic stays
simple: a snapshot directory is an all-or-nothing unit.

The accepted cost is storage. A consumer interested in one partition of a large table
replicates the whole table or reads it through the proxying face, and there is no middle
setting. A second cost is that the sidecar refusal names a refresh the caller may not be able
to run, so a read-only consumer on a stale replica is blocked until an operator acts.

Reversing the whole-Parquet rule is expensive: partition-level replication needs the replica
to carry an explicit record of which partitions it holds and needs every plan to consult it,
which is the query-time check this decision rejects.

## Revisit triggers

- A replica's storage requirement for a needed table exceeds the consumer machine's disk, so
  the table cannot be replicated at all.
- A per-replica holdings descriptor exists and is consulted during planning, making an
  absent-partition check something other than a predicate the plan could drop.
- Sidecar refusals become common enough in operation that callers are observed treating
  `ReplicaMissingIndex` as routine, which would mean the refusal is not carrying information.
