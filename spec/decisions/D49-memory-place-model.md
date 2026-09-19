# D49 — Nearness is a parent closure over declared edges, and the engine holds no geospatial type

**Status:** accepted

## Context

Rows carry an opaque `place_id` stamped by the entity matcher; the deployment supplies places, aliases and place-to-entity exposure as tables it maintains. The questions asked are containment: which claims concern this district, roll a figure from building to region. A geometry type brings a projection, a distance function and a spatial index with its own lifecycle, shared with nothing else in the engine.

## Decision

`read.resolve-entity` answers nearness as the transitive closure over the deployment's declared parent edges: near a place means at it or under it. The closure is materialized once and joined as an ordinary dimension, left-joined so an unmapped place tags null and the row survives. The engine holds no geometry type, distance function or radius.

## Options

| Option | Lost on | Cost |
| --- | --- | --- |
| Parent closure as an ordinary dimension *(chosen)* | — | A radius question is unanswerable; hierarchy gaps show as null tags, not errors. |
| A geospatial column type with radius search | Scope | A type system, index family and projection question would enter for a workload that needs none. |
| Coordinate pairs with ad-hoc distance in SQL | Correctness | Raw latitude-longitude distance errs by a latitude-dependent margin nobody states. |
| An external spatial service at query time | Consistency | A bounded read would mix two stores' clocks. |

## Consequences

- The place dimension replicates, reads at a vantage and falls under enforcement like any row.
- A metric answer is computed outside and landed as rows carrying their own freshness.
- The opaque `place_id` is expensive to reverse; a geometry type added later sits beside the closure.

## Revisit

- A deployment declares artificial parent levels to approximate a radius.
- Closure materialization dominates the dimension's build.
