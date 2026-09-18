# 0082 — Cancellation is a terminal status of its own, excluded from upstream health observations

**Status:** accepted 2026-09-18
**Decides:** `run.cancel.invariant.distinct-terminal-status`

## Context

A run is pending or running before it settles, and success, partial failure, failed or
canceled after. Every surface classifying a run covers all six, and the status is durable data
in the run record rather than a transient label — it is read by the history window, by the
export, by the console, and by anything downstream that decides whether its input is
trustworthy.

The statuses are read for different purposes. A failed run is evidence about the upstream: a
vendor refused, a schema moved, a credential expired. It is the signal an operator chases and
the signal a downstream consumer reacts to — a materialized model that would rather serve its
last good build than rebuild over a source that just failed holds where it is when its
upstream pipeline reports a failure.

A stopped run carries none of that. It is evidence about the operator: somebody typed a
command. The upstream may be perfectly healthy and was never asked. Folding the two together
puts an operator's own deliberate action into a signal whose whole meaning is "something out
there is wrong", and the consumer that reacts by holding at last-good does so on the strength
of a command the operator issued for an unrelated reason.

There is a second, smaller fact in the same clause. The status token travels from the catalog
onto the wire, and a status mismatched across that boundary is a bug that only shows up in a
consumer's classification arm.

## Decision

`Canceled` is a terminal status apart from the failure status, and upstream health
observations pass over it, so a deliberate stop holds no model at last-good and enters no
failure signal an operator is chasing. The catalog token and the wire value are the same
string, `canceled` with one `l`. Every surface classifying a run covers the status explicitly,
and a stopped run reports the step it was executing as failed carrying a cancelled tag while
leaving the run-level error unset — nothing went wrong, and the status is the whole story.

## Options considered

| Option | What it buys | What it costs |
| --- | --- | --- |
| **A terminal status of its own, excluded from health** *(chosen)* | Each status keeps one meaning, so a health reading stays a statement about upstreams and an operator's own action stays out of it. | Every place that classifies a run status carries one more arm, and the catalog token and the wire value have to stay the same string. |
| Fold cancellation into the failure status | One fewer status, one fewer arm everywhere, and no new token to keep in step across the boundary. | Loses on downstream consumers. The health signal an operator chases fills with deliberate stops, and a build engine holds a model at last-good on the strength of an operator's own command. |
| Fold it into success | Also one fewer status, and no false alarm in any health reading. | Loses on honesty. A stopped run did not do its work; reporting success over it makes a zero-row stop indistinguishable from a quiet stream. |
| Keep the failure status and add a cancellation flag beside it | No new status to cover, and consumers that ignore the flag still see something reasonable. | Loses on defaults. Every consumer has to opt into reading the flag, and the one that does not is exactly the one this decision exists to protect. |

## Criteria

1. **What each status is read for.** Whether one status carries one meaning to every reader.
2. **Behavior of a consumer that was not updated.** What a downstream reader does with the
   status if it does nothing new.
3. **Honesty about work done.** Whether the status describes what actually happened to the
   data.
4. **Surface area added.** How many classification sites gain an arm.

Criterion 1 decides it. A health signal is only useful while every member of it means the same
kind of thing, and mixing evidence-about-the-upstream with evidence-about-the-operator makes
the signal unreadable for the purpose it exists to serve. Criterion 2 rules out the flag
variant, which is otherwise the cheapest answer: a default that misleads an unmodified consumer
is the same failure with an extra step. Criterion 4 is the cost.

## Consequences

An operator can stop runs freely without contaminating anything that reads run health, and a
downstream build engine distinguishes "the source broke" from "someone stopped the source" with
no configuration.

The accepted cost is breadth. Six statuses is one more than five in every match, every wire
mapping, every console filter and every export projection, and the classification is exhaustive
by rule, so a new status has to declare which side of the health reading it falls on. The token
carries a second obligation: the catalog value and the wire value are one string, which is a
constraint two independently-written serializers can violate quietly.

Nothing distinguishes a stop issued in response to a genuine upstream problem from one issued
for an unrelated reason. An operator who stops a run because it was failing anyway produces a
record that reads as a clean operator action, and the failure that motivated it is visible only
in the earlier runs.

## Revisit triggers

- A reader appears that genuinely wants stopped and failed runs together, and cannot compose
  the two statuses itself.
- A seventh status is proposed, which by the classification's own rule declares its side of the
  health reading and re-opens where the boundary sits.
- Stops issued in reaction to upstream trouble become common enough that the reason field is
  doing classification work the status should do.
