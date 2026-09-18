# 0242 — A resource whose access list could not be mirrored reaches no subject

**Status:** accepted 2026-09-18
**Decides:** `visibility.sweep.refusal.orphan-grant`

## Context

A permission sweep reads one source and writes what it found: resource rows describing
governed objects, grant rows describing who reaches them, principals, group edges and
observed revocations. Coverage is never total. A source returns a permission list for
some objects and an error for others; a credential reaches a workspace's open rooms and
not its private ones; an object type is enumerable while its access list sits behind an
endpoint the sweep is not entitled to call. Each resource therefore carries a recorded
observed class — `mirrored`, `coarse`, `federated` or `unknown` — describing what the run
actually managed to learn about it.

`unknown` is the class for a resource whose access list failed to mirror. It is not a
statement that the resource is private, and not a statement that it is public. It is the
absence of a statement, recorded where a statement was expected.

Grant rows and resource rows arrive from the same run but not necessarily together. A
grant can land naming a resource whose own row never arrived, or naming one that arrived
carrying `unknown`. Reading such a grant as authorization means treating a fragment of a
permission model as the whole of it: the grant says a principal has some level on some
object, while the thing the mirror does not know is whether that object's audience is the
one principal or the entire organization.

The two available defaults are not symmetric. Excluding the resource produces a denial:
someone entitled to read it finds it absent, asks why, and an operator sees a coverage
gap and fixes the sweep. Including it produces a disclosure: a resource whose audience
the mirror could not determine is served to whoever the fragmentary grant happens to
match, and nobody involved observes anything wrong.

## Decision

A resource carrying `visibility_class = 'unknown'` reaches no subject at all, an operator
holding a deployment-wide credential included, until manifest policy names it explicitly.
A grant row landing with no resource row, or whose resource carries the unknown class,
raises `VisibilityOrphanGrant` at commit and contributes to no reachable set. Explain
prints `NEVER OBSERVED` for a resource with no observation on record, so a coverage gap
stays distinguishable from a decision.

## Options considered

| Option | What it buys | What it costs |
| --- | --- | --- |
| **Unknown fails closed; an orphan grant is refused at commit** *(chosen)* | The one class of resource whose audience is unknown is the one class nobody receives, and the failure surfaces as a visible absence with a named cause. | Sources with partial coverage produce large invisible regions: content sits in the store, answers nothing, and a reader entitled to it is told nothing is there. A break-glass path is needed and is not free. |
| Index it and filter later, at the read face | Nothing is lost from the corpus; the coverage gap is a read-time concern that a face can decide about. | Loses on failure mode: the unmirrored resource is precisely the one whose audience is unknown, so a later filter has nothing to filter on and the default at each face becomes a fresh guess. |
| Inherit the containing object's class and grants | Plausible in the common case — an item usually shares its folder's audience — and reduces the invisible region sharply. | Loses on the same criterion: inheritance is a guess, and a guess that widens is a disclosure. Per-item exceptions are exactly the case where the guess is wrong, and they are common in the sources that have them. |
| Serve unknown rows to deployment-wide credentials only | An operator can see what the mirror is hiding, which shortens diagnosis. | Loses on failure mode a third time: a deployment-wide credential is the widest audience in the system, so the exception grants the unmirrored resource to the reader it is most costly to be wrong about. |
| Accept the orphan grant and let the join drop it | Fewer commit-time refusals; the read path already drops rows without a resource. | Loses on detectability: a silently dropped grant is a mapping defect nobody sees, and the mapping is where a coverage gap is fixed. |

## Criteria

1. **Which failure mode the default produces** — a denial or a disclosure. *This
   criterion decides.* Every other criterion here is about cost and convenience, and
   those are recoverable: an operator who finds a corpus too dark can extend the sweep,
   name the resource in manifest policy, or exclude the table. A disclosure is not
   recoverable, and on an organization-wide face it is the failure the whole contract
   exists to prevent.
2. **Detectability by the party who can fix it** — whether the gap reaches a mapping
   author or a sweep operator as a named condition.
3. **Corpus coverage** — how much of the ingested content answers questions.
4. **Distinguishability of a gap from a decision** — whether an absence reads as "no
   observation" rather than as "denied".

## Consequences

An answer computed over a partially mirrored source is honestly narrow. The overshare
report and the explain output both read the same rows the enforcement path reads, so the
size of the invisible region is measurable rather than inferred.

The accepted cost is a dark corpus. Content is ingested, stored, paid for, and answers
nothing; a reader who knows the document exists is told it does not. That gap is
frustrating in exactly the way that produces pressure to widen the default, which is why
naming a resource in manifest policy is an explicit, reviewable act rather than a flag.

Reversing this is expensive because every deployment's answers would silently widen with
no read-path signal, and the resources that changed audience are by construction the ones
nobody has a record for.

## Revisit triggers

- The unknown class covers a large enough share of a source's resources that operators
  route around the mirror rather than fixing the sweep.
- A source is found whose container class is enforced by the source itself for every
  contained item, with per-item exceptions structurally impossible, which would make
  inheritance a derivation rather than a guess for that family.
- Break-glass access to unknown-class resources is exercised often enough to argue for a
  first-class, audited path rather than a manifest policy edit.
