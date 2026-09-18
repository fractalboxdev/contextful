# 0145 — Deferred per-row work is a built-in source connector over the context store

**Status:** accepted 2026-09-18
**Decides:** `derive.select.refusal.required-key`, `derive.select.refusal.build-context`

## Context

Some rows land incomplete by nature. A podcast episode arrives as a title, a publication
instant and an enclosure address; the words inside the recording are not in the row and
cannot be, because getting them means an hour of inference against a model the archive does
not hold. A link lands as an address; the document's head facts sit behind a request to a
host a third party chose. In both shapes the ingest that produced the row was correct and
complete, and the useful text is still outside the process.

The arity of that follow-on work is the shape that constrains everything else. One enclosure
becomes roughly ninety citable passages, each with its own interval and its own text. One
landed link becomes one head-fact row plus a handful of picture candidates. The operation
adds rows keyed to a parent; it does not rewrite the parent.

The run path already carries everything this work needs and did not want to write twice: a
lease that keeps two machines off one pipeline, a write guard, redaction rules applied
before bytes reach a snapshot, schema reconcile, per-batch durability, an atomic commit, a
cursor saved after durability, a run record and a run audit. Whatever carries derive work
either inherits that spine or rebuilds it.

The work also has to be able to revisit a row that already landed. An archive pulled last
year holds thousands of enclosures with no transcript beside them; the tier has to be able
to walk back into them, which rules out any position whose only view of the data is a
forward cursor over what is arriving now.

## Decision

`derive` is a built-in source connector whose input is the context store rather than the
outside world. A `[[pipeline]]` sets `connector = "derive"`, points it at a landed table and
a column holding media, and lands its answer through the ordinary write path into a table
of its own. The source is built with a store root and the identifier of the pipeline it
belongs to; built without either one it raises `DeriveNoStoreRoot` naming which of the two
is absent, rather than answering with an empty outstanding list. `engine`, `source_table`,
`media_column` and `parent_id_column` are present and non-empty at build, and an absent or
blank one raises `DeriveConfigKeyMissing` naming the key and the pipeline.

The build-time refusals exist because the failure they replace is silent. A source with no
store root reads zero rows and reports zero outstanding units, which on a run record is
indistinguishable from an archive where every enclosure already has its transcript.

## Options considered

| Option | What it buys | What it costs |
| --- | --- | --- |
| **A built-in source connector reading the store** *(chosen)* | Row-adding arity matches what a source does; full view of the landed table, so a row from any age is reachable; lease, cursor, redaction, schema reconcile, commit and retry all inherited | The tier appends into its own table alone and can fill no column on the parent row |
| A transform operation in the batch algebra | Sits directly in the write path with no second table | Lost on arity: select, rename, cast and filter rewrite a batch in place and add no rows, while one unit here becomes roughly ninety |
| A stage inside the pull that produced the parent | One pipeline, one pass, no join for the consumer | Lost on revisiting: a connector reads from a cursor and advances it, so a row that landed without its text stays that way forever |
| A dedicated job kind beside the pipeline | Free shape, no source contract to satisfy | Lost on re-implementation: lease, cursor, secret guard, redaction, schema reconcile, commit and retry are all rebuilt outside the spine that already provides them |
| A guest component in a second sandboxed world | Strong isolation of vendor code | Lost on bounds: per-call deadlines an order of magnitude below a transcription, a hard memory cap against the base64 of an hour of audio, and no filesystem for an engine that requires a PCM file |

## Criteria

1. **Arity of the operation** — whether the position can add rows keyed to a parent, or only
   rewrite the batch in front of it. **This criterion decided it.** Arity is not negotiable
   by effort: a position that rewrites in place cannot express one-becomes-ninety at all, so
   every other comparison is moot until a candidate survives this one.
2. **Reach back into landed data** — whether a row that landed before the tier existed can
   still be picked up.
3. **Re-implementation of the run-path spine** — how much of lease, cursor, redaction,
   schema reconcile, commit and retry a candidate rebuilds.
4. **Fit of the execution bounds** — whether the position's deadlines, memory ceiling and
   filesystem access match an hour-long inference call.

## Consequences

Derived text lives in its own table, keyed to the parent. A consumer reading a document
beside its derived passages joins two tables on the parent key; no null column on the parent
row is ever filled in later, and no reader gets the joined shape for free. That is the cost
this decision accepts, and it is permanent in the data: rows already written carry the split.

What gets easier: every future modality is a new engine behind an existing source, not a new
job kind. The tier states no durability, ordering or redaction rule of its own, so a change
to the write path reaches derive without an edit here.

Reversing the split — moving derived text back onto the parent row — is expensive. It needs
an in-place update path the write path does not have, and a rewrite of every derived table
already committed.

## Revisit triggers

- The write path gains an in-place row update that preserves snapshot semantics.
- A derive modality arrives whose arity is one-to-one, so the join buys nothing.
- The in-memory scan over a parent table stops fitting and the source needs a query engine
  behind it, which changes what a source is allowed to do.
