# 0230 — Widening a protected-class surface past its floor requires an explicit override flag

**Status:** accepted 2026-09-18
**Decides:** `enforcement.place.refusal.floor-widened-without-an-override`

## Context

A column or table classified as protected health information carries an implicit floor at
the fail-closed pair: a local device and an on-premises environment. The floor is applied
twice — a declared set wider than it resolves down to it when a read is served, and a
manifest declaring the wider set is examined at validation time. The runtime resolution is
what makes the floor a guarantee; the validation-time examination is what makes the
operator's intent legible.

Some deployments hold a vendor agreement that covers the class, and for them processing
those columns in a named cloud account is both lawful and the point of the deployment.
A specification that admits no widening at all does not stop those deployments — it moves
them outside the engine, into a copy of the table with the classification stripped, where
nothing records that the widening happened or who decided it.

The alternative failure is the quiet one. An allow-set on a protected column is one line in
a manifest among many, and a reviewer reading a diff of allow-sets has no signal
distinguishing the line that widens a classified surface from the dozen that widen ordinary
ones. The classification lives on the column; the widening lives in the allow-set; nothing
in the diff joins them.

## Decision

A manifest widening a protected-class surface past its floor raises
`EnforceProtectedFloorWidened` unless it carries an explicit override flag named for the
class. The flag travels in the manifest beside the surface it governs, so the decision
appears in the same diff as the widening it authorizes. The floor still resolves at serve
time, so a surface whose override flag is absent narrows to the floor regardless of what
its allow-set says.

## Options considered

| Option | What it buys | What it costs |
| --- | --- | --- |
| **Widening permitted, gated on a class-named override flag in the manifest** *(chosen)* | The decision is a distinct token in the diff a reviewer reads, attached to the surface it governs, and the deployment stays inside the engine where the audit record exists. | A flag is a thing that can be pasted. Nothing in the engine verifies the vendor agreement it stands for; it records that someone asserted one. |
| Refuse widening of a protected-class surface outright | The strongest possible statement, no flag to misuse, no override path to audit. | Loses on retention: deployments with a covering agreement exist, and an absolute refusal routes them around the engine entirely, where the choice is invisible rather than merely asserted. |
| Widen silently when the allow-set says so, relying on the classification for nothing | Simplest rule; one mechanism decides placement. | Loses on visibility: the widening is indistinguishable in review from widening an unclassified column, so the classification buys nothing at the moment it matters most. |
| Require an out-of-band configuration key enabling class overrides deployment-wide | One switch, set once by whoever holds that authority, not repeated per surface. | Loses on locality: the decision no longer travels with the table it governs, so a table moved or copied into another manifest carries a widened allow-set and none of the reasoning that justified it. |
| Require a second reviewer's approval recorded in the manifest | Ties the widening to a named party. | Loses on enforceability here — the engine reads a manifest, and an approval token in a file is a flag with more ceremony and the same verifiable content. |

## Criteria

1. **Whether the widening is deliberate and visible in review** — whether a reader of the
   manifest diff can tell that a classified surface was widened. *This criterion decides.*
   The floor's value is not that it cannot be lifted; it is that lifting it is an act
   someone performed on the record, and every option that loses here loses the whole
   protection whatever else it buys.
2. **Retention inside the engine** — whether a deployment with a legitimate need stays
   under the engine's enforcement and audit, or leaves it.
3. **Locality of the decision** — whether the authorization travels with the surface it
   authorizes.
4. **Strength of the assertion** — what the engine can actually verify behind the flag,
   which is nothing, and which no option here improves on.

## Consequences

A reviewer reading a manifest diff sees the class-named flag wherever a classified surface
is widened, and its absence is a refusal rather than a narrower result nobody notices at
validation time.

The runtime resolution stays in force, which produces the cost this decision accepts: a
widened surface that loses its override flag — through an edit, a merge, a partial copy
into another manifest — narrows back to the floor at the read without an error. The reads
that follow return nulls and withheld rows, and nothing in the response distinguishes that
from a surface that was never widened. Validation catches the inverse case, not this one.

The flag also asserts more than the engine can check. It stands for a vendor agreement the
engine never sees, so its presence is evidence about intent and not about lawfulness.

Reversing toward an outright ban is expensive once deployments depend on the override,
because the path they would take is the one outside the engine, which is the outcome this
record exists to avoid.

## Revisit triggers

- A surface is observed narrowing at serve time from a lost override flag, which would
  argue for making the flag's absence a startup refusal rather than a silent narrowing.
- The set of protected classes grows past the one class this floor names, so that one flag
  spelling no longer identifies which class a widening concerns.
- Override flags appear on surfaces whose deployments hold no covering agreement, which
  would move the deciding criterion from visibility toward verifiability.
