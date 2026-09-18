# 0256 — Negative assurance is scoped to observed states and carries its coverage

**Status:** accepted 2026-09-18
**Decides:** `visibility.explain.refusal.empty-window`, `visibility.explain.refusal.unqualified-negative`

## Context

The mirror holds permission state sampled by a sweep, not streamed continuously. Between
two observations a source can grant a share and withdraw it again, and nothing in the
tables records that it happened. A windowed replay therefore evaluates the decision at
each recorded observation and knows nothing at all about the intervals between them.

The question people bring to that replay is a compliance question: was this person able to
read this resource during this period? The shape of the answer they want is a flat no. The
shape the data supports is narrower — not visible at any point the deployment observed.

The gap between the two is the entire decision, and its size is measurable. Forty-one
observations over the window with a widest interval of eleven minutes against a
fifteen-minute budget is a dense sample and a strong statement. Three observations with a
six-hour gap is a weak one. Both would print the same bare negative, and a reader with no
coverage block cannot tell them apart.

The degenerate case is a window holding no observations at all. Evaluated as a negative it
reads as the strongest possible assurance while resting on nothing, which is the exact
inversion of what the data supports.

## Decision

An assurance answer carries a coverage block: observation count, run count, the widest
interval between observations, and how many intervals exceeded the table's budget, each
flagged. A window verdict reads `VISIBLE AT SOME OBSERVED POINT` or `NOT VISIBLE AT ANY
OBSERVED POINT`, printed beneath that block. An answer emitted without the block raises
`VisibilityUnqualifiedAssurance`, and a window holding no observations raises
`VisibilityNoObservations` and states that no claim is available.

## Options considered

| Option | What it buys | What it costs |
| --- | --- | --- |
| **Verdict scoped to observed points, printed beneath its coverage** *(chosen)* | The claim is weighable: a reader sees the sample density behind it and discounts accordingly. | The answer is longer and weaker than the one asked for, and its strength depends on sweep cadence, so a loose budget weakens past assurance retroactively. |
| An unqualified never-visible verdict | Exactly the shape a compliance reader wants, in one line. | Loses on whether the claim survives scrutiny: it is false whenever a grant and a revocation both land between two runs, and the failure is undetectable from the answer. |
| The verdict with no coverage block | Shorter, and still technically scoped by the wording. | Loses on the same criterion. A dense window and an almost-empty one print identically, so the qualification exists in the phrasing and not in anything the reader can weigh. |
| Treat an empty window as a negative | No special case; every window returns a verdict. | Loses on the same criterion, at the extreme: a claim over nothing observed reads as the strongest assurance available while resting on no evidence. |
| Interpolate across intervals, assuming state held between observations | Restores the flat answer with a stated model behind it. | Loses on honesty about the source: nothing supports the assumption, and a short-lived grant is exactly the event the interpolation erases. |

## Criteria

1. **Whether the claim survives scrutiny by a compliance reader.** *This criterion
   decides.* An assurance answer exists to be relied on by someone who will be asked to
   defend it; a claim that cannot be defended when the sampling gap is pointed at is worse
   than no claim, because it was acted on.
2. **Whether a reader can distinguish a dense window from a sparse one.** The coverage
   figures are what makes the verdict weighable rather than merely worded.
3. **Brevity of the answer.** Given up.
4. **Cost of computing coverage.** The intervals are already derivable from the
   observation instants the replay walks.

## Consequences

Assurance and sweep cadence become one subject. Tightening a budget strengthens future
assurance and does nothing for past windows; loosening one weakens every answer computed
over the loosened period, including answers already given. An operator tuning cadence for
cost is tuning the strength of a compliance claim, and the coverage block is where that
shows up.

The accepted cost is that the answer is never the flat no the reader wanted. Some readers
will read past the coverage block and treat the verdict as unconditional anyway, and the
block's presence does not prevent that — it only makes the qualification available to
anyone who looks.

Reversing toward an unqualified verdict is cheap to implement and expensive in fact: every
answer given under the stronger wording becomes unfalsifiable after the fact, since the
coverage that would have qualified it is not carried.

## Revisit triggers

- A source gains a gap-detectable event stream dense enough that intervals between
  observations are bounded rather than sampled, which would make a continuous claim
  supportable.
- Coverage blocks are observed to be uniformly dense across deployments, which would make
  the qualification true but uninformative.
- A second assurance consumer appears whose question is about a point rather than a
  window, where the coverage figures do not apply.
