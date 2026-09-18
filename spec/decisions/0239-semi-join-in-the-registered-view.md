# 0239 — The visibility filter is a semi-join compiled into the caller's registered view

**Status:** accepted 2026-09-18
**Decides:** `visibility.mirror.refusal.view-bypass`

## Context

Mirrored permission state is ordinary store data: `access_resources`, `access_grants`,
`access_group_members` and the rest are tables with the same snapshot and compaction
mechanics as content. Resolving a caller to the resources they reach is therefore a
relational question over those tables, and the answer is a set of resource identifiers.

The read face is not one path. A caller reaches rows through structured statements, through
a lexical arm, a keyword-scored arm, a vector arm and a fused ranking over all of them,
through previews, through templates and through embedded surfaces that other applications
host. Each of those is a place where a filter can be applied and a place where it can be
forgotten. New arms and new surfaces appear over time, and the ones that appear later are
written by people who did not write the filter.

The vector arm complicates it further. A sidecar index is not a relation — its arm returns
candidate identifiers rather than rows, and those identifiers have to re-join through
something before they contribute to a result. Ranking and aggregation are the same problem
in another form: both observe the rows they operate over, so a filter applied to the output
of a ranking has already let the ranking see what it was meant to hide.

The store's query engine can compile a relation per caller and bind it to a table's bare
name for the session's lifetime. Everything that names the table then reads the relation
without knowing it exists.

## Decision

The engine compiles a semi-join against the caller's reachable resource set into that
caller's registered view, and every surface returning rows reads that view by the table's
bare name. A read path that reaches table rows around the registered view raises
`VisibilityViewBypass`, naming the face and the table. There is no supported route past the
relation for speed.

## Options considered

| Option | What it buys | What it costs |
| --- | --- | --- |
| **A semi-join compiled into the caller's registered view, with bypass refused** *(chosen)* | One enforcement point for every surface, arm and embedding. A new read path is a consumer of the relation rather than a new place to get enforcement right. | The semi-join is paid on every query against a bound table, and a read path wanting to skip it for speed has no supported route. |
| A mandatory predicate the query builder appends | No relation to maintain; the filter is visible in the statement that runs. | Loses on enforcement-point count: every call path that constructs a statement is a place the predicate can be omitted, and the paths multiply as surfaces are added. |
| A post-filter over returned rows | Simple, uniform, applied in one place after retrieval. | Loses on the same criterion at a finer grain: ranking, scoring and aggregates observe forbidden rows before the filter runs, so counts, orderings and cut positions leak what the filter then removes. |
| Filter at ingestion, storing one copy per audience | No query-time cost at all; each caller reads a store already narrowed. | Loses on tractability: audiences change continuously and overlap arbitrarily, so the copy count is unbounded and every revocation rewrites stored data. |
| A filter in each surface, with a conformance test per surface | Each surface optimizes its own path; the test catches omissions. | Loses on the same enforcement-point count, with the test standing in for the property rather than establishing it — a surface added between test updates is unfiltered and passing. |

## Criteria

1. **How many enforcement points a deployment has** — the number of distinct places the
   filter must be correct for the guarantee to hold. *This criterion decides.* The read face
   grows new arms and new surfaces over time, so any design whose enforcement-point count
   grows with it fails eventually by construction, whatever its cost profile.
2. **Whether ranking and aggregation observe withheld rows** — whether the filter runs
   before or after the operations that leak through position and counts.
3. **Tractability under changing audiences** — whether the mechanism survives continuous
   revocation.
4. **Per-query cost** — the criterion the chosen option loses on.

## Consequences

Every retrieval arm inherits the filter without naming it, the vector arm included, since
its candidate identifiers re-join through the relation before contributing anything. A
surface added later is a consumer of the relation and is filtered by construction rather
than by its author remembering.

The cost accepted is per-query work on every read against a bound table, paid whether or not
the caller's reachable set is narrow, and there is deliberately no fast path. A surface that
finds the semi-join expensive optimizes the resolution behind it rather than skipping it.

The bypass refusal also makes some legitimate operations awkward. Maintenance work that
genuinely needs unfiltered rows — compaction, integrity checks, the sweep itself — is not a
read path in this sense, and the boundary between those and a read face is a thing an
implementation has to keep sharp rather than a property the relation enforces.

Reversing toward per-surface filtering is expensive because every surface built since would
have to acquire a filter it inherits from the relation, and the surfaces are where the count grows.

## Revisit triggers

- Semi-join cost is measured at real reachable-set sizes and shows in read latency at the
  tail, which would make the per-query criterion worth weighing against enforcement-point
  count.
- A read path appears whose legitimate work needs unfiltered rows and which is not
  distinguishable from a face, which would mean the bypass refusal is drawn at the wrong
  boundary.
- The query engine gains a mechanism that binds a filter below the relation, which would
  make the relation itself an implementation detail rather than the enforcement point.
