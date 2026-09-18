# 0054 — The relation vocabulary is closed per deployment, and an undeclared type is dead-lettered

**Status:** accepted 2026-09-18
**Decides:** `memory.declare.refusal.undeclared-relation`

## Context

Edges are explicit typed rows. Synthesis resolves both endpoints of a candidate edge and
writes one row carrying a `rel_type`; nothing infers an edge while a query runs. Traversal
is a recursive query over those rows in the language the read path already serves, and
almost every useful traversal filters on the type — "which parts", "which members", "what
this derives from".

A filter on a type is only as good as the caller's ability to enumerate the types. Under an
open vocabulary the caller cannot: a traversal filtering on `part_of` is silently incomplete
if some pass emitted `is_part_of`, `component_of` or `partOf` into the same column, and
nothing in the data says so.

The producer of candidate edges is a model. A model asked to name a relationship will name
one, and it will name a slightly different one next week under the same template. Left to
accumulate, the column holds a long tail of near-synonyms, each of which is a hole in
somebody's filter.

Candidates arrive in batches. One batch mixes edges whose types are known with edges whose
types are not, and the known ones are ordinary correct data.

## Decision

The relation vocabulary is a reserved core unioned with the types one deployment declares,
and that union is closed within the deployment. A candidate edge whose `rel_type` falls
outside the union lands in the dead-letter table and raises `MemoryUndeclaredRelation`. No
row is written under the unknown type, and the rest of the batch lands.

## Options considered

| Option | What it buys | What it costs |
| --- | --- | --- |
| **Reserved core plus a declared set, undeclared edges dead-lettered** *(chosen)* | A filter on `rel_type` is total, and the rejected edges are recoverable after a declaration edit. | Adding a relation type is a declaration edit, and edges rejected before that edit need a replay after it. |
| An open vocabulary | Nothing is ever rejected; a model's judgment about a new relationship is captured immediately. | Loses on filter reliability: no consumer can enumerate the types, so every type filter is silently partial and the column accumulates near-synonyms. |
| Auto-declare a new type on first use | Same capture as an open vocabulary, plus an enumerable set after the fact. | Loses on filter reliability the same way — the set grows by use, so any filter is incomplete the moment a later pass mints a type — and it lets a model mint the deployment's vocabulary. |
| Refuse the whole batch containing an undeclared type | One bad type is loudly visible; nothing lands half-processed. | Loses on throughput: one malformed candidate drops the correct edges beside it, and a model producing one novel type per batch stalls the pass entirely. |
| Drop the undeclared edge silently | Filters stay total with no dead-letter table to operate. | Loses on recoverability: the edge is unrecoverable after the declaration is fixed, and the operator is never told which type to declare. |

## Criteria

1. **Filter reliability** — whether a consumer filtering on `rel_type` can know its filter
   is complete. *This criterion decided it.* Traversal is the reason edges exist, type
   filtering is how traversal is written, and a filter whose completeness is unknowable
   makes every graph answer unauditable. Both open-vocabulary options fail it.
2. **Recoverability** — whether a rejected edge can be landed after the declaration is
   corrected. This chose dead-lettering over a silent drop, and it is what makes closing
   the vocabulary tolerable: the cost of a missing declaration is a delay, not a loss.
3. **Throughput** — whether one bad candidate costs the good candidates beside it. This
   chose per-edge rejection over per-batch refusal.
4. **Authorship of the vocabulary** — whether the deployment or the model decides what
   relationships exist. This reinforced the first criterion rather than deciding anything
   on its own.

## Consequences

Introducing a relation type is an explicit edit by the deployment, which is a slower loop
than discovering one from data. A pass that starts producing a genuinely useful new type
produces dead-letter rows until someone reads them and declares it.

Those dead-lettered edges are replayed after the declaration edit rather than landing
retroactively on their own. A deployment that never reads its dead-letter table
accumulates silently rejected edges and a graph that is quietly sparser than its sources
support — the dead-letter table is a queue somebody has to work.

The reserved core is fixed across deployments, so a type in the core cannot be redefined
locally to mean something else. A deployment whose domain uses one of those words
differently declares a distinct name rather than overloading the reserved one.

Declared relation types carry optional inverse and symmetric flags, and those flags are
hints to traversal over materialized rows. Nothing infers a row from them, so declaring a
type symmetric does not conjure the reverse edge.

## Revisit triggers

- The dead-letter table's undeclared-type rows are dominated by a small number of recurring
  types, which says the declaration loop is too slow for the rate at which the domain
  produces relationships.
- A consumer appears that does not filter on `rel_type` at all and is penalized by the
  rejection, meaning the closed set is buying a reliability nobody uses.
- Traversal gains a capability-based filter — matching on a declared flag rather than a
  name — at which point exact type enumeration stops being what completeness rests on.
