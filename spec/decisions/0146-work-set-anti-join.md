# 0146 — The candidate set is recomputed each tick by anti-join against the tier's own output

**Status:** accepted 2026-09-18
**Decides:** `derive.select.refusal.foreign-output-table`

## Context

Every tick the tier has to answer one question: which parent rows still lack their derived
output. The answer is not a position in a stream. Parent rows arrive out of order relative
to their readiness — an enclosure can land and be fetchable only a week later, a unit can
fail its attempt budget and later become reachable when an operator fixes a binding, a
backfill can insert rows whose publication instants are years old. A frontier that only
moves forward gets all three cases wrong in the same direction: it passes over the gap and
nothing ever brings it back.

The tier writes one row per finding and one marker row per unit that produced nothing, both
keyed by the parent key. That means the output table itself already records exactly which
parents are settled, with no second bookkeeping surface to keep in step with it. The
outstanding set is the parent table minus the keys the output table holds.

The name of that output table is the resolution question underneath. The anti-join is what
decides whether a unit is done, so whoever supplies the table name decides which rows the
pipeline considers finished. A manifest key naming an arbitrary table would let one pipeline
read another pipeline's output as its own completion record — and the two-sided split that
keeps a manifest from introducing commands applies here for the same reason.

## Decision

The outstanding set is recomputed from scratch each tick: the source reads the parent table,
reads the table it writes, and drops every parent for which it already holds a row, marker
rows included. The cursor is snapshot-shaped and carries no position. A table that has never
been created reads as zero derived parents rather than an error, so a first run has nothing
to special-case.

`max_rows_per_run` truncates the outstanding list after the anti-join has removed the derived
parents, never the scanned list before it. The pipeline identifier that resolves the output
table name comes from the build alone; a `[source.config]` key naming another pipeline's
output raises `DeriveForeignOutputTable`.

## Options considered

| Option | What it buys | What it costs |
| --- | --- | --- |
| **Anti-join against the tier's own output, recomputed each tick** *(chosen)* | No row can be stranded: a gap is outstanding until a row exists for it; no second bookkeeping surface; a first run needs no seeding | The scan is linear in the parent table, so cost grows with the whole archive rather than with the outstanding set |
| A second watermark over the parent table | Constant-time resumption, tiny state | Lost on stranding: a watermark advances past a gap and nothing brings it back — the exact failure the archive shape produces routinely |
| A watermark plus a backfill window | Bounded scan with a recovery path for recent gaps | Lost on operator effort: the window is a number that has to be computed, and re-computed every time the archive's arrival pattern changes |
| Applying the row cap before the anti-join | A bounded scan, trivially | Lost on stranding: under any ordering the run spends its whole budget re-examining rows already derived, and under a stable ordering it never reaches an undone row at all |
| Taking the output table name from a manifest key | An operator can point a pipeline at an existing table | Lost on the two-sided rule: one pipeline would mark another pipeline's work done, and a manifest authored elsewhere decides what this pipeline considers finished |

## Criteria

1. **Whether any row can be silently stranded** — whether there exists a sequence of arrivals
   and failures after which a parent row is never derived and nothing reports it.
   **This criterion decided it.** A stranded row is invisible by construction: it produces no
   error, no count and no log line, so no amount of operator attention finds it. Every other
   criterion here costs measurable resources, and measurable cost is preferable to a silence.
2. **Operator effort to backfill** — how much configuration a deployment writes and maintains
   to get old rows derived.
3. **Cost of a re-run** — what a tick spends when little or nothing is outstanding.
4. **Who supplies the completion record** — whether a manifest can change which rows a
   pipeline treats as done.

## Consequences

Every tick pays a full scan of the parent table, de-duplicated newest-wins in memory, with no
query engine behind it. On a small archive this is negligible; the cost grows with the whole
table and not with the work remaining, so a store with a million parent rows and nothing
outstanding still pays the scan. That is the cost this decision accepts.

What gets easier: correctness has no recovery mode. There is no reconciliation pass, no
repair command and no "re-derive since" flag, because the ordinary tick already is the repair
pass. Deleting a derived row is a supported way to request re-derivation.

What is now expensive to reverse: consumers and operators come to rely on delete-to-re-derive,
and a positional cursor would break that property silently rather than loudly.

## Revisit triggers

- The in-memory scan over a parent table stops fitting in the memory a tick is allowed.
- A query engine sits behind the store, making a set-difference query cheaper than a scan.
- A derive modality arrives whose output is not keyed one-to-one by parent key, so presence
  of a row stops meaning the unit is settled.
