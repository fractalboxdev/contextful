# 0088 — A destination table name is derived from the pipeline id and the table name, never declared

**Status:** accepted 2026-09-18
**Decides:** `pipeline.declare.invariant.table-name`, `pipeline.declare.refusal.table-name`

## Context

Several things in a manifest name a physical table without landing rows into it. A compaction or
publish job names the table it covers. A model reference names the table it reads. A read
surface resolves a table by name on behalf of an agent. None of those declarations create the
table; the landing run does.

That leaves a binding question. The pipeline declares an id and a set of table names; the store
holds one flat namespace of physical tables across every pipeline in the project. Two pipelines
may each declare a table called `issues`, and they are not the same table. So the physical name
carries both coordinates: `<pipeline id>_<table name>`, with every non-alphanumeric character
folded to `_` and every ASCII uppercase letter lowered, so pipeline `meta-ads` and table
`insights` bind `meta_ads_insights`.

The fold is not injective. `meta-ads`, `meta.ads` and `Meta_Ads` all fold to `meta_ads`. That
property is what makes accepting arbitrary spellings dangerous rather than merely lenient: if a
reference is accepted whenever it folds onto the derived name, two distinct spellings in two
files bind one physical table, and a reader comparing the two files sees two names and assumes
two tables.

The other pressure is timing. A job target or a model reference is validated at plan time, before
any run has created anything. Validation can only compare the written spelling against the
spelling the fold produces — there is no catalog entry to look up yet.

## Decision

A destination table's name is derived, not declared. The physical name is the fold over the
pipeline id and the table name, and no key in the specification overrides it. A job target or a
model reference naming a destination table in a spelling the fold does not produce raises
`PipelineUnboundTableName` and prints the spelling the fold expected, at plan and validate time
rather than at first read.

## Options considered

| Option | What it buys | What it costs |
| --- | --- | --- |
| **Derive the name; refuse any other spelling** *(chosen)* | Exactly one physical name per (pipeline, table) pair, computable before anything exists, so every reference validates ahead of a run. | A pipeline rename re-keys every landed table, and an operator reading a job target applies the fold mentally to find the pipeline it belongs to. |
| Optional explicit name overriding the derivation | An operator adopts an existing physical layout, and a rename leaves storage untouched. | Loses on disagreement: a job target written against the derived name and a model reference written against the override bind different tables from the same manifest, and nothing in the file shows they disagree. |
| Accept any spelling that folds onto the derived name | Authoring is forgiving; a hyphen or a capital is not an error. | Loses on validation: the fold is not injective, so two written spellings bind one table and a reader cannot tell from the text whether two references are the same reference. |
| Resolve references against the catalog at first read | The written spelling is checked against what actually exists. | Loses on timing: a reference to a table no run has created yet cannot be validated at all, so a typo in a job target surfaces at the first fire rather than at plan. |

## Criteria

1. **Whether a reference validates before a row moves** — whether a typo is caught at plan time.
   *This is the criterion that decided it.* A manifest is edited far more often than it is run
   against fresh storage, and the failures worth preventing are the ones where a job quietly
   covers nothing. Catalog-time resolution cannot reach that, and the forgiving spelling rule
   validates a reference into ambiguity rather than out of it.
2. **Whether two declarations can disagree about one physical table** — whether the manifest
   admits a state where the same table has two names in it.
3. **Namespace collision across pipelines** — whether two pipelines declaring the same table name
   can collide. Derivation settles this; an override reopens it.
4. **Operator legibility** — whether a human reading a physical name knows which pipeline owns
   it. Derivation scores here too, in the forward direction.

## Consequences

Every reference in the manifest is checkable with nothing but the manifest, and the error prints
the expected spelling, so the fix is mechanical. Table ownership is readable from the physical
name, which makes an orphaned table in the store attributable to the pipeline that abandoned it.

The cost accepted is rename rigidity. Changing a pipeline id re-keys every table it owns, and the
already-landed data sits under the old names — the rename is a data move, not an edit. There is
no supported path for adopting a table whose physical name was chosen elsewhere; something
outside the pipeline writes into that layout. The mental fold an operator applies when reading a
job target is a small, permanent tax.

## Revisit triggers

- Renaming a pipeline becomes a routine operation rather than a rare one, which makes the re-key
  cost recurring instead of incidental.
- A demand appears to land into a physical layout owned by another team's tooling, which is the
  case the override was rejected for.
- The fold is shown to collide in practice between two pipelines whose ids differ only in
  separators, which would make the derivation itself the source of ambiguity it was chosen to
  prevent.
