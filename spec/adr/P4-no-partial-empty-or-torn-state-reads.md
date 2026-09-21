# P4 — No partial, empty or torn state reads as complete

**Status:** accepted

## Context

A snapshot without its index, a pull assembled from two indexes, a page set missing its tail and a materialization that failed its contract all look like complete answers to a reader. An empty table or a blank field in one row is ordinary, and refusing it trains operators to ignore refusals.

## Decision

A reader sees a whole state or the prior one. An ordinary empty state is a result; an unevaluable or torn state is a refusal; a skip is legal only with a count that reaches the run record.

- `store.fold` publishes a snapshot and every declared sidecar in one commit, and reports a table that landed nothing per table while the pass continues.
- `store.pull` lands a tree consistent with one index or nothing, discards an object whose digest disagrees with its entry, and refuses after a bounded number of convergence rounds, leaving the catalog untouched.
- `run.publish` never exposes a materialization failing its declared contract, and omits a table whose artifacts disagree rather than reconstructing a value.
- `connector.source` fails the whole read on a failed follow-up, a missing page cursor or an authentication failure.
- `run.backfill` refuses an inverted or unevaluable rewind window and treats a well-formed window matching nothing as a no-op.
- `run.parse-cues` skips and counts an unreadable block and refuses a block running backwards; `run.select` skips and counts a parent row missing its key.

## Options

| Option | Lost on | Cost |
| --- | --- | --- |
| Whole-or-prior; skip only with a count *(chosen)* | — | Staging and one commit per publication; one bad follow-up discards a whole read. |
| Publish partial state and record the gap beside it | Distinguishability | A consumer reads the published identity, not the record, so a torn state serves as complete. |
| Fail everything on any defect | Availability | One blank field or one malformed third-party block stops a pipeline of good units. |
| Skip silently | Trace | A population of never-processed rows becomes a silence with no count behind it. |
| Refuse ordinary emptiness | Operator response | A refusal on a quiet stream teaches operators to swallow the refusal that matters. |

## Consequences

- A reader running during a fold continues against the prior snapshot and picks up the next after publication.
- Skip counts are part of the run record, so partial coverage is queryable.
