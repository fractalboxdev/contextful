# 0093 — Control flow is declared, not data-dependent

**Status:** accepted 2026-09-18
**Decides:** `pipeline.compile.refusal.predicate`

## Context

A plan is a flat node list with explicit predecessor edges. `branch` holds a predicate and a
label-to-node-id map; `parallel` holds a list of node ids run concurrently; children are reached
by id rather than nested, which leaves validation and lowering as non-recursive walks. Every arm
a run can take is therefore present in the plan before the run opens.

That shape is what a plan is for. An operator reads a compiled plan to learn what a fire will do,
a version is the hash over its canonically sorted nodes so one plan always prints one version,
and a diagram lowering renders the same graph for a document. A plan whose graph is derived at
runtime supports none of that: the artifact would list the nodes reached so far rather than the
nodes reachable.

Lowering constrains it a second time, and more tightly. One plan lowers node by node onto a
durable substrate — a step onto a durable step call, a sleep onto a durable timer or a wait
state, a branch onto a choice over the declared predicate, a fan-out onto concurrent steps or a
graph fan-out. Every substrate that offers a choice state names its arms statically. A conditional
whose targets are computed at execution time has no image under that lowering, so admitting one
would mean admitting a plan that lowers onto some targets and not others.

## Decision

A data-dependent conditional or loop in a workflow body raises `PipelineUndeclaredControlFlow`.
Branching travels as `branch` over a declared predicate, fan-out as `parallel`, and iteration as
a map, each of which the compiler reads. The predicate is evaluated at execution time and the
data decides which declared arm is taken; what the data does not decide is which arms exist.

## Options considered

| Option | What it buys | What it costs |
| --- | --- | --- |
| **Declared `branch`, `parallel` and map, over predicates the compiler reads** *(chosen)* | The compiled plan is the complete graph, inspectable before a fire and lowerable onto every substrate. | A branch whose arms are not known at compile time is expressed inside one connector, which moves it out of the graph a reader inspects. |
| Data-dependent control flow with the plan derived at runtime | Any shape is expressible; the author writes ordinary control flow. | Loses on inspectability — there is nothing to read before the run — and on lowering, since a substrate's choice state names its arms statically, so the plan would be portable to no target that offers one. |
| A general effect system as the authoring surface | Composition and typed effects, with control flow tracked by the type system. | Loses on the same two grounds, since the graph is still a function of execution, and adds a runtime to embed — the footprint already rejected for step bodies. |
| Static arms plus a dynamic dispatch node resolving a target by name at runtime | Covers plugin-style dispatch while keeping most of the plan readable. | Loses on inspectability at exactly the interesting node: the reader sees that a choice happens and not what it can choose, and the version hash stops describing the reachable graph. |

## Criteria

1. **Whether the emitted plan is inspectable ahead of execution** — whether reading the artifact
   answers what the fire can do. *This is the criterion that decided it.* A plan's value comes
   from being reviewable, diffable and hashable before anything runs; a graph that exists only
   after execution is a trace, and a trace answers a different question than a plan does.
2. **Whether lowering onto a durable substrate is total** — whether every plan a compiler accepts
   has an image on every supported target.
3. **Stability of the plan version** — whether one specification prints one version. A runtime
   graph has no single hash.
4. **Expressive reach** — the shapes an author can write directly. This is the criterion the
   chosen option loses on, and the connector carve-out is what pays for it.

## Consequences

A plan diff is a meaningful review artifact, and a version change means the graph changed. The
diagram lowering is faithful rather than approximate, since the nodes it draws are the nodes that
can run. Adding a substrate is a matter of mapping five node kinds.

The cost accepted is that dynamic dispatch disappears from view. Where the arms genuinely are not
known until the data arrives, the whole decision moves inside one connector, and the graph then
shows a single step where the interesting structure is — which is precisely the inspectability
the decision was made to protect, lost in the case that needed it most. How often that case
arises is unmeasured.

## Revisit triggers

- Connectors accumulate internal branching that a reader needs and cannot see, making the graph
  systematically less informative than the code it names.
- A substrate becomes the primary target whose choice state accepts dynamically named arms, which
  removes the lowering objection for that target and makes the restriction a portability choice
  rather than a correctness one.
- Plan review stops being a practice anyone performs, which would undercut the criterion that
  decided this.
