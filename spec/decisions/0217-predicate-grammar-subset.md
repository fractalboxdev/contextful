# 0217 — A row predicate outside the typed boolean subset is refused at manifest load

**Status:** accepted 2026-09-18
**Decides:** `enforcement.filter-rows.refusal.term-outside-the-grammar`

## Context

A row predicate is operator-authored text that the relation compiler conjoins into every
registered relation for a granted table. It decides membership: a row survives the read when
the predicate holds for the calling subject. That places the predicate inside the reference
monitor, on the same footing as the tenant equality and the mirrored permission semi-join.

A predicate evaluates *inside* the relation it restricts, which means the restriction is not
itself restricted. Whatever the expression reaches, it reaches with the engine's own
authority, not the caller's. A subquery inside the predicate reads relations the caller holds
no grant on, and the rows it reads influence whether the caller's row appears — an observable
channel out of a table nobody granted. An arbitrary function call has the same reach and adds
a second problem: evaluation cost. A recursive expression or a pattern-heavy match multiplies
per-row work, and the caller chooses how many rows the scan visits.

The predicate is also declared once and evaluated many times, by many callers, long after the
operator who wrote it has moved on. Where the violation is discovered decides who can act on
it. The manifest loader has the declaration, the schema and the operator in front of it; a
caller hitting the same violation at read time has a failed query and no authority to fix it.

## Decision

A row predicate is a typed boolean subset of SQL: column references, comparison operators,
conjunction, disjunction, negation, membership lists, pattern matching, null tests, and scalar
functions declared safe. Each predicate parses to a tree when the manifest loads, and a
predicate referencing anything the grammar omits raises `EnforcePredicateOutsideGrammar`,
naming the offending node. The same grammar governs mask exceptions and exception conditions,
so one parser decides every operator-authored boolean in the enforcement path.

## Options considered

| Option | What it buys | What it costs |
| --- | --- | --- |
| **A typed boolean subset, enforced by the parser at manifest load** *(chosen)* | Reach and cost are both bounded by construction: the constructs that read foreign relations or multiply per-row work have no spelling in the grammar. The operator holds the failure. | A policy needing a lookup against another table is inexpressible and must be written another way. |
| Accept full SQL and rely on the relation's own restrictions | No grammar to maintain; an operator writes what they already know. | Loses on reach: the predicate evaluates inside the relation and is not constrained by it, so a subquery reads past the grant the relation encodes. |
| Accept a declared-safe function list with no grammar bound | Blocks the obviously dangerous calls while leaving SQL's expressive shape intact. | Loses on cost control: recursion and pattern-heavy expressions stay reachable, and the caller decides how many rows pay for them. |
| Validate at first use | No load-time pass; the parse happens where evaluation happens. | Loses on who discovers it: the failure lands on a caller who cannot repair a declaration, and a rarely-read table hides a broken predicate indefinitely. |

## Criteria

1. **Reach of the expression** — whether a predicate can observe a relation the caller holds
   no grant on. Full SQL fails this outright.
2. **Attacker control over evaluation cost** — whether a caller can make one declared
   predicate expensive per row. The safe-function list fails this.
3. **Who discovers a bad declaration** — the operator writing it, or a caller running into it.
   Validation at first use fails this.
4. **Expressiveness for real policies** — how many legitimate policies the subset can state.
   This is the criterion the chosen option loses on.

Reach decides it. Cost control and discovery both argue the same direction, but they are
recoverable: an expensive predicate can be measured and rewritten, and a late discovery is a
delay. A predicate that reads across a grant boundary is an authority failure inside the
component whose whole job is authority, and no downstream layer re-checks it.

## Consequences

The set of things a predicate can do is small enough to state in one sentence, which makes the
restriction auditable without reading the parser. Cost per row is bounded by the grammar
rather than by a timeout, so a restricted scan stays close to an unrestricted one and the
subject join pushes down into the scan.

The accepted cost: a policy whose condition depends on another table has no predicate form. It
is written as a mirrored permission table instead, which the semi-join consumes ahead of
anything the operator wrote — a second artifact to maintain, and one that must be kept in step
with the source it mirrors. Whether that covers every policy shape operators actually want is
unmeasured; the grammar is narrow by intent, so the pressure to widen it is expected rather
than hypothetical.

Widening the grammar later is cheap in mechanism and expensive in claim: each admitted
construct must be argued against reach and cost, and an admitted construct cannot be withdrawn
without breaking manifests already written against it.

## Revisit triggers

- A policy shape operators repeatedly need is inexpressible both as a predicate and as a
  mirrored permission table.
- The engine gains a way to evaluate a subquery under the caller's own grants rather than the
  relation's, which removes the reach argument.
- Measured per-row predicate cost diverges materially from an unrestricted scan, which would
  mean the grammar bounds the wrong thing.
