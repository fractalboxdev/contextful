# D35 — Mirrored authorization carries a freshness budget and refuses past it

**Status:** accepted

## Context

Access is decided by a join over grants mirrored from each source. A mirrored grant ages between sweeps, and a revoked grant keeps answering until the next observation lands. The failure to prevent is an aged allow that reads as healthy.

## Decision

Staleness is one enforced figure per source, and every path past it narrows or refuses.

- `disclosure.sweep` advances `watermark_at` only on a run that read every governed resource; an event stream moves it only where declared gap-detectable.
- `disclosure.bound-staleness` checks `max_acl_staleness` per table. Past it, a read raises `VisibilityAccessStale` (HTTP 503) with the lag, the budget and the last observation; a never-swept source is maximally aged. A budget tighter than the sweep cadence fails at diagnose.
- `on_stale = public_only` serves only rows a public-status sweep, its own watermark inside budget, marks public, makes no source call on the read path, and marks the result degraded.
- `disclosure.reach` walks the group closure to `max_group_depth` and 10000 nodes, and raises `VisibilityClosureDepth` with HTTP 422 at either bound rather than truncating.
- The reachable-set cache keys on the subject plus a source epoch and a directory epoch; a degraded result is never retained.

## Options

| Option | Lost on | Cost |
| --- | --- | --- |
| Coverage watermark, per-table budget, typed refusal, swept public-status narrowing *(chosen)* | — | A limping sweep takes a source offline; an expensive full run makes its budget hours; the public-status sweep is a second sweep shape to operate. |
| Serve past the budget with an advisory freshness field | Detectability | A revoked grant would keep answering, marked by a field nobody must read. |
| Return zero rows past the budget, or truncate the closure | Detectability | A shortened set would read as a quiet corpus indefinitely. |
| Newest observation anywhere sets the watermark | Worst-case coverage | One fresh resource would vouch for grants observed days ago. |
| A time-to-live on the reachable-set cache | Invalidation by construction | A revoked grant would answer for an undeclared second staleness window. |

## Consequences

- A revocation rotates the cache key, so no superseded entry is read again.
- A sweep of one source rotates keys for other tables sharing its epoch.
- Refusal carries every figure an operator needs to diagnose it.

## Revisit

- A source offers sequenced delivery with gap reconciliation, making its stream eligible to move the watermark.
