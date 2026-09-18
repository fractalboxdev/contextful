# 0053 — A table naming a memory shape is validated against that shape's canonical columns when the declaration loads

**Status:** accepted 2026-09-18
**Decides:** `memory.declare.refusal.canonical-column`

## Context

Synthesized memory is five table shapes over the ordinary store substrate. A table names
`shape` beside its columns, and that one word does two things: it binds the schema
validator to the shape's canonical column set, and it tells the tool surface which ranking
defaults to publish for the table.

The canonical columns are not decoration. The revision rule reads a subject, a predicate
and a scope; the fold reads a deduplication key; liveness reads a declared validity pair;
recall orders on a tier and a score; the evidence gate reads row identifiers. A claim table
missing any one of those has a revision rule that cannot run, a liveness predicate that
cannot resolve, or a recall ordering with nothing to order on.

A declaration is loaded when a deployment starts, and it is loaded before any write and
before any read. A missing column is therefore knowable at that moment, from the
declaration alone, with no data and no traffic.

The alternative moments are worse in a specific way: at first write the deployment is
already running and the defect arrives as a failed pipeline; at first read it arrives as an
answer, and the tool surface has by then already published ranking defaults for a shape it
cannot honor.

## Decision

A table naming a shape and omitting a canonical column of that shape raises
`MemoryShapeColumnMissing` when the declaration loads. The deployment does not start
against that declaration, no column is filled in on the table's behalf, and the tool
surface publishes ranking defaults only for shapes whose tables carry the columns those
defaults read.

## Options considered

| Option | What it buys | What it costs |
| --- | --- | --- |
| **Refuse at declaration load** *(chosen)* | The defect is visible with no data, no traffic and no partial state; the tool surface never advertises a shape it cannot serve. | A deployment adding a column to a shape re-validates every table declaring it, and a table part-way through an evolution does not load. |
| Validate at the first write | The deployment starts; only the pipelines that touch the table are affected. | Loses on signal latency: the deployment is already running and serving, so the defect arrives as a runtime failure in a component that did nothing wrong. |
| Fill a missing canonical column with a default | Nothing fails; the shape's machinery has something to read. | Loses on meaning: a canonical column holding an invented value is indistinguishable from data, so a fabricated scope or validity bound enters the revision rule as if the deployment had asserted it. |
| Warn at load and serve the table as shapeless | The table stays readable as an ordinary table; the operator is told. | Loses on signal latency in a subtler form — a warning at start is a line in a log, and the tool surface has already decided what to publish. |
| Drop the shape's defaults and keep the machinery | The ranking surface degrades rather than refusing. | Loses on meaning: the revision rule and the liveness predicate are not defaults, and neither can be dropped without changing what the rows mean. |

## Criteria

1. **Signal latency** — how far the defect travels from the edit that caused it before
   anyone sees it. *This criterion decided it, together with the second.* A declaration is
   an authored artifact; the whole point of checking it is to fail on the edit rather than
   on the traffic.
2. **Meaning preservation** — whether the fix invents data. This eliminated defaulting
   outright: a canonical column exists to carry an assertion the deployment made, and a
   filled-in value is an assertion nobody made.
3. **Startup availability** — whether one bad table stops a deployment. The chosen option
   scores worst; this is the cost accepted, and it is bounded because the failing artifact
   is the declaration the operator just edited.

## Consequences

Adding a canonical column to a shape is a breaking change for every table declaring that
shape, and every such table is re-validated at the next load. That is the intended
behavior — the alternative is a fleet of tables that claim a shape they no longer fit — but
it means shape evolution is a coordinated edit rather than an incremental one.

A table cannot be evolved in place through a state where it declares the new shape and
carries the old columns. The migration path is to land the columns first and the
declaration second.

The refusal is per-declaration, not per-deployment-partition, so one malformed table stops
the load even where every other table is correct. Whether that is too blunt at fleet scale
is unmeasured; no deployment has yet declared enough memory tables for the blast radius to
be observed.

## Revisit triggers

- Shape evolution becomes frequent enough that coordinated column-then-declaration edits
  are a routine operational burden rather than a rare one.
- A deployment declares memory shapes across enough independently-owned tables that one
  team's malformed declaration stops another team's serving.
- A canonical column appears whose absence degrades a ranking default without breaking the
  revision rule, the fold or liveness — that column is a candidate for a warning rather
  than a refusal.
