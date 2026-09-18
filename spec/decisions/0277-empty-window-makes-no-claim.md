# 0277 — A window holding zero observations returns no claim rather than a negative verdict

**Status:** accepted 2026-09-18
**Decides:** `accountability.attest.refusal.empty-window`

## Context

An access explanation over a window does not evaluate policy at arbitrary instants. It
replays the decision at each recorded observation inside the window and prints one line per
observation, followed by coverage — the observation count, the sweep count, the widest gap,
and how many gaps exceed the budget. The verdict that closes the report reads VISIBLE AT
SOME OBSERVED POINT or NOT VISIBLE AT ANY OBSERVED POINT.

Both verdicts are statements about observed states, and the whole design of the surrounding
output exists to keep them that way: the coverage block is printed beside the verdict
precisely so a reader can see how much of the window the observations actually cover, and a
bare "never" without that qualification is refused elsewhere in the same operation.

A window containing zero observations is the degenerate case of that structure. The replay
produces no lines, every coverage number is zero or undefined, and the negative verdict —
which is true of the empty set as a matter of logic — would print with nothing beside it to
qualify how much it covers. An auditor reading NOT VISIBLE AT ANY OBSERVED POINT over a
quiet weekend reads it as evidence the resource was not visible over that weekend. It is
evidence of nothing at all.

The emptiness has ordinary causes: the sweep was down, the source was idle, the resource is
new, or the window sits before the deployment began observing. None of them is
distinguishable from the others in the output, and none of them supports a verdict.

## Decision

A window holding zero observations returns `AssuranceWindowEmpty` and the words "no claim
can be made" in place of a verdict. The refusal is the output of a well-formed request over
a well-covered surface, so it carries the window it ran over and the coverage block with its
zeros, and a caller can tell it apart from a rejected request. The window is not widened and
no neighbouring observation is borrowed.

## Options considered

| Option | What it buys | What it costs |
| --- | --- | --- |
| **Refuse with `AssuranceWindowEmpty` and say no claim can be made** *(chosen)* | The output cannot be read as assurance it does not carry. The absence of evidence appears as an absence. | A caller asking about a quiet period gets no verdict and has to widen the window or wait for a sweep. |
| Print NOT VISIBLE AT ANY OBSERVED POINT over zero observations | Uniform with the non-empty case; one code path, one verdict vocabulary. | The strongest-looking sentence the surface emits is printed exactly when the surface knows least. A reader quoting it into an audit asserts something the data never said. |
| Return a generic error | Simple, and it certainly does not over-claim. | The caller cannot separate an unobserved resource from a malformed request, so the one case that wants a retry with a wider window looks like the one that wants a corrected argument. |
| Widen the window automatically until an observation is found | Always produces a verdict, and the verdict rests on real observations. | It answers a different question from the one asked, and the reader has to notice the changed bounds to see that. Silent scope drift under an assurance surface is the failure this contract is built against. |
| Print the verdict with a prominent coverage warning | Keeps the uniform shape and flags the gap. | A warning in an output is not a control. The verdict survives the copy-paste; the warning does not. |

## Criteria

1. **Readability as assurance** — whether a reader who quotes the output into their own
   audit asserts something the data supports. A negative verdict over zero observations is a
   claim about the empty set dressed as a claim about a resource.
2. **Distinguishability of causes** — whether the caller can tell an unobserved period from
   a bad request. The generic error loses here.
3. **Scope fidelity** — whether the answer is about the window that was asked about.
   Automatic widening loses here.
4. **Uniformity of the output shape** — one verdict vocabulary across empty and non-empty
   windows. This is the criterion the chosen option loses on.

Readability as assurance decides it. Every other criterion trades convenience; this one
decides whether the surface can be relied on at all. An assurance surface earns its name by
being unable to produce a confident sentence from an empty input, and uniformity is worth
less than that, since a refusal carrying the window and the zeros is already legible.

## Consequences

Negative assurance from this surface always rests on at least one replayed observation, so
the coverage block beside a verdict is never vacuous. An operator who sees
`AssuranceWindowEmpty` learns something actionable — the sweep did not cover this window —
which the negative verdict would have hidden.

The accepted cost is an extra round trip for the caller in the quiet case, and one more
outcome for every consumer of the explanation to handle. Automated consumers that branch on
two verdicts now branch on three states.

Reversing this is cheap in code and expensive in the field: an audit trail already contains
refusals where a later version would print a verdict, and a reader comparing old and new
reports over the same window sees them disagree.

## Revisit triggers

- Observation coverage becomes dense enough that an empty window reliably means the resource
  did not exist, at which point the emptiness itself carries information the verdict could
  express.
- A continuous observation guarantee replaces discrete sweeps, so that a window has a defined
  state at every instant rather than at recorded points.
- Operators are seen widening windows repeatedly to escape the refusal, which would mean the
  budget and the sweep cadence, not the verdict rule, are what is wrong.
