# D21 — The derive work set is an anti-join against marker rows

**Status:** accepted

## Context

Deferred per-row work — transcription, link preview — runs over rows already landed, arrives out of order, and fails per unit for reasons ranging from a flapping network to a refused scheme. A watermark advances past a gap and never returns; a failure recorded outside the output is invisible to whatever selects the next unit.

## Decision

The tier's own output table is the single record of what is done, failed or settled, and the work set is recomputed from it every tick.

- `run.select` is a built-in source connector over the store. Each tick reads the parent table and the tier's own output and drops every parent already holding a row, markers included; the row cap applies after the anti-join. The output table name derives from the pipeline identifier.
- `run.land` records a unit that produced nothing as one marker row carrying status, attempts, last error and a `retryable` column.
- Status separates `ok`, `empty` (the engine established absence), `unavailable` (silence, the default) and `failed`.
- `retryable = false` settles a unit for good; `attempts` counts what was tried, so raising an attempt ceiling revives no settled refusal.

## Options

| Option | Lost on | Cost |
| --- | --- | --- |
| Anti-join each tick against output rows and in-table markers *(chosen)* | — | The scan is linear in the parent table; every consumer filters marker rows; two columns carry one unit's fate. |
| A watermark over the parent table, with or without a backfill window | Stranding | A gap passed once would stay underived. |
| A dead-letter queue or sibling marker table | Visibility to the anti-join | Failed units would be re-selected every tick and be unreachable beside content. |
| One empty status for every silence | Honesty of landed values | A bot check or geo-block would land as a statement about the recording. |
| Permanence encoded as an attempt count | Permanence | Raising the ceiling would revive every security refusal at once. |

## Consequences

- Cost grows with the archive, not with the outstanding set.
- Every engine carries an absence-reason channel and asserts emptiness explicitly.
- A marker written with no permanence value re-attempts its unit once.

## Revisit

- The parent scan dominates tick cost on a real archive.
- A re-derive signal for changed parent rows becomes necessary.
