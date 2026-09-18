# 0147 — A parent row missing its key or its media value is skipped and counted rather than failed

**Status:** accepted 2026-09-18
**Decides:** `derive.select.refusal.incomplete-unit`

## Context

The scan turns parent rows into units. A unit needs two values off its row: the parent key
that will key the derived rows, and the media value the engine will be pointed at. Either can
be missing. A feed can publish an item with no enclosure; a connector can write a column that
its upstream left blank; an older row can predate the column entirely and read as null; a
publisher can write a whitespace string where an address belongs.

These rows are not rare and they are not transient. They sit in the parent table permanently,
and the scan meets them again on every tick, since the anti-join only removes parents that
already have a derived row and a skipped row has none.

The two obvious dispositions fail in opposite directions. Treating a malformed row as a run
failure hands one publisher's blank field the power to stop a pipeline that has thousands of
perfectly good units behind it — and because the row is permanent, the pipeline stays stopped
until a human edits the archive. Dropping the row with no record leaves a population of rows
that are never derived, with nothing anywhere that says how many there are: an operator
looking at a run record sees a plausible number of findings and no reason to suspect a
shortfall.

## Decision

A parent row whose `parent_id_column` or `media_column` holds null, nothing, or whitespace
raises `DeriveUnitIncomplete`. The row is skipped, it is counted, and the count reaches the
run record alongside the scanned row count and the already-derived count. The run continues
through the remaining units. No derived row and no marker row is written for the skipped
parent, so the row stays outstanding and is re-examined next tick.

A landed empty string reads as absence here, matching the treatment a connector that wrote no
column at all receives.

## Options considered

| Option | What it buys | What it costs |
| --- | --- | --- |
| **Skip the row, count it, report the count** *(chosen)* | One malformed row costs one row; the population is visible as a number on every run record | The row is re-scanned and re-skipped on every tick, and the count is the only signal that it exists |
| Fail the run on the first incomplete row | Impossible to overlook; forces the archive to be clean | Lost on availability: one blank field among thousands of good units stops the pipeline, and since the row is permanent the pipeline stays stopped until a human intervenes |
| Drop the row with no record | Cheapest, and the run record stays clean | Lost on trace: a population of never-derived rows becomes a silence with no count behind it, and a shortfall reads as a complete run |
| Write a marker row for the incomplete parent | The row settles, the anti-join retires it, no re-scan cost | Lost on truth: a marker states that the engine produced nothing for this unit, and no engine was ever called — a later fix to the parent row would find the unit already retired |

## Criteria

1. **Whether a malformed row can stall a run** — whether one bad value blocks work that is
   unrelated to it.
2. **Whether a row can disappear without trace** — whether the never-derived population is
   observable from the run record alone. **This criterion decided it.** Both failures are
   real, but a stall is loud: it announces itself on the first tick and an operator acts on
   it. A trace-free drop is a wrong number that looks right, and nothing in the system ever
   contradicts it.
3. **Whether a retired unit can be revived** — whether fixing the parent row later causes the
   unit to be derived.
4. **Cost of the disposition per tick** — what the scan spends on rows it will never derive.

## Consequences

A structurally broken row is re-scanned and re-skipped forever. On an archive with a large
permanently-blank population, every tick pays that scan and every run record carries a
non-zero skip count that never falls. That is the cost this decision accepts: a steady,
visible number rather than a silent zero.

What gets easier: fixing the parent row is all it takes. The next tick finds the row complete
and derives it, with no command to run and no retired state to clear.

What is now expensive to reverse: operators read the skipped-incomplete count as a standing
health number. Turning incomplete rows into markers later would make that number fall to zero
and look like a fix, when it would be a retirement.

## Revisit triggers

- The skipped-incomplete count on a typical deployment stops being a small fraction of the
  scanned count, so the re-scan cost dominates.
- The parent table gains a way to record that a row is known-incomplete at ingest, making the
  determination available before the derive scan runs.
