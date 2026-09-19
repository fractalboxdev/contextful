# D14 — External signals are single-valued catalog writes

**Status:** accepted

## Context

A run waits on parties outside the engine: an approver resolving a suspension, an operator stopping a fire, a clock closing a deadline. Those parties retry, arrive late, and run in other processes. The catalog is the one store every process shares, and the core reads no clock and runs no scheduler.

## Decision

Every external signal lands as one catalog write whose value is fixed once observed, and every mismatch is a named outcome.

- `run.suspend`: resolving a token again with the identical payload returns the written value; a different payload raises `AwakeableAlreadyResolved` (409). A deadline is evaluated lazily against the injected instant, and the `timed_out` transition persists; a late resolution raises `AwakeableTimedOut` (410).
- `run.cancel`: a stop is a requested-at instant, scope and reason written on the run row, raced against the pull on a 500 ms poll. Only a running or pending row accepts it; a stop matching nothing raises `CancelTargetNotInFlight` and exits non-zero.
- `canceled` is a terminal status of its own, excluded from upstream health observations.

## Options

| Option | Lost on | Cost |
| --- | --- | --- |
| Catalog write, first value wins, sticky transitions, named mismatch *(chosen)* | — | A legitimate payload correction needs a new suspension; stop latency is one poll interval; every status classifier carries one more arm. |
| Last write wins on a token | Single-valuedness | Replay would read a payload the first resumption never saw. |
| An eager timer or control socket | Footprint | Every deploy target would owe a durable timer and a second channel beside the catalog. |
| Queue an unmatched stop, or answer empty success | Legibility | A stale request would halt an unrelated fire, or an operator would wait on a run never marked. |
| Fold cancellation into failure | Downstream health | Deliberate stops would fill the failure signal and hold models at last-good. |

## Consequences

- An unpolled suspension stays nominally pending until something reads it.
- A stop reaches only the awaits that select on it; wider reach is a property of the run's cancellation token, not of this channel.
- The catalog token and the wire value for cancellation are one string, `canceled`.

## Revisit

- A deploy target where the requesting process and the run share no catalog.
- The poll's catalog read becomes measurable under high fire concurrency.
