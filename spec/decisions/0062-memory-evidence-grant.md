# 0062 — A synthesized claim is served only while every evidence row remains readable through the caller's own session

**Status:** accepted 2026-09-18
**Decides:** `memory.recall.invariant.evidence-gate`, `memory.recall.refusal.evidence-unresolved`, `memory.recall.limit.evidence-references`

## Context

A synthesized claim is a conclusion distilled out of rows. It carries
`evidence_row_ids` naming the rows it was distilled from, and it carries an inference zone
inherited from those rows. The zone check keeps a claim drawn from restricted evidence
inside that evidence's reach at the zone grain, by the same rule a table read applies.

Zone is not the whole reach. The rows underneath a claim are subject to row-level
enforcement, column masking, table-level grants and erasure, all of which move
independently of the zone the claim inherited at write time. A claim written when its
evidence was readable by a wide audience keeps that zone after the evidence is masked,
revoked, tombstoned or moved into a table the caller has no grant on. The claim text itself
is derived content: "the vendor's renewal price rose 40%" carries the restricted number
whether or not the reader can open the invoice row it came from.

So the question is whether a distilled conclusion can outlive the reach of the rows it was
distilled from. Recall runs inside a caller's enforced session, which is the one place the
caller's actual reach is known — the same session the claim mirror is read through. That
makes per-row resolution at recall time possible, and it makes it a per-row join whose cost
scales with how wide the evidence list is.

Evidence lists are not naturally bounded. A consolidation over a large batch can name every
row it read, and a claim naming tens of thousands of rows turns one recall into a join
nobody budgeted for.

## Decision

A synthesized claim is served when every one of its evidence rows resolves and reads
through the same enforced session the caller holds. An unreadable source row, a masked
source row, an unknown table, a reference into another memory row, and malformed lineage
each suppress the claim and raise `MemoryEvidenceUnresolved`. A claim naming more than 256
entries of evidence is suppressed rather than resolved, raising `MemoryEvidenceOverflow`.
Suppression removes the claim from the result; it does not fail the recall.

## Options considered

| Option | What it buys | What it costs |
| --- | --- | --- |
| **Resolve every evidence row in the caller's session at recall time; suppress on any failure; cap the list at 256** *(chosen)* | The conclusion never outlives the reach of its rows; revocation and masking take effect on the next recall; the join cost carries a stated ceiling | A claim whose source table was renamed or compacted goes silent with no explicit reader-facing signal; a claim distilled from very wide evidence is suppressed at the ceiling |
| Serve the claim with an unresolved-evidence marker | The reader learns the claim exists and why it is incomplete; nothing goes silently missing | Lost on containment: the conclusion itself carries the restricted content, so a marker discloses exactly what the gate is for |
| Resolve evidence once at write time and cache the verdict | Recall stays a single read; no per-row join on the serving path | Lost on revocation: a later grant change, mask or erasure never reaches the cached verdict, so the claim keeps serving after its evidence stops reading |
| An unbounded evidence list | No claim is ever suppressed for width alone | Lost on read cost: resolution is a per-row join, so one wide claim sets the latency of every recall that returns it |

## Criteria

1. **Containment** — whether restricted content can reach a caller who cannot read the rows
   carrying it.
2. **Revocation latency** — how long after a grant change, a mask or an erasure the claim
   stops serving.
3. **Read cost** — the bound on work one recall performs.
4. **Reader legibility** — whether a reader can tell the difference between a claim that
   does not exist and a claim that was suppressed.

Criterion 1 decided it, and it is what rejects the marker option outright: a marker is a
more legible failure, and legibility is worth having, but a marker that names the claim has
already disclosed the conclusion, which is the content the gate exists to hold. Criterion 2
then rejects the cached verdict among the containment-preserving options, because a cache
is correct exactly until the first revocation. Criterion 4 loses, and the record says so.

## Consequences

Revocation is immediate at the grain that matters: the next recall after a mask, a grant
change or an erasure stops serving the claims that drew on the affected rows, with no
re-synthesis pass and no invalidation sweep.

Recall pays a per-row join against evidence on every synthesized claim it would return.
The 256-entry ceiling bounds that per claim; total recall cost still scales with how many
claims survive earlier filtering, and that product is unmeasured.

A claim whose source table is renamed, dropped or compacted away goes silent. The reader
sees fewer claims and no error, and only an audit query against the base table shows the
claim is present and suppressed. This is the cost accepted, and it lands hardest on
ordinary schema maintenance rather than on any adversarial case.

A consolidation that wants to keep a wide claim servable states narrower evidence rather
than every row it read. That pushes a modelling obligation onto synthesis which did not
exist before, and a claim already written with a wide list cannot be narrowed without
writing a new claim.

## Revisit triggers

- Measured recall latency where the evidence join, rather than candidate filtering,
  dominates.
- A suppression-reason channel that can name that a claim was withheld without naming the
  claim, which would let the marker option back in under containment.
- Evidence lists clustering at the 256 ceiling in a live deployment, which means the bound
  is shaping synthesis rather than catching outliers.
