# D09 — An incremental position is bound to a named field; a seed is bounded

**Status:** accepted

## Context

A cursor value means nothing without the field it was measured on: compared against a renamed field it skips or re-lands a window with no error. A seed loads history from an export beside a live stream, and a seeded row stamped past the live boundary, or a changed export, corrupts the fold without a symptom.

## Decision

- `run.advance` stores the field name with each position. Opening with a different declared field refuses before any request, naming both; a deliberate rename resets the position explicitly. A pipeline with no position starts fresh.
- A row with no orderable value in the clock field, or a stream switching between text and numeric positions, fails the pull terminally, leaves the position in place and spends no attempt.
- `store.merge` resolves a cursor by its declared kind and never by recency; only lease-taking kinds live under the leased `cursors/` prefix.
- `run.seed` requires a seeded table to declare `primary_key` and an event-time `order_by`, checked at plan and at validate. A seeded stamp at or past `below` fails its whole chunk, naming the value, and is never clamped or dropped. A committed seed whose source fingerprint moved refuses the run and points at the reset verb.

## Options

| Option | Lost on | Cost |
| --- | --- | --- |
| Bind the position to its field; bound and fingerprint the seed *(chosen)* | — | A field rename or an export top-up needs an explicit reset. |
| Reset the position silently on a rename | Silent wrongness | The source's window re-lands or is stepped over with no record. |
| Resolve cursor copies last-write-wins | Record skipping | A stale machine's later write moves the position backwards or past unread rows. |
| Clamp or drop an out-of-bound seed stamp | Auditability | The engine fabricates event time, or the seed holds less history than the export. |
| Reload automatically when the fingerprint moves | Cost control | Every touch of the export spends hours and vendor quota unasked. |

## Consequences

- A position is interpretable on its own: value, field and kind travel together.
- A source that supplies no fingerprint skips the load and warns each run that a top-up goes unnoticed.
