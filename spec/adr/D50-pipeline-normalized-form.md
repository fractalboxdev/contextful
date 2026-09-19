# D50 — Nested Arrow is canonical, and the relational shape is a reversible projection

**Status:** accepted

## Context

Sources hand over nested data; tables are flat. Nested to relational is a lossless unnest when each child row carries its list index. Relational to nested is an ordered aggregation that silently permutes a list the moment that index is absent. Normalization runs once in host code, whatever the number of sinks.

## Decision

`run.normalize` holds the canonical form as nested Arrow structs and lists. Relational shredding is a late projection at the sink: a struct flattens into parent-child column names, a list shreds into a child table joined by a foreign key, and every child row carries its list index. A child table emitted without that index raises `PipelineListIndexMissing`, naming the parent and the list.

## Options

| Option | Lost on | Cost |
| --- | --- | --- |
| Nested canonical, relational as a late reversible projection *(chosen)* | — | A flat-only sink pays a downgrade on every write; a filter binding the root table lands parentless children. |
| Relational as the storage default | Reversibility | Rebuilding the nested form would permute lists, and two sinks would normalize twice. |
| Raw JSON, shred nothing | Queryability | Every query would re-infer types, too late to emit a schema-diff event. |
| Nested canonical with an optional list index | Reversibility | The loss would surface only as a permutation with no error. |

## Consequences

- A source normalizes once and projects per sink; `native` keeps nesting up to the sink's declared capability and emits a downgrade schema-diff event past it.
- A child table always reconstructs into the list it came from.
- A relational-everywhere project takes the path doing the most work.

## Revisit

- Orphaned children from root-level filtering become a reported data-quality problem.
- A deployment is relational at every sink, making the nested intermediate pure overhead.
