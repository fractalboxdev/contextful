# 0037 — A replica answers reads and routes every write to the canonical store

**Status:** accepted 2026-09-18
**Decides:** `sync.replicate.refusal.write-through-a-replica`

## Context

A replica is a machine that has pulled a deployment's bucket and answers queries from the
Parquet it holds. Its catalog — the run bookkeeping, the snapshot pointers, the cursor
positions — is reconstructed from the file tree it received, not authored on that machine.
Everything in it is a materialized copy of a decision some other machine committed.

The data plane underneath is append-only. A run lands part files under its own run and node
segments, and a snapshot is a recomputable fold over those files. Two writers meet only at
the bucket index, where the merge resolves each key class by an ownership arm keyed on the
writing node id. Those arms assume one writer per tier: snapshot entries resolve to the
local writer, catalog entries resolve at the key, run files union on identifiers that are
unique per node. None of them describes what happens when two machines claim the same tier
with different contents, and nothing in the wire format carries the information that would
let them.

A replica accepting a local write creates exactly that situation. The write lands in a tier
whose ownership arm names a different node, and the next refresh either discards it silently
or propagates an index the canonical writer did not produce. Both outcomes are worse than a
refusal, and neither is visible to the caller that issued the write.

Consumers still need a write path. The question is whether it runs through the replica or
around it.

## Decision

A replica answers reads and holds no write path. A write verb invoked against a replica
raises `ReplicaWriteRefused` and names the canonical store the deployment configuration
points at, so a caller that reached the wrong machine learns which machine to reach instead.
A replica's catalog is a materialized cache; the canonical store is the one source of truth
for every table the replica serves.

## Options considered

| Option | What it buys | What it costs |
| --- | --- | --- |
| **A replica refuses writes and names the canonical store** *(chosen)* | One writer per tier, so the merge's ownership arms stay sound. The refusal is immediate and carries the address of the machine that can serve the write. | An offline consumer records nothing of its own. Every write is a network round trip to the canonical store. |
| Accept local writes and push them back on the next refresh | A consumer writes while disconnected and reconciles later. | Lost on soundness of the merge: reconciling divergent writes needs a conflict resolution rule the append-only data plane does not supply, so the merge faces two contents for one tier with nothing to choose between them. |
| A write-through replica that forwards each write synchronously | A caller uses one address for reads and writes. | Lost on legibility of the surface: the bytes never land locally, so "replica" names something that is a proxy in the write direction and a cache in the read direction. |

## Criteria

1. **Soundness of the merge** — whether a second writer produces an index state the per-class
   ownership arms can resolve. Accepting local writes fails this outright: the merge would
   have to choose between two contents for one tier with no rule that says which wins.
2. **Legibility of the surface** — whether a reader of the deployment configuration can say
   what a replica holds and what it does. A write-through replica passes soundness and loses
   here, since the same object would be authoritative for reads and transparent for writes.
3. **Offline capability** — what a disconnected consumer can still do. This is the criterion
   the chosen option loses on, and it is the reason to revisit.

Soundness of the merge decides it. Offline writing is a feature; a merge that cannot resolve
two claims on one tier is data loss, and no amount of offline convenience outranks that.

## Consequences

Reads get simpler: a replica's catalog can be dropped and rebuilt from the file tree at any
time, since nothing in it originated there. Debugging gets simpler too — a row's origin is
one machine, named in its run path.

The accepted cost is that an offline consumer cannot record anything of its own. A consumer
that needs local annotations stands up a separate store for them rather than extending the
replica, and joining the two is the consumer's work, not the sync path's. Reversing this is
expensive: a local write path would require a conflict resolution model for every writer-owned
tier, and the ownership arms would have to carry provenance they do not carry.

## Revisit triggers

- A deployment reports consumers routinely disconnected for longer than one refresh interval
  and unable to proceed.
- The data plane acquires a conflict resolution rule for a writer-owned tier — for example a
  per-row last-writer rule with a comparable clock — that a second writer could commit under.
- The proxying face's round-trip latency makes a write-heavy consumer workload unviable, which
  is unmeasured for a consumer on a high-latency link.
