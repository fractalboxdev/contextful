# 0271 — A table is governed per person or as a cohort, and a cohort read neither widens an under-floor group nor returns an individual row without a grant

**Status:** accepted 2026-09-18
**Decides:** `disclosure.bound-cohort.refusal.dual-governance`, `disclosure.bound-cohort.refusal.cohort-widening`, `disclosure.bound-cohort.refusal.per-individual-row`, `disclosure.bound-cohort.refusal.singleton-cohort`

## Context

A deployment declares, per table, whether its rows are governed as individuals or as cohorts.
The two regimes impose different obligations on the same bytes. Per-person governance carries
the obligations at the grain of the source: a row is about someone, and reaching it is a
decision about that person. Cohort governance carries a group-size floor: rows are readable
only aggregated into groups of many contributors, and an under-floor group leaves the result
set entirely.

Those obligations contradict each other on any given read. A per-person read of a row that
also belongs to an under-floor cohort satisfies one regime and breaches the other. There is no
predicate that expresses both correctly at once, because the correct answer differs — return
the row, drop the group — and no combination of the two is a coherent third answer. In a
system that applied both, the contradiction would not surface as an error; it would resolve in
favour of whichever check ran last, which makes the governance of a table a property of
evaluation order rather than of the declaration.

The cohort floor also has two specific ways of being recovered by a caller who accepts it in
principle. The first is widening: a group of three under a floor of ten reappears if the read
merges it into a coarser key and returns the merged figure. The merged figure is attributable
back to the same contributors whenever the coarser key has only that one under-floor group
inside it, so widening returns what suppression withheld, one step removed. The second is the
individual row itself: if any reader of a cohort-governed deployment can ask for a row, the
floor guards nothing, since the aggregate path is optional.

The declaration has a third failure that happens before any row exists. A cohort key names an
attribute shared by many contributors — a region, an industry, a tenure band. A key whose grain
resolves to one person is a per-person table wearing a cohort label, and every read of it
clears a floor of any value trivially.

## Decision

A table's policy block declares `governance = "per-person"` or `governance = "cohort"`, and a
table declared under both regimes raises `DisclosureCohortDualGovernance`. A table declaring
neither resolves to cohort governance. On a cohort table the floor guard runs after candidate
generation and before the top-K cut, and an under-floor group collapses into a sentinel with
its rows leaving the result set. A read that would recover an under-floor group by merging it
into a coarser key raises `DisclosureCohortWidening`. A per-individual row from a
cohort-governed deployment requires an explicit grant on the reader's token, and a read
without one raises `DisclosurePerIndividualUngranted`. A declared cohort key whose grain
resolves to one person raises `DisclosureSingletonCohort` at declaration, ahead of any row
landing under it.

## Options considered

| Option | What it buys | What it costs |
| --- | --- | --- |
| **One regime per table, widening refused, individual rows behind a grant, singleton keys refused at declaration** *(chosen)* | Each read has one correct answer fixed by the table's declaration, and the two recovery paths around the floor are closed rather than discouraged. | A deployment needing both views of one source carries two tables and two declarations, and a key whose grain narrows over time reds the declaration rather than degrading quietly. |
| One table under both regimes with a per-query selector | No duplication; a caller picks the view it needs from the same source. | Lost on expressibility: a per-person read and a cohort floor impose contradictory obligations on the same rows, and the contradiction resolves silently in favour of whichever check runs last. |
| Widening an under-floor group into a coarser key | Fewer sentinels; a caller gets a figure instead of a hole. | Lost on attributability: the widened figure is attributable back to the same contributors, so it publishes exactly what the floor withheld at a coarser label. |
| Returning per-individual rows to any reader of a cohort deployment | The cohort floor governs aggregates without making individual lookups a separate capability. | Lost on whether the floor guards anything: the aggregate path becomes optional, and every suppressed group is reachable one row at a time. |
| Allowing a cohort key whose grain is one person | Operator freedom in choosing keys; no grain analysis at declaration. | Lost on the same criterion: it re-creates a per-person table under a cohort label, and every group clears any floor. |
| Detecting a singleton key at read time rather than at declaration | Catches a key whose grain narrows after the fact, with live data as evidence. | Lost on timing: rows land under the key first, so the discovery happens after the disclosure it would have prevented. It composes with the declaration check rather than replacing it. |

## Criteria

1. **Whether one predicate expresses both regimes correctly at once.** **This criterion
   decided.** It is not a matter of which regime is stronger — it is that no single evaluation
   satisfies both, so a dual-governance table has no correct behavior to implement. Everything
   else on this list is a choice among workable options; this one eliminates its alternative
   outright.
2. **Attributability of a returned figure** — whether a published value traces back to the
   contributors a suppression withheld.
3. **Whether the floor guards anything** — whether a second path reaches the same rows without
   passing it.
4. **Timing of a declaration fault** — whether it lands before or after rows exist under the
   bad key.
5. **Authoring cost** — how many declarations a deployment maintains. The chosen option is the
   most expensive here.

## Consequences

Governance becomes readable off the table's declaration, so an auditor answers "how is this
table protected" without tracing a query path. The floor's placement — after candidate
generation and before the top-K cut — means an under-floor group is dropped ahead of being
counted, returned, or scored into a visible aggregate, so the cut does not become a second way
of observing it. The default to cohort governance means an undeclared table carries the floor
rather than none.

The cost accepted: a deployment needing both views of the same source carries two tables and
two declarations, with the duplication and the drift risk that implies. There is no single
source with two lenses, and keeping the two in step is the operator's work. A second cost lands
on key stability: a cohort key whose grain narrows over time — a region that empties, an
industry band that ends up with one member — reds the declaration rather than degrading
quietly, which is an outage for a table that was previously fine. Refusing widening also costs
real answers: some coarser merges are genuinely safe, and the rule refuses them all rather than
attempting to distinguish the attributable ones.

Merging the two regimes later would require deciding the contradiction, which is the thing
this decision holds is undecidable.

## Revisit triggers

- A predicate is proposed that satisfies both regimes on the same read without an ordering
  dependency, which would remove the criterion that decided this.
- Two-table duplication produces drift between the per-person and cohort declarations of one
  source often enough to be its own failure mode.
- A widening class is characterized whose result is provably not attributable to an
  under-floor group's contributors.
