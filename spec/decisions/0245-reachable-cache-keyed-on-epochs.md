# 0245 — The reachable-set cache keys on committed access state, and degraded results are not retained

**Status:** accepted 2026-09-18
**Decides:** `visibility.reach.refusal.caching-a-degraded-result`

## Context

Resolving a reachable set per request is not cheap. It resolves a credential's verified
person to a subject, the subject to source principals through identity links, those
principals to a group closure, and principals plus closure plus `public` to resources
through grants joined onto resource rows, dropping the unknown class and subtracting
tombstones in force. A reader asking several questions in a session pays that cost each
time, over access state that did not change between the questions.

Caching it is obvious. Invalidating it correctly is the entire problem, because a stale
reachable set is a revoked grant that still answers. The mirror exists to make revocation
take effect on the first read after its sweep commits; a cache that outlives the sweep
undoes exactly that property, and does so silently.

Access state in this system is committed state. Grants, resources, tombstones, freshness,
group edges and identity links all land through the journaled run path as ordinary store
rows, which means each of those tables has a committed version that advances when a sweep
writes to it. That makes a cache key available that is not a timestamp and not a guess: a
key built from the identity of the committed state itself.

There is a second, less obvious retention question. Some results are produced under
conditions that are not the normal ones — past a table's staleness budget under the
narrowing posture, where the served rows are a deliberately shortened set drawn from a
re-probed public signal. Such a result is correct for the instant it was computed and
wrong the moment the sweep catches up, and its shape encodes the degradation rather than
the access state. Storing it under a key describing access state files a degraded answer
where a full one is expected.

## Decision

The reachable-set cache key is the subject, a source epoch over grants, resources,
tombstones and freshness, and a directory epoch over the group graph and identity links.
Equal cache keys imply identical committed access state by construction, so a revocation
rotates the key and the superseded entry is never read again. Storing a reachable set or a
result produced past a budget, or produced under the narrowing posture, raises
`VisibilityDegradedCached`; those paths pay full cost on every request.

## Options considered

| Option | What it buys | What it costs |
| --- | --- | --- |
| **A key over the subject plus a source epoch and a directory epoch** *(chosen)* | Invalidation is a property of the key rather than a rule someone has to remember to apply, so a revocation cannot fail to invalidate. | Epoch granularity is per access table rather than per source, so a sweep of one source rotates another's keys and buys a recompute; degraded paths pay full price on every request. |
| A time-to-live on the cached set | Trivial to implement, bounded memory, one tunable number, and it works the same on every input. | Loses on whether invalidation is a construction: a window during which a revoked grant still answers is reintroduced by the mechanism that was supposed to remove it, and the window is a second undeclared staleness number sitting behind the one the operator actually declared. |
| A single global epoch over all access state | The simplest possible key, with no per-table accounting. | Loses on cost: every sweep of every source rotates every subject's key, so a deployment with many sources cold-starts the entire cache several times an hour. |
| Explicit invalidation messages emitted by the sweep | Precise: only the affected subjects are evicted, and nothing else moves. | Loses on the same criterion as a time-to-live: correctness depends on every write path remembering to emit, so a new sweep kind or a new access table is one omission away from a silent stale allow. |
| Cache degraded results under a flag in the key | The narrowing posture gets the same latency benefit as the normal path. | Loses on failure mode: a degraded set is a narrowed snapshot of a moment, and retaining it extends a deliberately temporary answer past the condition that justified it. |

## Criteria

1. **Whether invalidation is a policy question or a construction** — whether correctness
   depends on someone applying a rule or follows from the key's definition. *This
   criterion decides.* Every other option here is correct while its invalidation
   discipline is maintained, and the failure when it is not maintained is a revoked grant
   answering reads with nothing marking it wrong. A key that cannot name stale state has
   no discipline to maintain.
2. **Hit rate under ordinary sweep cadences** — how much of the cache survives a sweep.
3. **Whether a degraded answer can outlive its condition** — retention of results
   produced under the narrowing posture or past a budget.
4. **Per-request cost avoided** — the closure and join work a hit removes.

## Consequences

A revocation is complete at the first read after its sweep commits, with no eviction step
and no window. The same key makes a materialized group closure safe, since it shares
identical invalidation with the request-time walk rather than being a second account of
membership.

The accepted cost is hit rate. Epochs are per access table rather than per source, so a
sweep touching one source rotates keys for subjects whose reach that source does not
affect, and every such subject pays a full recompute on its next request. On a deployment
with many sources on tight cadences, that can approach recomputing on every request —
which is the behavior the cache was added to avoid, arrived at safely rather than
unsafely.

Degraded paths pay full cost on every request, permanently. That is a deliberate
disincentive: the narrowing posture is meant to be uncomfortable enough that it is not
left switched on.

Reversing toward time-based invalidation is expensive because every deployment's declared
staleness budget would quietly become a floor rather than a bound, with no signal at any
read.

## Revisit triggers

- Measured hit rate on real deployments falls low enough that the cache stops paying for
  itself, which would argue for per-source epoch granularity rather than for per-table.
- A source's sweep cadence tightens to the point where its epoch rotates faster than a
  typical session, making cached sets unreachable for that source's readers.
- The narrowing posture is in sustained use somewhere, which would make its per-request
  cost a real operational number rather than a deterrent.
