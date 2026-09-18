# 0094 — The chain rewrites a batch in place and never grows it

**Status:** accepted 2026-09-18
**Decides:** `pipeline.transform.refusal.transform-chain`

## Context

The transform chain is an ordered list of four operations declared once at pipeline level:
select, rename, cast, and a filter over a single column. Each rewrites a batch in place — the
rows leaving the chain are the rows that entered it, modulo the one operation that drops rows by
predicate. None of the four adds a row.

That arity is not incidental. It is what lets the chain sit between normalize and the write with
no coordination: identity columns are already injected, the row id is a hash over the row's own
content, and a chain that neither adds nor reorders rows leaves those ids meaningful. It is also
what lets the chain bind the root table alone and let a shredded child pass through untouched.

The pressure to widen it comes from per-row enrichment. Transcribing an audio attachment into
segments, extracting entities from a document, reading an image into described regions — each
consumes one row and produces many. Placing that work in the chain does not add an operation to
a list of four; it changes what the chain is. Every downstream statement about the batch — that
its rows are the rows that entered, that a filter's only effect is dropping, that a child's
parent id names a row in the parent batch — is stated over the old arity.

Latency is the obvious complaint about such work and it is the wrong one. A slow operation
inside the chain is a slow pull; that is a scheduling problem with known answers. A
row-multiplying operation is a shape problem, and shape does not yield to scheduling.

## Decision

A chain operation that would emit more rows than it consumed raises `PipelineTransformArity`.
Deferred, vendor-mediated work whose output count exceeds its input count reads landed rows
through a separate tier, which pulls from the store rather than from a vendor mid-pull and
writes its output back through the ordinary destination as queryable corpus.

## Options considered

| Option | What it buys | What it costs |
| --- | --- | --- |
| **Fixed arity in the chain; row-producing work reads landed rows in its own tier** *(chosen)* | The chain's guarantees hold as stated, and enrichment gets its own cadence, its own retries and its own failure accounting. | A row landed without its derived text is unreachable until the derive tier runs, so an operator reasons about two cadences rather than one. |
| A transform interface with a model-endpoint capability, inside the chain | Enrichment declared where the rest of the batch shaping is declared. | Loses on arity: the algebra would have to change anyway, since the operation produces rows, so the chain's in-place statement would be false for every pipeline whether or not it used the capability. |
| An in-source vision or speech runtime performing the expansion during the pull | No second tier; the rows land complete. | Loses on deployment profile, since it pulls a system dependency into every image, and on arity all the same — the expansion still happens inside the pull. |
| Allow row growth in the chain and restate the guarantees around it | One place for all per-row work. | Loses on what the restatement costs: the filter's meaning, the child's parent binding and the content-hash identity of a produced row are each newly undefined, and each needs its own rule. |

## Criteria

1. **Arity rather than asynchrony** — whether the operation changes the chain's algebra or merely
   its duration. *This is the criterion that decided it.* A slow operation is a scheduling
   question; a row-producing one is a different algebra, and admitting it would rewrite every
   downstream statement about a batch. The latency argument that usually drives this kind of
   decision is the weaker one here and was not allowed to carry it.
2. **Deployment profile** — what a build carries for a capability most pipelines do not use.
3. **Failure accounting** — whether a vendor's failure during enrichment is separable from a
   source's failure during a pull. A separate tier separates them; an in-chain call does not.
4. **Operator simplicity** — how many cadences produce a complete table. This is the criterion
   the chosen option loses on.

## Consequences

The chain stays four operations that a reader can hold in mind, and the identity columns injected
upstream stay meaningful through it. Enrichment gains what it needs anyway: its own retry policy
against a vendor with its own rate limits, and a failure that does not fail a pull.

The cost accepted is a visible one. Between a landing run and the derive tier's pass, the table
holds rows whose derived columns are absent, and any read in that window returns them. An
operator explaining why a document has no extracted text explains two cadences. Nothing in the
read path currently marks a row as awaiting derivation, so the window is invisible to a consumer
until the derived values appear.

## Revisit triggers

- The two-cadence window becomes the common complaint from readers of the store, rather than an
  occasional one.
- A row-producing operation appears that is neither vendor-mediated nor slow — a pure local
  explode — for which the deferred tier is all cost and no benefit.
- The read path gains a way to mark a row as pending derivation, which would make the window
  legible and change what the cost actually is.
