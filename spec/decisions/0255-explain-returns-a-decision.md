# 0255 — The access explanation returns a decision and its path, never a row

**Status:** accepted 2026-09-18
**Decides:** `visibility.explain.refusal.row-in-a-diagnostic`

## Context

The commonest question a reader asks about a governed deployment is why they do or do not
reach something. Answering it well needs the machinery: the principals a subject resolved
to, the group closure actually walked, the grant paths found, the tombstones in force, the
watermark and its lag. That output is a reasoning trace about a resource.

A diagnostic that reasons about a resource sits one short step from a diagnostic that
returns the resource. The step is attractive for a real reason: the operator debugging a
complaint often wants to see the rows to confirm the table holds what they think. Every
comparison mode, every "show me what the unfiltered query would return", every side-by-side
of governed and ungoverned output, is that same step.

Taken, the step creates a second read path with a different ceiling from the one the whole
contract rests on. The read path compiles a semi-join into the caller's view and the read
face consumes that view as its one relation. A diagnostic returning unfiltered rows is not
a variant of that path — it is a path around it, permanently available to whoever can call
the diagnostic, and reachable by anyone who can phrase a question as a diagnostic.

The narrow version fails the same way. An elevated-read mode scoped to operators still
means one principal holds a ceiling higher than every human the mirror governs, and that
ceiling exists whether or not anyone is debugging.

## Decision

The access explanation returns `VISIBLE` or `DENIED` together with the path that produced
it, and no content from the resource under examination. Returning such content raises
`VisibilityDiagnosticRow`, whatever credential called the diagnostic.

## Options considered

| Option | What it buys | What it costs |
| --- | --- | --- |
| **Decision and path only, no content, for every caller** *(chosen)* | The diagnostic cannot become a disclosure path, and there is exactly one read path with one ceiling. | An operator debugging a content-shape problem gets no rows here and takes a second step through an ordinary governed query. |
| An elevated-read mode returning unfiltered rows for comparison | Answers "is the table wrong or is the filter wrong?" in one call. | Loses on whether the diagnostic can become a disclosure path: the mode holds a standing ceiling above every human, available to whoever can call it. |
| Return rows the *caller* reaches, as a sample | No new ceiling; the sample is within the caller's own scope. | Loses on usefulness for the one case that motivates it — the caller who cannot see the resource is precisely the caller asking — and reintroduces content into a decision endpoint for no answer. |
| No machine answer at all; route the question to a support workflow | Nothing to abuse. | Loses on usefulness: the commonest reader question becomes unanswerable at the surface, and the reasoning is reconstructed by hand from tables. |

## Criteria

1. **Whether the diagnostic can itself become a disclosure path.** *This criterion
   decides.* A decision endpoint that returns no content cannot leak what it reasons
   about regardless of how it is called, and that property holds without depending on who
   is trusted; every other option's safety depends on a caller list staying correct.
2. **Whether the reader's own question is answerable.** The path output has to be rich
   enough that a denial explains itself.
3. **Operator debugging cost.** Real, and given up.
4. **Number of read paths with distinct ceilings.** One.

## Consequences

The path output carries the weight. It names the resolved principals with their link
methods, marks asserted links as conferring nothing, prints the closure with depths, names
what overrode a live grant, and distinguishes `NEVER OBSERVED` from a denial — because
none of that can be recovered by looking at rows.

The accepted cost falls on the operator. Confirming what a table actually holds is a
separate query under a credential that genuinely grants it, so a shape problem and a
permission problem are diagnosed in two steps rather than one, and the two-step path is
slower exactly when someone is escalating.

Reversing is a one-way door in practice. A comparison mode, once available, is written into
runbooks and tooling, and withdrawing it later removes a capability operators have come to
treat as the normal way to debug.

## Revisit triggers

- A content-shape diagnostic appears elsewhere that answers the operator's question
  without reading through this endpoint.
- The path output is observed to be insufficient for real denials — cases where operators
  routinely need rows because the trace does not say enough.
- A per-resource attestation makes "what does this table hold" answerable from metadata
  rather than from content.
