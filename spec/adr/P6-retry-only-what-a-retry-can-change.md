# P6 — Retry only what a retry can change

**Status:** accepted

## Context

A retry spends attempts, wall time and vendor quota. Spent on a revoked credential, a malformed declaration or a missing file, it changes nothing and delays the refusal the operator needs. A failure that describes the engine rather than one unit, spent per unit, exhausts every unit's budget for a fact unrelated to any row.

## Decision

A failure carries a class, and only `Transient` and `RateLimited` retry. A refusal whose check is pure over static input is terminal and consumes no attempt; a `Permanent` failure closes its step at once with its typed cause.

- `run.retry` retries by class, never by message text.
- `connector.lease` classes a mint answer by status: 401 or 403 is `auth_expired` with no retry, another 4xx a configuration fault, a 5xx or transport error transient, and a 429 carries its `Retry-After` into the run-level retry. An expired cached lease with an unreachable provider sends nothing to the vendor.
- `run.journal` answers a missing blob with a typed failure carrying its content hash, never by re-entering the effect.
- `run.exec` treats an unreachable engine as run-scoped: the run ends, units stay outstanding with attempts unspent, and the next tick retries them whole. A link engine, a different publisher per unit, raises no run-scoped failure.

## Options

| Option | Lost on | Cost |
| --- | --- | --- |
| Classify each failure; retry only the changeable classes *(chosen)* | — | Every adapter maps its failures onto the class set, and a misclassified transient fails early. |
| Retry every class uniformly | Budget | Revocations and typos are retried against the vendor until the schedule exhausts. |
| Branch on error message text | Stability | A vendor rewording a message changes retry behavior. |
| Retry a permanent failure once as a hedge | Budget | Every terminal refusal costs one extra round trip and delays the answer. |
| Fail units one at a time on an engine outage | Attempt budget | One unreachable binary marks every outstanding unit failed. |

## Consequences

- A terminal refusal surfaces on the first attempt, with its cause intact.
- Retry schedules live in one layer, so nested retries do not multiply attempts.
