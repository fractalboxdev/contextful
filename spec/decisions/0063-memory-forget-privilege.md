# 0063 — Forget is a privileged grant excluded from the default set, and its markers commit with the rows they retire

**Status:** accepted 2026-09-18
**Decides:** `memory.forget.refusal.privilege`, `memory.forget.invariant.marker-commit`

## Context

Forget removes a subject from memory and from everything distilled out of it. It writes a
tombstone carrying the selector, the stated reason and the acting subject tuple, and a
cascade one hop out over the synthesized rows whose evidence includes the forgotten
subject. The rows a tombstone covers leave the files at the next compaction; the tombstone
is what removes them from every read in between.

Two properties of the surrounding system shape how that operation is authorized and how it
commits.

The first is who writes to claim tables. Synthesis runs continuously under a write grant,
and a direct write from a console turn uses the same door. Those credentials live in
pipeline configuration and in agent sessions; they are routine, numerous and long-lived. An
erasure, by contrast, removes rows a human curated, and its cascade reaches rows the
caller never named. If forget rides the write grant, every routine synthesis credential in
the deployment can erase curated claims, and the blast radius of one leaked pipeline
credential is the memory table rather than a batch of candidates.

The second is replication. A store's rows reach a replica through a pull over committed
objects. Retiring rows locally does not retire them anywhere else; what reaches a replica
is the commit. A tombstone written to local disk and committed later leaves a window in
which a replica pulls the committed rows and not the marker that retires them, and the
replica then serves rows the origin has already erased — with no signal that anything is
missing, because from the replica's position nothing is.

## Decision

Forget is excluded from the default grant set. A caller holding a write grant without the
forget grant raises `MemoryForgetUngranted`. A tombstone and its cascade markers reach the
bucket in the same commit as the rows they retire, so a replica's next pull sees the
erasure rather than the rows.

## Options considered

| Option | What it buys | What it costs |
| --- | --- | --- |
| **A separate forget grant, with markers committed alongside the retirement** *(chosen)* | One leaked write credential cannot erase; no window in which a replica can pull rows without their markers | An operator holds a second credential for erasure, and a forget is a heavier commit than an ordinary write |
| Forget inside the write grant | One credential to provision and rotate; no second grant in the delegation profile | Lost on blast radius: a routine synthesis credential could erase curated claims, and the credential that writes the most is the one most exposed |
| A forget grant, with markers committed on the next ordinary commit | Cheapest forget — no commit of its own, amortized into the next write | Lost on durability of the marker: a replica pulling in between resurrects the forgotten rows, and nothing at the origin can tell that it did |
| A review queue rather than a grant — any writer requests, an operator approves | Human eyes on every erasure regardless of credential | Lost on latency and on the erasure obligation: an erasure with a deadline cannot wait on a queue nobody is watching |

## Criteria

1. **Blast radius of one credential** — what the most widely deployed credential can destroy.
2. **Durability of the marker relative to the rows** — whether a replica can observe rows
   after the origin has retired them.
3. **Operational weight** — credentials to provision and rotate, and commits performed.
4. **Erasure latency** — how long from the request to the rows being unreadable everywhere.

Two criteria decided two separate halves, and the record keeps them apart. Criterion 1
decided the grant split: the alternative is not that erasure becomes slightly riskier but
that every pipeline credential in the deployment is an erasure credential. Criterion 2
decided the commit rule, and it outranks criterion 3 there because the failure it prevents
is silent and unrecoverable — a resurrected row on a replica looks like an ordinary row,
and the origin has no way to observe that the resurrection happened.

## Consequences

An erasure is auditable to a credential that does nothing else, so the audit trail
distinguishes a routine write from a retirement without reading the operation type.

An operator provisions, stores and rotates a second credential, and any automation that
needs to erase — a scheduled retention sweep, a subject-erasure driver — needs that
credential wired to it, which is a wiring step that a single-grant design would not have.

A forget performs a commit of its own rather than riding the next one, so a bulk erasure
over many selectors is many commits. Batch erasure throughput is therefore bounded by
commit cost, and that bound is unmeasured.

Reversing the grant split is cheap in the authority model and expensive in the field: every
deployment that provisioned a second credential would keep it, and folding forget back into
the write grant widens the reach of credentials already issued.

## Revisit triggers

- A bulk erasure path whose commit-per-selector cost becomes the limit on meeting an
  erasure deadline.
- An authority model that can scope a single grant by selector, which would let one
  credential carry forget over a narrow subject set without carrying it over the table.
- A replication model in which a replica cannot pull between two local commits, which
  removes the resurrection window the commit rule exists to close.
