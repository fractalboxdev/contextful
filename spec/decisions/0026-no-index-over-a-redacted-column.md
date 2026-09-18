# 0026 — No index exists on disk over a column redacted at write time

**Status:** accepted 2026-09-18
**Decides:** `store.encrypt.refusal.index-over-a-redacted-column`

## Context

Write-time redaction removes a column's content before it becomes a row. What lands is a
placeholder, and the original value exists nowhere under the store root. This is a
stronger position than masking at read time, where the value is present and a policy
decides who sees it, and it is chosen for the columns whose exposure would be
unrecoverable.

Sidecar indexes are built from column content. A full-text segment holds the terms of the
column it indexes. A vector graph holds neighbour structure over embeddings, which
recovers approximate similarity between rows without holding any row. A bloom filter
answers membership for a value a holder can guess. A zone map holds per-range minimum and
maximum values verbatim. Each of these is a separate artifact from the Parquet, written
beside it and indexed by the same manifest.

The threat this file's encryption clauses are written against is a stolen bucket
credential: someone holding read access to every object under the prefix and no key. At
rest encryption answers that threat for the Parquet and for the sidecars, because both are
encrypted under the same key. It answers it only as long as the key holds.

A redacted column is a different promise. Its content is supposed to be absent, not
protected. An index built over it before redaction, or built over the placeholder in a way
that still carries the original term structure, reintroduces the content as a second
artifact with a second protection story. The question is whether that second artifact is
permitted to exist at all.

## Decision

The manifest validator raises `StoreIndexOverRedactedColumn` for an index declared over a
column redacted at write time. No structure over that column exists on disk in any form —
not a segment, not a graph, not a filter, not a zone map. The refusal is at declaration
time, in the validator, so the index is never built rather than built and removed. A
redacted column is reachable by exact scan over the placeholder and by nothing faster.

## Options considered

| Option | What it buys | What it costs |
| --- | --- | --- |
| **Refuse the declaration; no index over the column exists** *(chosen)* | The at-rest claim is complete without qualification: the content is absent, and absence needs no key to hold. One rule, checked in one place, before anything is written. | A redacted column is unsearchable through any accelerated path. A query over it takes the exact scan or takes nothing. |
| Encrypt the index and permit it | Redacted columns stay searchable, and the index is protected by the same mechanism protecting the Parquet. | Lost on completeness of the at-rest claim. The guarantee then rests on key custody for content that redaction was chosen precisely to remove. A stolen key recovers structure the store promised did not exist. |
| Build the index locally and strip it at the sync edge | Local queries stay fast, and nothing sensitive reaches the bucket. | Lost on number of places the confinement can fail. The local tree is itself a copy target — a backup, a replica seed, a developer machine — so the stripping has as many failure points as there are edges, and every new edge is a new one. |
| Permit the index over the placeholder alone, with a check that no original term survives | Zone maps and filters over a constant placeholder are harmless and cheap. | Lost on verifiability. Proving that a built structure carries no residue of the pre-redaction values is a property of the builder rather than of the artifact, and a reader holding the file cannot check it. |

## Criteria

1. **Completeness of the at-rest claim** — whether the guarantee holds against a reader
   holding every object and, separately, against a reader holding the key. *This criterion
   decided it.* Redaction and encryption answer different threats, and permitting an
   encrypted index over a redacted column collapses them into one: the column's protection
   would be exactly as strong as the key, which is the protection redaction was chosen to
   improve on.
2. **Number of places the property can fail** — call sites, edges and processes that have
   to behave correctly for the content to stay absent. A validator refusal is one place; a
   stripping rule is one per edge.
3. **Verifiability from the artifact alone** — whether someone holding the tree can
   confirm the property without trusting the process that wrote it.
4. **Query cost over the column** — how expensive the remaining access path is. This one
   was outranked: the columns redaction is chosen for are rarely the columns a query
   filters on, and a scan is slow rather than wrong.

## Consequences

The at-rest scope is statable in one sentence with no qualifier about key custody: a
stolen bucket credential reveals encrypted Parquet, encrypted sidecars, and no
write-time-redacted content through index structure. That sentence survives a key
compromise, which is the point of it.

The cost is real and lands on query planning. A predicate over a redacted column has no
zone map to prune with, no filter to skip files by, and no segment to rank with, so it
degrades to a full scan over the placeholder — which is usually an empty result arrived at
slowly. A caller who wanted similarity over a redacted column has no path at all and has
to choose masking at read time instead, accepting the weaker promise deliberately.

Reversing this is expensive once stores exist. Permitting the index later means every
store written under the refusal has no such index, so the capability arrives per table and
per snapshot rather than at once, and the at-rest sentence acquires a version qualifier
that a reader has to resolve against the snapshot they hold.

## Revisit triggers

- An index structure exists whose contents are provably independent of the values indexed
  — checkable by a reader holding the file rather than asserted by the builder.
- Read-time masking acquires a guarantee strong enough that redaction stops being the
  instrument chosen for the columns in question, which removes the conflict entirely.
- Measured query cost over redacted columns on real stores shows the scan path is reached
  often enough to be a workload rather than an edge case.
