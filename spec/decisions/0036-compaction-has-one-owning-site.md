# 0036 — Exactly one machine owns compaction for a store

**Status:** accepted 2026-09-18
**Decides:** `sync.lease.refusal.second-compaction-owner`

## Context

Everything on the data plane is append-only. Run files are written once under a run
directory carrying the writing node, request-ledger files carry the node in the filename,
and the index unions them across writers because their identifiers are unique. A duplicate
of any of these costs a re-landed row that the next fold removes. This is what makes a
bucket safe to write from more than one machine at all.

Compaction is the exception. A fold reads a table's committed runs, materializes
last-write-wins per key into one immutable snapshot, publishes that snapshot with its
sidecars in one step, and then drops the run files the snapshot subsumes. The dropping is
the part that is not append-only, and it is not incidental — retaining every run file
forever defeats the purpose of folding.

Two folds racing over one table each read a set of runs, each write a snapshot, and each
delete the runs they folded. The two sets are not identical, because the second fold began
later and saw runs the first did not. Whichever snapshot ends up published omits the runs
the other fold consumed, and those run files are already deleted. The rows in them are in
neither snapshot and in no file. Nothing errors: both folds succeeded, both published, and
the index merges cleanly.

The lease that serializes a pipeline is the obvious instrument and the wrong-sized one. It
is taken per pipeline, for the duration of a run, to protect a cursor advance. A fold
spans a table's whole snapshot tier across every pipeline that ever wrote to it, and it
runs on its own trigger rather than inside a run.

## Decision

Compaction is the operation that is not append-only, and exactly one machine owns
compaction for a store. A compaction pass started on a machine the store does not name as
its compaction owner raises `SyncCompactionNotOwned`. The owner is named in deployment
configuration, and the merge's rule that snapshot entries resolve to this writer's follows
from it — one writer owning a table's snapshot tier as a unit, a snapshot being a
recomputable fold over run files the union already carries.

## Options considered

| Option | What it buys | What it costs |
| --- | --- | --- |
| **Name one compaction owner per store and refuse a pass elsewhere** *(chosen)* | The one operation that can lose committed rows has exactly one site, and the merge's snapshot rule becomes a consequence rather than a coordination problem. | The owner is deployment configuration an operator sets and a reader has to know, and a store whose owner is offline accumulates run files until it returns. |
| Serialize compaction through the same bucket lease a pull takes | Reuses a mechanism already present, with no new configuration and automatic failover to whichever machine holds it. | Lost on scope. The lease is per pipeline and per run; a fold spans a table's whole snapshot tier across every pipeline that wrote it, so holding a pipeline lease excludes nothing that matters. |
| Let any machine compact under compare-and-set on the snapshot pointer | No configuration, no single point of stall, and the pointer swap is already indivisible. | Lost on row loss. The compare-and-set orders the publication, not the deletion: by the time a fold loses the pointer race it has already deleted the run files the winner's snapshot omits. |
| Compact on every machine and reconcile the outputs | No owner, no stall, and every machine's snapshot tier is complete on its own. | Lost on cost. The fold is the most expensive pass in the system, and paying for it once per machine to produce copies that then have to be reconciled buys nothing the single owner does not. |
| Retain run files after a fold and collect them later | A racing fold deletes nothing, so no rows are lost even under concurrency. | Lost on deferral rather than on principle: collection becomes the non-append-only operation and needs the same single site, at the price of a second mechanism and a larger tree in the meantime. |

## Criteria

1. **Which operation can lose committed rows** — whether the failure mode is a duplicate
   or an absence. *This criterion decided it.* Every other pass in the system survives
   being done twice, so concurrency there is a performance question; compaction is the
   single pass where doing it twice produces rows that exist in no file, with no error from
   any component. Availability and configuration-free operation are genuine advantages of
   the rejected options and neither is worth a silent absence.
2. **Scope match between the exclusion and the operation** — whether the mechanism's unit
   is the unit being protected.
3. **Ordering of deletion against publication** — whether the losing pass can have already
   destroyed what the winner omits.
4. **Cost of the pass** — how expensive the work being duplicated is.
5. **Availability** — whether the system keeps folding when one machine is down. This one
   was outranked: accumulating run files degrades read performance and disk use, both
   recoverable when the owner returns.

## Consequences

The merge's snapshot rule needs no conflict handling: a snapshot entry resolves to this
writer's, because only one writer produces them for a table and a snapshot is in any case
a recomputable fold over run files the union already carries. Retention and snapshot
collection have one site, so their effects reach a consumer through one writer's ownership
arm.

The accepted cost is an operator-visible piece of deployment state. Someone has to know
which machine is the owner, and a reader debugging why a store is not folding has to know
to look for it. A store whose owner is offline keeps ingesting and keeps answering — runs
are queryable the moment they land — but accumulates run files, so query cost and disk use
grow until the owner returns. There is no automatic failover, and adding one is not a
configuration change: it is a distributed agreement on which machine may delete, which is
the thing this decision avoids needing.

## Revisit triggers

- Ownership at store granularity is observed to be too coarse, so one machine's fold
  backlog for a large table starves every other table in the store.
- A fold is made to publish without deleting, with collection as a separately owned pass,
  which would let any machine fold and move the exclusion to a cheaper operation.
- Owner downtime is observed to cause read degradation often enough that an elected owner
  is worth the agreement it requires.
