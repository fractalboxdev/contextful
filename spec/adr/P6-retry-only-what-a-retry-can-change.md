# P6 — Retry only what a retry can change

**Status:** accepted

## Context

A retry spends attempts, wall time and vendor quota. Spent on a revoked credential, it changes nothing and delays the refusal.

## Decision

A failure carries a class, and only `Transient` and `RateLimited` retry. A refusal whose check is pure over static input is terminal and consumes no attempt. A `Permanent` failure closes its step at once with its typed cause.

## Options

| Option | Lost on | Cost |
| --- | --- | --- |
| Classify each failure; retry only the changeable classes *(chosen)* | — | Every adapter maps its failures onto the class set, and a misclassified transient fails early. |
| Retry every class uniformly | Budget | Revocations and typos retry against the vendor until the schedule exhausts. |
| Branch on error message text | Stability | A vendor rewording a message changes retry behavior. |

## Consequences

- A terminal refusal surfaces on the first attempt, with its cause intact.
- Retry schedules live in one layer, so nested retries do not multiply attempts.
