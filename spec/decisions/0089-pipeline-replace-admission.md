# 0089 — The replacing write mode admits only a source that re-reads its entire input each pull

**Status:** accepted 2026-09-18
**Decides:** `pipeline.declare.refusal.write-mode`

## Context

A table declares one of two write modes. Under `append`, a run adds to what the table holds and
no key retires when it stops appearing. Under `replace`, the landing run *is* the table's whole
current state and every earlier commit stops being read. The read face honors that declaration
directly: a replacing table's view is the last committed run and nothing before it.

That makes `replace` a statement about the source, not about the table. It asserts that one pull
carries the source's complete current state. Whether that assertion holds is decided entirely by
how the pipeline reads: a source that re-reads its whole input every pull satisfies it, and a
source that reads a delta does not.

Every incremental shape carries a slice rather than a whole. A monotonic clock cursor fetches
rows at or past a committed position. A page-token cursor walks forward from where it stopped.
A backfill chunk plan deliberately partitions the input, each chunk committing an independent
part. A seed ceiling loads history below a cutover. In each case one landing run holds a
fraction of the source, and committing that fraction as a replacement retires every row outside
it.

The failure has no signal. A replacing table whose pull returned a window reads as a table that
shrank, and shrinking is exactly what a legitimate replacement of a smaller source looks like.
There is no count to compare against and no error to raise after the fact.

## Decision

`replace` declared beside a monotonic or page-token cursor, beside a backfill chunk plan, or
beside a seed ceiling raises `PipelineReplaceUnsupported`, naming the table each declaration
sits on. The refusal fires while the pipeline is validated, ahead of any row movement. A
version-id cursor is necessary and insufficient: what `replace` admits is a source re-reading
its entire input each pull, and a version-id declaration that still reads a delta is refused on
the same ground.

## Options considered

| Option | What it buys | What it costs |
| --- | --- | --- |
| **Admit `replace` only where the source re-reads its whole input; refuse the delta shapes by name** *(chosen)* | The mode's assertion is checked against the declaration that would falsify it, before a run. | A vendor whose full export arrives through a paged endpoint cannot express replacement, and the refusal lands at validation rather than at the declaration review that introduced it. |
| Admit `replace` on any source and document the hazard | Every combination is expressible; the author decides. | Loses on detectability: the deletion is silent and reads as a shrinking table, so the documentation is consulted only after the data is gone. |
| Admit a version-id cursor unconditionally as the delta shape that is safe | Covers change-data sources that carry a full row image. | Loses on the same ground: a version-id cursor over an ordered change log is a window like any other, so the cursor kind alone does not establish that the pull carried the whole input. |
| Refuse `replace` outright and fold at read time on a declared key | One write mode, one read rule, no admission question. | Loses on coverage: a re-exported file tree with no stable key is exactly the shape `replace` exists for, and a keyed fold cannot express deletion at all. |

## Criteria

1. **Whether the wrong combination is detectable after the fact.** *This is the criterion that
   decided it.* The other options all reduce to trusting the author, and the cost of a wrong
   declaration here is silent, unrecoverable deletion presented as a successful run. Where a
   mistake is both silent and destructive, the check belongs ahead of the first fire even at the
   price of refusing combinations that might have been fine.
2. **Whether the admission rule is checkable from the declaration alone**, without observing a
   pull. Cursor kind, chunk plan and seed ceiling are all declared, so it is.
3. **Coverage of the shapes that genuinely need replacement** — a full re-export, a dimension
   table restated each day.
4. **Where the refusal lands** — declaration review, validation, or first fire.

## Consequences

A replacing table's contents are the last run's rows, and that statement holds without a caveat
about which cursor produced them. An operator reading a manifest can determine, from the text,
whether a table can lose rows.

The cost accepted is expressiveness. A source whose complete export is paginated — common
enough among vendor bulk endpoints — cannot declare replacement even though each full walk does
carry the whole input, because the declaration cannot distinguish a full walk from a resumed
one. Those pipelines append and fold on a declared key, which cannot express a deletion, so a
key the vendor stops returning survives. The refusal also arrives at validation rather than
during review of the manifest edit, so the author learns about it one step later than ideal.

## Revisit triggers

- A declaration appears that distinguishes a complete paginated walk from a resumed one —
  for instance, a source reporting that a pull began at the input's origin and reached its end.
- Deletion becomes expressible under `append` through a per-connector retirement signal, which
  removes the main reason a pipeline reaches for `replace`.
- A version-id source is found whose every pull is provably a full row image, making the
  necessary-and-insufficient carve-out unnecessary.
