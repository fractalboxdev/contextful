# 0101 — A sequential chunk plan refuses a parallelism above one

**Status:** accepted 2026-09-18
**Decides:** `pipeline.backfill.refusal.chunk-plan`

## Context

A backfill compiles to a chunk plan whose shape follows the source's cursor kind. A
lease-free clock plans time-window or id-range chunks: each chunk carries a predicate
that names its own slice of the source, so any worker can fetch any chunk in any order
and the union is the whole range. A page token plans one sequential unbounded chunk
walked in order, because the only way to reach page three is to hold the token page two
returned. A version id plans version ranges sequential per stream. A partitioned clock
plans partition-by-window chunks safe across partitions.

The plan's parallelism is declared by the operator beside it, as a throughput lever over
a backfill that may run for hours across many scheduler ticks. On a windowed plan it is
exactly that: N workers take N independent predicates, and a worker that loses a race
re-fetches a window the fold collapses by key.

A sequential plan has no independent predicates to hand out. The position is a token the
previous response produced, held in one place. Two workers advancing it do not each
fetch the range and duplicate — they each take the token, each advance it, and the
loser's segment is stepped over. Nothing fetched it, nothing recorded that it was
skipped, and the chunk completes done. The fold has nothing to collapse, because the
rows never arrived.

The two failure modes therefore differ in kind rather than in degree. Duplicate fetches
cost vendor quota and are absorbed by the declared key. Skipped segments are silent
permanent data loss inside a run that reports success.

## Decision

A sequential chunk plan declared beside a parallelism above one raises
`PipelineChunkParallelism`, naming the plan and the value. The refusal lands at plan
time, ahead of any fetch, so the operator corrects the declaration rather than reading
a completed backfill that is missing segments.

## Options considered

| Option | What it buys | What it costs |
| --- | --- | --- |
| **Refuse the declaration at plan time** *(chosen)* | A sequential walk cannot lose segments; the operator learns the plan kind does not take the lever they reached for | A long sequential backfill has no throughput lever inside the engine |
| Allow the parallelism and rely on the fold | One uniform declaration across every plan kind; the windowed case keeps its lever | Lost on data loss: the fold collapses duplicates by key, and a page-token walk skips segments rather than duplicating them, so there is nothing for the fold to see |
| Silently clamp the value to one | Nothing refuses; the run is correct | Lost on auditability: the manifest then describes a run that did not happen, and an operator reading a declared parallelism of eight has no way to learn the backfill ran with one |
| Warn and clamp | Correct run, and the warning is present | Lost on where the signal lands: a warning in a run log is read after the backfill, while the declaration is wrong before it, and a plan-time refusal costs the operator one edit instead of hours |

## Criteria

1. **Whether a losing concurrent advance costs duplicate fetches or lost data** — the
   nature of the failure the lever admits. *(decided it)*
2. **Auditability of the declaration** — whether the manifest describes the run that
   happened.
3. **Throughput available to a long backfill.**
4. **Uniformity of the declaration across plan kinds.**

The nature of the failure decided it because it is the one criterion on which the plan
kinds genuinely differ. Throughput and uniformity both argue for admitting the value
everywhere, and both are arguing about cost and convenience; lost data is not a cost the
operator can price, because they cannot see it. A backfill that silently holds a hole is
worse than one that takes longer, and worse than one that refuses to start.

Auditability then settled the shape of the refusal against clamping: the failure is in
what the operator wrote, and the only place to correct it is before the run.

## Consequences

A backfill over a page-token or version-id source is honest about its duration: it is
the vendor's page latency times its page count, and no declaration shortens it. That is
a real limit an operator plans around — a multi-day initial load for a large history is
the expected shape rather than a misconfiguration.

The windowed and partitioned plans keep the lever, so the sources where concurrency is
safe are unaffected.

The cost accepted is the absence of any throughput lever inside the engine for the
sequential kinds. Speeding such a backfill means narrowing the window, adding streams
where the source exposes them, or waiting. The engine offers no third option, and for a
vendor with slow pages and deep history that is the dominant cost of onboarding the
source.

Reversing this is expensive in a specific way: any run made under a relaxed rule holds
holes nobody can locate afterwards, since the chunks completed done and the plan records
no skipped segment. Re-loading the whole range is the only repair.

## Revisit triggers

- A source appears whose sequential cursor is resumable from a recorded position rather
  than from the previous response alone, which makes independent predicates
  constructible and the plan no longer sequential.
- A vendor exposes a stream or shard dimension over a page-token feed, which turns the
  plan into the parallel-across-streams shape the version-id kind already uses.
- Onboarding times for sequential sources become the dominant complaint, which is the
  cost this decision accepted and the reason to look for a safe lever rather than to
  relax the refusal.
