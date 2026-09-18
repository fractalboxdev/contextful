# 0165 — Per-unit failure state lives in the output table as a marker row

**Status:** accepted 2026-09-18
**Decides:** `derive.land.refusal.failure-off-the-table`

## Context

The derive tier recomputes its work list every tick. The cursor is snapshot-shaped and carries no
position, so there is no saved offset that advances past a unit: a parent row is outstanding when
it lacks its derived output at the instant of the scan, and the source establishes that by reading
the table it writes and dropping every parent it already holds a row for. Membership in the output
table is the entire definition of done.

That construction has one consequence that governs everything else. A unit which produced nothing
and left nothing in the output table is, by the scan's own definition, still outstanding — forever,
and at full cost. Worse, the loop is silent and self-sustaining. A run whose every unit fails
emits no batch; with no batch there is no commit; with no commit the cursor is re-saved unchanged;
and the next tick recomputes the identical outstanding list and re-attempts the identical units.
Nothing about that cycle is distinguishable from a healthy pipeline with a slow source.

The other half is inspectability. The store's whole read face is SQL over landed tables. State
kept anywhere else — a sidecar file, a queue, a ring of cursors — is state an operator cannot
join, cannot count and cannot filter alongside the content it describes, and is state that has to
be made durable and crash-consistent a second time, against the same commit boundary the write
path already provides.

A sibling table would satisfy inspectability. It does not satisfy the loop, because the anti-join
reads one table: the one the pipeline writes. Markers in a neighbour are invisible to the thing
that decides what is outstanding.

## Decision

Per-unit failure state lives in the output table. A unit that produced nothing lands one marker
row at `cue_seq = -1` carrying `unit_status`, `attempts`, `last_error` and `retryable`, with the
content columns null. The anti-join reads markers as it reads content rows, so a settled unit
leaves the outstanding set and a population of undone work is a row count rather than a silence.
Per-unit failure state recorded anywhere other than the output table raises
`DeriveFailureOffTable`.

## Options considered

| Option | What it buys | What it costs |
| --- | --- | --- |
| **A marker row in the output table, visible to the anti-join** *(chosen)* | The loop terminates; failure is queryable beside content; one durability path | The output table mixes content rows and failure rows, so every consumer filters |
| A dead-letter queue | Familiar shape; failures segregated from content | Loses on all three criteria: a second storage shape to restart-proof, unreachable by SQL, and invisible to the anti-join, so the endless re-selection remains |
| A cursor ring recording attempted units | No new table; small | Lost on the loop: a bounded structure silently forgets the oldest failures, so the units that failed longest ago are re-selected at every tick, and it is neither queryable beside content nor visible to the anti-join |
| A sibling table of markers | Inspectable by SQL; content table stays clean | Lost on the loop: the anti-join reads the table the pipeline writes, so a marker in a sibling leaves the unit outstanding and it is re-selected at every tick |
| Extend the anti-join to read a sibling table | Keeps the content table clean and terminates the loop | Loses on effort and on coupling: two tables to reconcile, two schemas to keep in step, and an anti-join whose correctness depends on a second table existing |

## Criteria

1. **Whether the outstanding set can loop forever.** Measured by asking what the next tick does
   after a run in which every unit failed.
2. **Inspectability** — whether an operator can count and filter failures with the same query
   language they read content with.
3. **Survival across restart** — how many independent durability mechanisms the tier depends on.
4. **Consumer burden** — what a reader of the output table has to do to see only findings.

The loop decides it. Criteria 2 and 3 are satisfied by any in-store option including the sibling
table, so they do not separate the field; criterion 1 does, and it separates it completely,
because every option that does not land in the scanned table leaves a pipeline that spends its
whole budget re-attempting the same failures at every tick with no signal that it is doing so.
Criterion 4 is the price and is paid once, in a predicate.

## Consequences

Easier: a failure is a row. Counting undone work, grouping by reason, finding the units a given
host cost — all are ordinary queries against the table the content is in. The tier inherits the
write path's lease, durability, atomic commit and cursor handling with nothing added.

Harder: the output table's schema serves two populations, so content columns are nullable for a
reason that has nothing to do with content. A marker asserts nothing about who wrote words it
does not hold, which means `transcript_source`, `body`, `start_s` and `end_s` are null on it, and
a consumer reading those columns without filtering sees nulls it must interpret.

Accepted cost: the output table mixes content rows and failure rows, so every consumer filters on
the status column or on the sequence number. A consumer that forgets counts markers as findings —
a marker's `cue_seq` of `-1` and null body make that visible on inspection, but nothing enforces
the predicate.

Expensive to reverse: the negative sequence number and the shared schema are now part of the
table's contract, and the citation key construction assumes it. Moving markers out later would
break the anti-join's single-table property, which is the thing that terminates the loop.

## Revisit triggers

- A consumer is observed counting markers as findings, which would argue for a generated view that
  applies the predicate rather than for moving the markers.
- Marker rows come to dominate a table's row count to the point where content queries pay a
  material scan cost for them.
- The scan acquires a second input table for another reason, which would remove the objection that
  sank the sibling-table option.
