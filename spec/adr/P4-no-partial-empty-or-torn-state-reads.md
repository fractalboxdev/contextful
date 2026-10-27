# P4 — No partial, empty or torn state reads as complete

**Status:** accepted

## Context

A snapshot without its index or a page set missing its tail looks complete to a reader. An empty table is ordinary, and refusing it trains operators to ignore refusals.

## Decision

A reader sees a whole state or the prior one. An ordinary empty state is a result; an unevaluable or torn state is a refusal. A skip is legal only with a count that reaches the run record.

## Options

| Option | Lost on | Cost |
| --- | --- | --- |
| Whole-or-prior; skip only with a count *(chosen)* | — | Staging and one commit per publication; one bad follow-up discards a whole read. |
| Publish partial state and record the gap beside it | Distinguishability | A consumer reads the published identity, not the record, so a torn state serves as complete. |
| Skip silently | Trace | Never-processed rows become a silence with no count behind it. |
| Refuse ordinary emptiness | Operator response | A refusal on a quiet stream teaches operators to swallow the refusal that matters. |

## Consequences

- A reader running during a fold continues against the prior snapshot and reads the next one after publication.
- Skip counts are part of the run record, so partial coverage is queryable.
