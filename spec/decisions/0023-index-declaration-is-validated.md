# 0023 — An index declared over an absent column is refused at manifest validation

**Status:** accepted 2026-09-18
**Decides:** `store.index.refusal.index-column-absent`

## Context

Each sidecar is declared per table in the manifest with a kind, a column and its builder
parameters. The declaration is authored text; the column it names exists only if the
table's reconciled schema carries it. Those two facts are maintained by different people at
different times, so a declaration naming a column the schema lacks is an ordinary
authoring outcome — a rename at the source, a column that has not arrived yet, a typo.

Where that mismatch is discovered decides what it costs. A pass builds sidecars after it
has staged Parquet, so discovering the missing column mid-build means the staged data is
written and thrown away, and the whole fold fails on a manifest typo. Publication is
all-or-nothing, so there is no partial outcome to keep.

Tolerating the mismatch is worse than expensive. An unbuildable index that is skipped and
logged leaves retrieval degraded with no signal a reader can see: the query still runs,
falls back to a scan or to another candidate source, and returns a shorter answer that
looks like a correct one. Creating the named column as null and indexing it is the same
failure with a success report attached — the index exists, the build reports rows indexed,
and every probe against it matches nothing.

Manifest validation is the one moment where the schema is known, the declaration is known,
and nothing has been written.

## Decision

Declaring an index over a column the table's reconciled schema does not carry raises
`StoreIndexColumnAbsent` at manifest validation, ahead of the pass that would build it. The
declaration is checked against the reconciled schema, so a pass that starts is a pass whose
declared sidecars are buildable.

## Options considered

| Option | What it buys | What it costs |
| --- | --- | --- |
| **Refuse at manifest validation** *(chosen)* | No staged Parquet is discarded, and every declared sidecar in a running pass is buildable | A table whose indexed column arrives from a source later declares the index after the column exists rather than ahead of it |
| Skip an unbuildable index and log it | The pass always completes; a not-yet-present column is tolerated | Lost on signal: a silently absent index degrades retrieval with no artifact a reader can see, and the query returns a shorter answer that reads as a correct one |
| Create the named column as null and index it | The declaration always applies; the sidecar always exists | Lost on signal, with a success report on top: the index answers every probe with nothing while the build reports success |
| Discover it during the pass and fail there | No validation step to maintain | Lost on where the cost lands: staged Parquet has already been written and is discarded, and the whole fold fails on a manifest typo at the least-watched moment |

## Criteria

1. **Where the cost of the mismatch lands** — how much work is done before it is known.
   *(the one that decided it)* A pass that discovers the missing column mid-build has
   already written staged Parquet, and an all-or-nothing publication means the entire fold
   is lost to a manifest typo. Tolerance of a declaration ahead of its column was the
   competing criterion and lost, because it is an authoring-order inconvenience with a
   one-line fix, while the tolerant options replace an error with a degraded retrieval that
   reports success.
2. **Visibility of a degraded retrieval** — whether a reader can tell an index is missing.
3. **Honesty of a build report** — whether "built" means "usable".
4. **Declaration ergonomics** — whether an author can state intent before the column
   exists.

## Consequences

A manifest that validates is one whose sidecars can be built, so a pass fails on data
problems rather than on declaration problems. A typo names itself at the moment it is
introduced, in the file where it was written.

The cost accepted is ordering friction between a schema and its indexes. A table whose
indexed column arrives from a source only after ingestion begins declares its index in a
second change, once the column exists — so the manifest cannot express the intended end
state up front, and an author tracks the follow-up themselves.

Validation now depends on the reconciled schema being current. A declaration valid against
today's schema becomes invalid if the source stops emitting the column and the schema is
re-reconciled without it, which turns a source-side change into a manifest refusal.

## Revisit triggers

- A source pattern in common use introduces indexed columns only after first ingest, making
  deferred declaration the norm rather than the exception.
- A pass gains the ability to publish the data half and retry the sidecar without a partial
  snapshot ever being readable, which changes what discovering the problem mid-build costs.
- Declarations are commonly authored against a schema the deployment has not yet
  reconciled, making the validation check unavailable at the moment the manifest is
  written.
