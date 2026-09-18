# 0244 — The group closure is a bounded read-time walk that refuses at its bound

**Status:** accepted 2026-09-18
**Decides:** `visibility.reach.refusal.closure-over-depth`

## Context

Most grants in a real source name a group rather than a person. Resolving what a subject
reaches therefore means walking the mirrored group graph from that subject's source
principals outward, collecting every group they belong to transitively, and matching the
resulting set against grant rows. `access_group_members` holds one row per edge, and
organization membership is normalized into the same table with the organization as a
node, so an `org` grant matches like any other group grant.

Two facts about the graph shape the design. It is deep and it changes. Nested groups are
how organizations express structure — a team inside a department inside a division, plus
cross-cutting groups for projects, on-call rotations and access tiers — and membership
turns over on a completely different timescale from content. A document written once is
read for years; a person joins a team, leaves a project and changes manager several times
in the same period, and every one of those events changes what they reach.

The graph also admits pathological shapes that no organization intends: a cycle
introduced by a group that contains a group that contains it, an accidental fan-out where
one group's expansion pulls in most of the directory, a mirrored graph mid-sweep where
edges from two different runs coexist. A walk with no bound is a walk that can run until
the request times out, and an unbounded walk sitting inside the hot path of every read is
an availability hazard the source controls rather than the deployment.

Bounding the walk raises a second question: what happens at the bound. A walk that stops
and returns what it has collected produces a reachable set that is narrower than the true
one. The answer computed from it contains only rows the reader is genuinely entitled to —
it is not a disclosure — and it is missing rows the reader is also entitled to, with
nothing in the response distinguishing it from a complete answer over a reader whose
reach happens to be small.

## Decision

The closure runs per request over the mirrored graph, walking to `max_group_depth`,
default 8 hops, declared per deployment. A closure reaching the depth bound raises
`VisibilityClosureDepth` (HTTP 503) and returns no set. A materialized closure is keyed
on the same epochs as the request-time walk, making it an optimization with identical
invalidation rather than a second account of who belongs to what.

## Options considered

| Option | What it buys | What it costs |
| --- | --- | --- |
| **Read-time walk to a declared bound, refusing at the bound** *(chosen)* | A membership change takes effect on the next read with no reindexing, and a graph too deep to walk produces a loud, typed failure an operator can act on. | The closure dominates an otherwise cheap query on deep or wide group graphs, and a large organization needs it materialized under identical invalidation to stay fast. |
| Fold expansion into the content index at write time | The read path never walks a graph; reachability is a column, and query cost is flat regardless of group depth. | Loses on change frequency: a single membership change would require reindexing every content row the affected groups reach, so the system's most frequent event triggers its most expensive operation, and the window between the change and the reindex is a window of wrong answers. |
| Truncate at the bound and serve what was collected | The request succeeds; a reader gets a usable, never-over-wide answer, and no availability event reaches anyone. | Loses on explainability: the narrowed answer is indistinguishable from a correct one, so a reader concludes the content does not exist and nobody observes that the system is wrong. |
| No bound at all | Correct on every graph, however deep, with no tuning knob. | Loses on availability: a cyclic or pathological mirrored graph turns every read into an unbounded walk, and the shape that triggers it is set by the source rather than the deployment. |
| A per-request time budget instead of a depth bound | Bounds the real resource — time — rather than a proxy for it. | Loses on explainability in a subtler form: the same request succeeds or fails depending on load, so the refusal is not reproducible and an operator cannot tell a deep graph from a busy machine. |

## Criteria

1. **Explainability of the result a reader receives** — whether a degraded answer is
   distinguishable from a correct one. *This criterion decides.* An empty or narrow result
   is an ordinary outcome in this system — a quiet repository, a new reader, a narrow
   corpus — so a silently truncated closure lands in the one place where wrongness is
   invisible, and it stays invisible indefinitely. A refusal is disruptive, reproducible
   and attributable, which is what makes it fixable.
2. **Change frequency of the input** — how often the data driving the computation turns
   over, and therefore what precomputing it costs.
3. **Availability under a hostile graph shape** — whether a source can make reads
   unbounded.
4. **Per-request cost** — the work the walk adds to an otherwise cheap query.

## Consequences

A membership change takes effect on the first read after its sweep commits, with no
reindexing of content and no deletion of rows. The same property that makes revocation
cheap makes the group graph cheap to keep current.

The accepted cost is per-request work proportional to the reader's position in the graph.
On a large organization with deep nesting the closure dominates an otherwise cheap query,
and the mitigation — materializing the closure — is only legitimate under the same epoch
keying as the live walk, which means it is an optimization rather than a second source of
truth. Building it is real work that a deployment past a certain size cannot avoid.

A deployment whose graph genuinely exceeds the bound is offline for the affected readers
until an operator raises `max_group_depth` or the graph is flattened. That is the
intended shape of the failure, and it is loud.

Reversing toward truncation is cheap in code and expensive in trust: every narrow answer
in the system would become ambiguous, retroactively.

## Revisit triggers

- Measured closure cost at the tail lands high enough on real deployments that the walk,
  rather than the retrieval, sets read latency.
- A deployment's genuine group graph exceeds 8 hops, which makes the default a fit
  question rather than a safety one.
- Materialized closures are in use widely enough that the request-time walk becomes the
  fallback rather than the primary path, which changes what the depth bound is protecting.
