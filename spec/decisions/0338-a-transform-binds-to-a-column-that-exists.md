# 0338 — A chain operation naming an absent column refuses rather than dropping out

**Status:** accepted 2026-09-18
**Decides:** `pipeline.transform.refusal.filter`

## Context

The transform chain is declared once at pipeline level, in the manifest, ahead of any pull. The
batch it rewrites is produced by a source and the normalize stage: columns come from a vendor's
payload, types are inferred over deferred-typing JSON, and a struct's flattening decides the
names the chain sees. Nothing in that path is fixed by the manifest, so the column set the
chain binds against is known only once rows are in hand.

A declaration can therefore name a column the batch does not carry, on two very different
paths. The author mistypes a name, and the manifest states a rule that has never once run. Or
the vendor stops sending a field — renamed, moved under a different parent, dropped from a plan
tier — and a rule that ran on every previous pull stops finding its operand.

The two operations at stake fail in opposite directions if they quietly drop out. A filter is
the one operation that removes rows: a filter that does not run lands every row it was written
to exclude, which is a widening of what enters the store, performed silently, at the one stage
whose declared purpose is to narrow. A cast rewrites a column's type in place and leaves its
name and position alone, precisely so a downstream projection reading it by name is unaffected;
a cast that does not run leaves that column under whatever type inference produced, and the
reader by name receives the type the manifest says has been rewritten.

Checking at declaration time does not reach either case. The manifest is validated before a
source is contacted, and the column set does not exist yet.

## Decision

A filter or a cast naming a column the incoming batch does not carry raises
`PipelineTransformColumnMissing`, printing the column and the table it was evaluated against.
The check runs where the batch is, so an operand that a vendor removed mid-life is caught on
the pull that first lacks it rather than on the pull that first depends on it.

## Options considered

| Option | What it buys | What it costs |
| --- | --- | --- |
| **Refuse at the batch, naming the column and the table** *(chosen)* | A declared rule either runs or stops the table, so the rows in the store are the rows the chain admits. | A vendor dropping one field halts a table that would otherwise land useful rows, and resuming is an edit to the manifest. |
| Skip an operation whose column is absent | Every pipeline keeps flowing across a schema change, with no operator action. | Lost on the direction of the error: a skipped filter lands exactly the rows it exists to exclude, and the widening is invisible in the manifest, in the run report and in the landed table. |
| Treat the absent column as null and evaluate against it | One rule covers both shapes; nothing halts. | Loses on meaning: the result is an artifact of the comparison rather than of the data — one predicate drops the whole batch and its negation keeps it — and neither outcome is what the author wrote. |
| Warn and continue, recording the skip on the run report | The skip is at least legible to someone reading the report. | Loses on the same direction, weakened only by a report nothing gates: the rows land, and the widening stands until a person reads a line. |
| Validate the chain against a declared table schema at manifest load | The typo is caught before any I/O, where the author is. | Loses on reach: the column set is produced by the source and the normalize stage, so a manifest-time check sees a schema the author wrote rather than the one the vendor sends, and drift passes it. |

## Criteria

1. **Direction of the error** — whether not running a declared operation withholds rows or
   admits them. *This criterion decides.*
2. **Where the column set is knowable** — what a check can see at the moment it runs.
3. **Blast radius** — how much of a fire one absent column stops.
4. **Availability under vendor drift** — whether a pipeline keeps landing rows when a payload
   changes.

Criterion 1 decides. The chain's two row-affecting operations are asymmetric: a filter that
silently does not run puts rows into the store that the manifest says are excluded, and a cast
that silently does not run hands a reader a type the manifest says it has. Criterion 2
eliminates the option that would have caught the typo earliest, since the shape being bound
against arrives with the data. Criterion 3 is answered outside this decision — a table-scoped
failure is what the per-table error setting governs — and criterion 4 is what is given up.

## Consequences

The manifest's chain is a statement about every row in the destination table: each declared
operation ran on each batch that landed, or the batch did not land. That is what lets a filter
be used to keep a category of rows out of the store, rather than being read as a best-effort
narrowing that a schema change can lift.

The cost accepted is availability under drift. A vendor renaming one field stops the table that
depends on it, and the pipeline resumes when the manifest names the new column. Under the
continuing table-error setting the failure stays confined to that table and the fire reports
it; under the halting default it stops the fire.

The refusal also fires per batch rather than per pipeline, so a source whose payload shape
varies between pages surfaces at the first page that lacks the column, part-way through a pull,
with the rows before it already committed.

## Revisit triggers

- Sources appear whose payloads legitimately vary field-by-field across pages, making a
  per-batch operand check the wrong granularity.
- A declared table schema becomes available ahead of the pull — a contract published by the
  source rather than written by the author — which would let the typo be caught at load without
  giving up the drift check.
- An optional-operand spelling is wanted often enough to be worth expressing, which would make
  absence a declared case rather than a refusal.
