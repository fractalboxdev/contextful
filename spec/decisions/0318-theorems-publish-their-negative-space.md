# 0318 — Every theorem publishes the statement it leaves open, and the composition order is specified rather than commuted

**Status:** accepted 2026-09-18
**Decides:** `formal.prove.refusal.theorem-without-negative-space`, `formal.prove.refusal.commutation-claim`, `formal.prove.refusal.mediation-from-composition`

## Context

The composition theorem says that a row admitted by the conjunctive fold over a list of
layers was admitted by every member of that list. That is a soundness statement, and it is
narrower than its name suggests in three specific ways: it assumes the required members
were in the list at all, it treats a member as a test rather than as a removal, and it says
nothing about whether an execution reaches the fold before an effect happens. All three
gaps are exactly where an auditor's reliance sits.

A theorem travels. It is quoted in a report, in a document, in an auditor's packet, and by
then it is usually a constant name and a one-line gloss. A name is a compression, and
compressions round toward what the reader hopes for. A constant carrying the word soundness
reads as a guarantee about the layers being present unless the exclusion travels with it.

Order is the second question the composition theorem attracts. The fold ranges over the
single order the engine applies. It is tempting to strengthen this into order-independence,
which would make the composition result robust against a reordering anywhere downstream.
It is also false as stated: filtering on a value and then masking that value yields a
different relation from masking first and then filtering, since the masked value is no
longer available to filter on. A commutativity claim would need preconditions the stages do
not carry.

The third pressure comes from what the theorem is asked to stand in for. What a reader
ultimately wants is mediation: that no reachable state reaches stored rows outside the
fold. That statement quantifies over an execution relation and proceeds by induction over
reachable transitions. The composition theorem quantifies over a list and a row identifier.
The two are different statements about different objects, and the gap between them is
precisely the third exclusion above.

## Decision

Every theorem is published together with the statement it leaves open; one published
without it raises `TheoremWithoutNegativeSpace`, naming the constant. The composition
theorem ranges over the one order the engine applies and establishes that order's security
and result semantics, and a claim that the stages commute raises `CommutationClaimed`. A
filter-composition theorem offered as a mediation theorem raises
`MediationClaimedFromComposition`. The negative space rides in the theorem's own inventory
row, so it is carried by the same artifact a reader cites.

## Options considered

| Option | What it buys | What it costs |
| --- | --- | --- |
| **Publish each theorem with its exclusions; specify the order; refuse the stand-in** *(chosen)* | The compression a reader ends up quoting carries its own limits, and the three ways the statement is narrower than its name travel with it. | Each theorem carries a paragraph of what it excludes, and the published guarantee reads narrower than a reader would prefer. |
| Publishing theorems without their exclusions | Shorter artifacts, and a claim that sounds as strong as the work behind it. | Lost on reader reliance: the gap between a constant's name and its statement is where an auditor relies unprotected, and a quotation that drops nothing cannot drop a limit it was never given. |
| Proving order-independence of row filtering, column masking and write-time removal | Robustness against a reordering downstream, and one less thing the engine has to hold fixed. | Lost on truth. Masking destroys the value a filter would read, so the relations differ; the claim would need preconditions the stages do not carry. |
| Asserting mediation from layer composition | The statement a reader actually wants, from proofs that already exist. | Lost on scope: the two statements quantify over different objects, and no amount of composition supplies an execution relation or an induction over reachable transitions. |
| Recording exclusions in prose beside the proofs rather than in the inventory row | Freedom of expression, and no schema to maintain. | Lost on travel: the artifact a later reader cites is the per-constant report generated from the inventory, so an exclusion living elsewhere is exactly the one that gets dropped in quotation. |

## Criteria

1. **What a later reader can act on** — whether the artifact they hold states its own
   limits. *This criterion decided it.* A theorem's value is entirely in downstream
   reliance, and reliance calibrated by a constant's name rather than its statement is
   worse than no theorem, because it displaces the caution an unproved area would have
   attracted.
2. **Truth of the claim** — whether the statement is provable at all. This is what settled
   the order question; no criterion outranks it, and it did not compete here because the
   commutation option fails on it outright.
3. **Object identity between the statement offered and the statement wanted** — whether two
   theorems quantify over the same thing.
4. **Brevity of the published artifact** — how much a report costs to read. Outranked by
   the first criterion.

## Consequences

An auditor reading a theorem reads its limits in the same breath, and a quotation that
drops the exclusion is visibly a quotation of a different statement. The engine's
composition order becomes a fixed part of the specification rather than an implementation
detail, since the theorem's meaning depends on it. Mediation stays an open statement,
named as open rather than implied by something adjacent.

The accepted cost is that the published guarantee reads narrower than a buyer or an auditor
would prefer, and every theorem carries a paragraph nobody enjoys writing. Constants are
also named for the object they range over rather than the property a reader hopes for,
which makes the inventory read more cautiously than the work behind it.

## Revisit triggers

- A mediation theorem is proved, which would let the composition theorem's third exclusion
  be replaced by a citation rather than a gap.
- Masking is made to preserve the filtered value under a declared precondition, which would
  make a scoped commutativity statement true rather than false.
- The negative-space text is observed to be dropped in practice when theorems are quoted,
  which would argue for carrying it inside the constant's own statement rather than beside
  it.
