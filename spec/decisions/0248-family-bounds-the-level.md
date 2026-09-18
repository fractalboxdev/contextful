# 0248 — The declared source family mechanically bounds the fidelity level a table may claim

**Status:** accepted 2026-09-18
**Decides:** `visibility.declare-fidelity.refusal.level-outside-the-family`, `visibility.declare-fidelity.refusal.family-undeclared`

## Context

A table declares a fidelity level: `mirrored` where the source enforces per-principal
lists at a grain the mirror can join exactly, `coarse` where a container or workspace
signal approximates it, `federated` where the source is queried live under the reader's
own credential, or `excluded` where no safe path exists. Rows are served out of the store
at the first two levels only.

The level is a claim about what a source's permission model supports, and it is written by
a pack author who has read that model. Some claims are wrong in ways that are obvious in
retrospect and invisible in review, because the difference between a defensible level and
an indefensible one is a fact about the shape of the source rather than a fact about the
mapping's code.

Two shapes of source make a servable level structurally wrong rather than merely
optimistic. In a `person-container` source, the container is one human being — a mailbox,
a personal drive, a direct-message history. Its access list reads, correctly and
uselessly, as one principal, and a mapping that mirrors it faithfully produces a table
whose every row grants to exactly one person, which is either right and worthless or a
projection error that silently widens. In a `directory` source, the rows are records about
people rather than content — profiles, reporting lines, employment attributes — and the
source's own permission model governs who may query the directory, not who appears in it,
so any per-row audience the mapping derives is invented.

These are not judgments made per table. They follow from what kind of source it is, which
is a judgment made once, by whoever integrates the source, and recorded as `family`:
`container-roster`, `item-exception`, `person-container` or `directory`. Everything
downstream of that classification is a bound rather than a question.

A mapping that declines to name a family gets the bound for free, which is to say it gets
no bound at all, and the level claim goes back to resting on reviewer judgment alone.

## Decision

`family` takes `container-roster`, `item-exception`, `person-container` or `directory`,
and a mapping declares it for every table it lands. A `person-container` table declared at
a servable level, and a `directory` table declared above `excluded`, raise
`VisibilityFamilyBound` and the table does not load. A mapping that lands a table without
naming its family raises `VisibilityFamilyUndeclared`. Classification is a judgment made
once; the bound downstream of it is mechanical.

## Options considered

| Option | What it buys | What it costs |
| --- | --- | --- |
| **A declared family, with the permitted levels derived from it and enforced at load** *(chosen)* | The one judgment a human makes is made once per source and stated explicitly, and the levels it permits are checked by the loader on every table rather than by a reader on every review. | The bound binds only where a family is declared, so the refusal of an undeclared family is what keeps the reviewer-judgment failure mode out, and every mapping pays a declaration it might consider obvious. |
| Reviewer judgment on each level declaration | No vocabulary to maintain, no classification to get right, and full flexibility for a source that does not fit the four families. | Loses on failure visibility: the mistake surfaces as a disclosure, months later, at a reader who was never entitled to the row, and the review that would have caught it is the one thing that already happened. |
| A per-vendor allowlist of permitted levels | Precise for every source anyone has integrated, with no abstraction to argue about. | Loses on generality: it grows one entry per integration and says nothing about a source nobody has seen, so the first table of a new source is unbounded — which is exactly when the classification matters most. |
| Infer the family from the mapping's shape | Nothing to declare; the bound derives from the projection the author already wrote. | Loses on the same criterion as reviewer judgment, indirectly: a `person-container` source mapped with a container-roster projection infers the wrong family from the very mistake the bound exists to catch. |
| Warn on an out-of-family level rather than refusing to load | A table that the author knows is an exception still works, and the warning records the disagreement. | Loses on failure visibility: a warning at load is read once, by the person who already decided, and the table serves rows for as long as it exists. |
| Default an undeclared family to the most restrictive | No refusal to handle; an omission fails closed rather than open. | Loses on where the decision lands: a table silently dropping to `excluded` looks like a broken integration rather than a missing declaration, so the author debugs the wrong thing. |

## Criteria

1. **Whether the check is reviewable or mechanical** — whether the correctness of a level
   claim depends on a human noticing something on the day they read the mapping. *This
   criterion decides.* Classifying a source is genuinely a judgment and there is no
   mechanism that can replace it; the point is that it is one judgment, stated once,
   where it can be argued about — and that everything derived from it is a bound a loader
   enforces every time, rather than a second judgment made again per table under less
   attention.
2. **Generality over a source nobody has integrated yet** — whether the mechanism says
   anything about the next source.
3. **Where the failure surfaces** — at load, at review, or at a reader.
4. **Declaration burden on a pack author** — one more required field per table.

## Consequences

A mapping author states the kind of source once and receives a mechanical answer about
what levels that kind permits, at load, before any row is served. The classification is a
small, legible field that a reviewer can argue with directly, which is a far better review
surface than a level claim whose justification is implicit.

The accepted cost is that the bound binds only where a family is declared, which makes
`VisibilityFamilyUndeclared` the clause carrying the weight: without it, every table
omitting the field would return to reviewer judgment, and omission is the cheapest path.
Pack authors pay a required declaration on tables where the family feels self-evident.

A source that genuinely does not fit the four families has no expressible classification
and cannot land a table until the vocabulary grows, which is a deliberate obstruction
rather than an oversight.

## Revisit triggers

- A source appears whose permission model fits none of the four families, making the
  vocabulary a fit question.
- A `person-container` source is found that exposes a genuine per-resource grant list
  distinct from the container's owner, which would make the bound wrong for that family
  rather than conservative.
- The family declaration is observed being set to whatever loads rather than to what the
  source is, which would mean the field is being treated as a switch rather than as a
  classification.
