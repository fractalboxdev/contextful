# 0215 — A read reaching stored rows outside a registered relation is a defect, not a documented boundary

**Status:** accepted 2026-09-18
**Decides:** `enforcement.compose.refusal.unmediated-path`

## Context

Three layers stand between a stored row and a caller: removal at write time, the
redistribution bound at sync, and row-and-column restriction at query time. Calling that
stack a reference monitor is a claim with a fixed meaning — the monitor is always
invoked — and downstream contracts are written against the composition rather than
against the individual layers. The formal file publishes theorems about the composition's
negative space; the accountability file records what each mediated decision saw. Both
depend on there being no second way to reach a row.

The query-time layer works by rewriting. The compiler emits one registered relation per
granted table, bound to that table's bare name inside the session, carrying the mirrored
semi-join, the tenant equality, the row predicate, the table policy, the project default
and the column projection in a fixed order. Every surface returning rows reads the
registration by the bare name, which is why a retrieval arm added later inherits the whole
composition without naming any part of it.

That property is exactly what an unmediated path destroys. A code path that opens the
Parquet directly, or binds a table by its physical location rather than its bare name,
returns rows that skipped all six components of the relation. It does not fail a check; no
check was reached.

The paths that want to do this are the internal ones. Compaction reads whole files.
Manifest maintenance walks objects. A statistics job wants the unfiltered column. Each has
a plausible case for exemption, and each exemption is a sentence in a document that a
later contributor reads as permission to add the next one.

## Decision

A read arriving at stored rows outside a registered relation raises
`EnforceUnmediatedPath`. A path that reaches rows without traversing the layers is a
defect by definition and never a documented limitation, internal work included: every
maintenance and compaction path routes through a registered relation or is refused. The
reference-monitor name is carried with its always-invoked obligation attached, so the
composition property downstream contracts depend on is unconditional.

## Options considered

| Option | What it buys | What it costs |
| --- | --- | --- |
| **Every read routes through a registered relation; anything else is a defect** *(chosen)* | The composition property is unconditional, so a theorem, an audit record and a downstream contract can all rest on it without qualification | Every internal read path, maintenance and compaction included, needs a relation or is refused, which costs an operator-written path more setup than a direct file read |
| Document a small set of exempt internal paths | The awkward internal cases get a straightforward implementation, and the exemptions are at least written down | Loses on reliance: the enumeration is exactly what a later contributor extends without review, and every downstream guarantee becomes conditional on a list that grows |
| Drop the reference-monitor name and state the three layers individually | Accurate about what is actually enforced; no imported obligation to live up to | Loses on what downstream depends on: the composition, not the layers, is what the formal and accountability contracts are written against, and stating the layers separately supplies no property spanning them |
| Enforce mediation on caller-facing surfaces and exempt the engine's own reads | The common case is covered at much lower cost | Loses on the same reliance criterion, with a boundary — engine-internal versus caller-facing — that shifts every time a surface is added |

## Criteria

1. **What a reader can rely on from the name.** *(decided it)* The other criteria measure
   implementation cost and candor. This one determines whether the stack supplies
   anything an auditor can act on. A monitor with documented exceptions offers no property
   at all, because every guarantee becomes conditional on an enumeration whose future
   length is unknown, and a guarantee of unknown scope is not one.
2. **Whether downstream contracts have a composition to depend on.** The formal theorems
   and the audit record are both written against the whole, not the parts.
3. **Whether the boundary is stable.** Any exemption rule needs a line between exempt and
   non-exempt paths, and every candidate line moves as surfaces are added.
4. **Implementation cost of internal read paths.** Conceded, and the whole cost of this
   decision.

## Consequences

An auditor can state the property in one sentence with no qualifier, and a contributor
adding a retrieval surface inherits the whole composition by binding a table by its bare
name rather than by remembering six components. A path that skips mediation is a bug
report rather than a design discussion.

The cost accepted is that internal work pays the enforcement price. Compaction,
maintenance and statistics all route through a registered relation, which means they need
a session and grants wide enough to see what they must see, and a direct file read that
would have been three lines becomes a relation to compile and a grant to justify. That
cost is borne repeatedly, by the people least inclined to think of themselves as callers.

Reversing this is cheap to write and destroys the property permanently: once one exemption
exists, no statement about the composition can be made without enumerating exemptions, and
prior audits no longer mean what they said.

## Revisit triggers

- A maintenance operation appears whose correctness genuinely requires unfiltered rows and
  which cannot be expressed as a grant, so no relation can serve it.
- Relation compilation cost is measured to dominate a maintenance path that runs over
  whole files rather than over rows.
- The formal contract's theorems are restated in terms of the individual layers rather
  than their composition, removing what the unconditional property is carrying.
