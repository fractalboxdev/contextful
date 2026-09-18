# 0095 — Nested Arrow is canonical and the relational shape is a reversible projection

**Status:** accepted 2026-09-18
**Decides:** `pipeline.normalize.shape.normalized-form`, `pipeline.normalize.refusal.list-index`, `pipeline.normalize.refusal.normalize-mode`

## Context

Sources hand over nested data. A vendor response is an object with sub-objects and lists of
sub-objects, and the store's tables are flat. Something between them decides which shape is
canonical — the one every stage after normalize operates on — and which is derived.

The two shapes convert asymmetrically. Nested to relational is an unnest: each list becomes a
child table joined by a foreign key, each struct becomes parent-child column names, and the
transformation is mechanical and lossless as long as each child row carries the index it held in
its parent's list. Relational to nested is an ordered aggregation, and it is lossy the moment
that index column is absent — the rows regroup, but their order inside the list is whatever the
scan returned, so a list of ranked results comes back permuted and nothing reports it.

Normalization is also the stage a connector does not participate in. Type inference over
deferred-typing JSON columns, struct flattening, list extraction and id assignment all run in
host code, and the shredding is a late projection applied at the sink — so a source landing in
two sinks passes the normalize stage once. Whichever shape is canonical is the shape held once
and projected many times.

## Decision

The canonical normalized form is nested Arrow structs and lists. Relational shredding is a late
projection applied at the sink: a struct flattens into parent-child column names, a list shreds
into a child table joined by a foreign key, and every child row carries its list index. A
relational projection that emits a child table carrying no list index raises
`PipelineListIndexMissing`, naming the parent and the list, since the index is what keeps the
projection reversible — the nested form is reconstructed by aggregating a child's rows in that
index's order.

## Options considered

| Option | What it buys | What it costs |
| --- | --- | --- |
| **Nested canonical, relational as a late reversible projection** *(chosen)* | One normalize pass per source regardless of sink count, and a projection that round-trips. | A flat-only sink pays a downgrade at write time, and the chain binds the root table alone, so a filter that gates out a root row still lands parentless children. |
| Relational as the storage default | Everything downstream sees flat tables; no sink has a capability question. | Loses on reversibility — reconstructing the nested form needs an ordered aggregation that is lossy without the index — and on cost, since a source landing in two sinks would normalize twice with no shared intermediate. |
| Store raw JSON and shred nothing | Nothing is lost, and the write path is trivial. | Loses on queryability: the type inference has to happen somewhere, and deferring it to read time means every query re-infers, and the sink is too late to emit a schema-diff event when a type moves. |
| Nested canonical with the list index optional | Fewer columns in child tables that nobody reconstructs. | Loses on reversibility, silently: the projection looks complete and the loss appears only when someone tries to rebuild the nested form and gets a permutation with no error. |

## Criteria

1. **Reversibility of the projection** — whether the derived shape can be turned back into the
   canonical one without loss. *This is the criterion that decided it.* The direction that is
   lossless is the direction that should be derived, and losing list order is the kind of loss
   that produces a plausible wrong answer rather than a failure. The optional-index variant is
   rejected on the same ground, which is why the missing index is a refusal rather than a
   warning.
2. **Normalize passes per source** — whether landing into two sinks costs one pass or two.
3. **Where type inference happens** — write time, once, or read time, repeatedly.
4. **Sink capability handling** — whether a flat-only sink is a downgrade with a reported
   schema-diff event or a hard failure.

## Consequences

A source is normalized once and projected per sink, and `native` preserves nesting up to the
sink's declared capability, exploding only the part the sink cannot hold and emitting a downgrade
schema-diff event rather than failing quietly. A child table is always reconstructible into the
list it came from.

The cost accepted has two parts. A flat-only sink pays the downgrade on every write, so the
cheapest path for a project that is relational everywhere is the path that does the most work.
And the chain binds the root table alone: a filter that gates out a root row still lands that
row's children, whose parent id then names an id no surviving row carries. That orphan is a real
consequence of projecting late rather than filtering after the shred, and nothing in the write
path removes it.

## Revisit triggers

- Orphaned children from root-level filtering become a reported data-quality problem rather than
  a known edge, which would argue for the filter binding the shredded set.
- A deployment is relational at every sink, making the nested intermediate pure overhead with no
  second sink to amortize it.
- Reconstruction of the nested form from the relational projection proves to be a path nobody
  takes, which would weaken the criterion that decided this.
