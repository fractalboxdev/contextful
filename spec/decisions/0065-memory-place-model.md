# 0065 — Nearness is a parent closure over declared edges, and the engine holds no geospatial type

**Status:** accepted 2026-09-18
**Decides:** `memory.resolve-entity.invariant.no-geometry`

## Context

Rows carry a place. A row answers which place it concerns through an opaque `place_id`
stamped by the same matcher `entity_id` is stamped by, and the deployment supplies the place
dimension, its per-language aliases, and the map from a place to the entities exposed at it
as three CSV tables it maintains.

The questions asked of that dimension are containment questions. Which claims concern this
district. Which entities are exposed at this site or anywhere under it. Roll this figure up
from a building to a campus to a region. "Near" in the vocabulary of this corpus means at a
place or under it — a relation over a hierarchy the deployment already declares, not a
distance.

Answering a metric question instead is not a small addition. A geometry type brings a
coordinate system, a projection, a distance function whose answer depends on which
projection was chosen, and a spatial index family with its own build, its own storage and
its own maintenance under compaction. None of that is shared with the rest of the engine:
the store is columnar files with a snapshot commit, the read path is one query language,
and the enforcement rules are written over rows and columns. A spatial index is a second
structure with a second lifecycle, and a projection is a second correctness question no
other part of the system has.

The closure, by contrast, is data. Parent edges are declared rows, the transitive closure
over them is materialized once, and it joins as an ordinary dimension — under the same
enforcement, the same commit and the same vantage as anything else. The join onto the alias
dimension is a left join, so an unmapped place lands a null tag and the row survives for
both the reader and the dimension's maintainer.

## Decision

The engine holds no geometry type, no distance function and no radius. Place identity is an
opaque `place_id`, and nearness is the transitive closure over the deployment's declared
parent edges: near a place means at that place or under it. The closure is materialized once
and joined as an ordinary dimension. A spatial question is answered by the parent closure
or not at all.

## Options considered

| Option | What it buys | What it costs |
| --- | --- | --- |
| **A parent closure over declared edges, materialized as an ordinary dimension** *(chosen)* | Covers the containment questions the dimension is asked; no new type, index or projection; enforcement, commit and vantage rules apply unchanged | A genuine radius question is unanswerable, and the parent hierarchy is operator-maintained data whose gaps show as null tags rather than as errors |
| A geospatial column type with radius search | Metric questions answerable directly; a familiar interface for anyone arriving from a spatial database | Lost on scope: it adds a type system, an index family and a projection question to an engine whose containment workload needs none of them |
| A per-row coordinate pair with ad-hoc distance computed in SQL | No new type and no index — just two numeric columns and an expression | Lost on correctness across projections: a distance over raw latitude and longitude is wrong by a margin that varies with latitude, and nothing in the query text says which projection the author assumed |
| An external spatial service consulted at query time | Full metric capability with none of it in the engine | Lost on the consistency boundary: the answer would come from a store with its own vantage, so a bounded read would mix two clocks |

## Criteria

1. **Scope of what enters the engine** — new types, new index families, new lifecycles.
2. **Fit to the questions asked** — how much of the workload the model answers.
3. **Correctness** — whether an answer is right independent of an assumption the caller did
   not state.
4. **Maintenance burden on the deployment** — data the operator keeps accurate.

Criterion 1 decided it, because the workload is containment and criterion 2 is therefore
satisfied by the cheapest option available. Had the questions been metric, criterion 2
would have outranked criterion 1 and the geometry type would have been correct despite its
weight — the rejection is about this workload, not about geospatial types. Criterion 3 is
what removes the coordinate-pair shortcut from consideration even for a deployment willing
to accept an approximation, since the approximation's error is unstated and position
dependent.

## Consequences

The place dimension costs nothing the rest of the engine does not already pay. It is rows,
so it replicates, it reads at a vantage, it is subject to enforcement, and it is rebuilt by
the same path that rebuilds any materialization.

A radius question has no answer. A deployment that needs one computes it outside the engine
and lands the result as rows, which means the result carries its own freshness rather than
the store's.

The hierarchy is operator-maintained. A place nobody added a parent for tags null and stays
readable, which keeps the row available and makes the gap quiet — a missing parent edge
reads as an unmapped row rather than as an error, so it is found by a maintainer looking at
null tags and not by a query failing.

Adding a geometry type later is additive rather than reversing: the closure stays useful for
containment, and the new type would sit beside it. What is expensive to reverse is the
opaque `place_id`, since every row already stamped with one carries no coordinate to
migrate from.

## Revisit triggers

- Containment-shaped workarounds appearing for a question that is genuinely metric — a
  deployment declaring artificial parent levels to approximate a radius.
- A place hierarchy deep or wide enough that closure materialization dominates the
  dimension's build, which changes the cost comparison the scope criterion rests on.
- Results computed outside the engine being landed and re-read often enough that their
  freshness, not their accuracy, becomes the complaint.
