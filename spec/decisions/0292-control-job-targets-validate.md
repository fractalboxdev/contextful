# 0292 — A job target binds at the manifest or the manifest is refused

**Status:** accepted 2026-09-18
**Decides:** `control.fire.refusal.compact-target`, `control.fire.refusal.build-target`

## Context

Two maintenance kinds name what they act on. A `compact` job carries a target naming an
on-disk table, spelled `<pipeline>_<table>` with every non-alphanumeric character in either
half folded to `_`; omitting the target covers every table declaring a primary key. A
`build` job carries a target naming a declared model, and omitting it covers every model.
In both cases the target is a string an operator writes that has to bind to something a
pipeline actually produces.

The folding rule is where this goes wrong in practice. An operator reads a pipeline id and
a table name out of the document and writes them with the separator they see, and the
on-disk spelling is the folded one. A target off by one character binds to nothing.

Neither kind has a run-time symptom when its target binds to nothing. A compaction with no
matching table has no rows to fold and completes; a build with no matching model has
nothing to publish and completes. Both report success. The job's watermark advances. The
table that should have been folded keeps accumulating small files, and the model that
should have been published keeps a watermark that stands still while a correctly wired
consumer concludes there is nothing new. Nothing in the deployment's health, its logs at
normal verbosity, or its run history distinguishes this from a deployment where the work is
happening and there is nothing to do.

The document is validated when it is applied, and validation is the one moment where an
operator is present, holding the text they just wrote, with the pipeline declarations in
the same document available to check against.

## Decision

A `compact` target that binds to no produced table, and a `build` target naming no declared
model, raise `JobTargetUnbound` in the same validation pass that reads the manifest, before
the block joins any armed set. The document is refused rather than applied with a job that
would fire forever against nothing. A target omitted on either kind is legal and means the
kind's full scope — every table declaring a primary key, every declared model — so the
refusal falls on a named target that resolves to nothing rather than on the absence of a
name. Beside this, a document publishing a model with no enabled `build` job covering it
draws a validation warning per model.

## Options considered

| Option | What it buys | What it costs |
| --- | --- | --- |
| **Refuse at manifest validation** *(chosen)* | The error lands while the operator is holding the text, with the declarations to compare against in the same document. | A manifest naming a table its pipeline has not produced refuses, so a document and the store it describes have to move together and a document cannot be applied ahead of the pipeline it anticipates. |
| Fail at fire time | The document applies immediately; the check sees the real store rather than the declarations. | Lost on absence of a run-time symptom: the job reports a failure per cadence into a log nobody reads between cadences, and the table it should have folded is never named at a moment anyone is looking. |
| Warn and continue | Nothing is blocked; the diagnostic still exists. | Lost on the same criterion, more severely: the deployment reads as healthy, every job succeeds, and the unfolded table and the frozen published watermark are discovered by their downstream effects. |
| Bind targets by search at fire time, folding on both sides | An operator's unfolded spelling resolves, removing the most common authoring error. | Lost on determinism: a fuzzy bind makes two different targets resolve to one table, and a target that resolves differently after a new pipeline lands is worse than one that never resolved. |

## Criteria

1. **Presence of a run-time symptom** — whether the failure announces itself at all once
   the document is applied. **This criterion decided.** Both kinds complete successfully
   with nothing to do, so there is no signal to route, no failure to surface and no
   status to degrade; validation is the only place the problem is observable, which makes
   the comparison between validation and run time not a comparison of two checks but a
   choice between one check and none.
2. **Distance from the error to the person who made it** — validation runs with the
   operator present and the declarations in hand.
3. **Determinism of the bind** — whether one target string resolves to one artifact, stably,
   as the document grows.
4. **Independence of document and store** — whether a document can be applied ahead of the
   pipeline it describes. The chosen option gives this up.

## Consequences

An applied document is one where every named maintenance target resolves, so a deployment's
maintenance cadence is known to be doing work rather than known to be firing. The folding
rule becomes checkable rather than a convention an operator has to reproduce from memory.
The related warning for a published model with no covering build job closes the other half
of the same silence, where the job is absent rather than misaddressed.

The cost accepted: the document and the store move together. An operator cannot apply a
document that anticipates a pipeline landing later, and a compaction job cannot be staged
ahead of the table it will fold. Renaming a pipeline or a table breaks every job block
naming it, and the document is refused until all of them are updated in the same edit —
correct, and a stop in the middle of a rename. A deployment whose pipelines are generated
and whose maintenance blocks are authored by hand feels this on every generation.

## Revisit triggers

- Documents are routinely applied in a staged sequence where a job legitimately precedes
  the pipeline it targets, making validation-time binding an obstacle rather than a check.
- A run-time symptom for an unbound target appears — a job that can report it folded zero
  tables against a target it was given — which would make fire-time detection visible.
- Target spellings acquire a canonical form the operator writes directly, removing the
  folding step that produces most unbound targets.
