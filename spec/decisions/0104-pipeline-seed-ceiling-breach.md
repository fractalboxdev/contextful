# 0104 — A seeded stamp at or past the ceiling fails its whole chunk

**Status:** accepted 2026-09-18
**Decides:** `pipeline.seed.refusal.ceiling`, `pipeline.seed.refusal.ceiling-evaluation`

## Context

A seed block attaches a bulk-load source to a live pipeline and declares `below`, a
ceiling on the seeded table's `order_by` scale. The ceiling is what separates the two
halves of the table's history: every seeded row sorts strictly below the earliest live
row, and the live connector owns everything from the cutover forward. That separation is
a stated invariant of a seeded table, and the ceiling is the only thing enforcing it.

An export does not respect the ceiling by construction. It is a file a vendor or an
operator produced, often on a different clock, sometimes covering a wider range than
requested, sometimes carrying a handful of rows re-stamped by a later correction. A stamp
at or past the ceiling means that row's event time overlaps the live connector's
territory, and the same key may already be landed from the live side.

The ceiling is evaluated on the landed root batch after normalize and the chain and
before the write, so it reads the same column the read view orders by. That places the
decision at the last point before rows become durable: what the engine does about a
breach it must do instead of writing, not after.

A ceiling that cannot be evaluated is a separate case with the same weight. A batch
missing the declared ordering column, or carrying a stamp that cannot be ordered against
`below` at all — a different scale, an unparseable value — leaves the engine with no
basis for the comparison. A ceiling nothing can evaluate is a ceiling nothing enforces,
and the ordering invariant would hold only by luck.

## Decision

A seeded row whose ordering stamp reaches or passes `below` raises
`PipelineSeedCeilingBreached` naming the offending value, and its whole chunk lands
nothing. The stamp is never clamped and never dropped. A batch missing the declared
ordering column, and a stamp that cannot be ordered against the ceiling at all, each
raise `PipelineSeedCeilingUnevaluable`.

## Options considered

| Option | What it buys | What it costs |
| --- | --- | --- |
| **Fail the whole chunk, name the value** *(chosen)* | The ordering invariant holds over every landed row; the operator holds the export and can repair or re-cut it | One malformed stamp in a large export costs the whole chunk, and an export on a different scale cannot be loaded without repair |
| Clamp the stamp to the ceiling | Every row lands; the invariant holds by construction | Lost on auditability: it fabricates event time, and a row whose stamp the engine wrote cannot be distinguished from one the source carried, so the ordering guarantee becomes unauditable |
| Drop the offending rows | The load completes; the invariant holds | Lost on completeness: the seed silently holds less history than the export did, and nothing records which rows were discarded or how many |
| Land the conforming half of the chunk | Most of the data lands; only the breach is rejected | Lost on distinguishability: a partial window reads exactly like a complete one, so the table reports covering a range whose rows are partly absent |
| Treat an unevaluable stamp as conforming | No load is blocked by a scale mismatch | Lost on enforcement: the comparison never runs, so the ordering invariant is asserted over rows nothing checked |

## Criteria

1. **Whether the resulting state is auditable** — whether an operator can later verify
   the ordering guarantee from the landed rows alone. *(decided it)*
2. **Distinguishability of a partial load from a complete one.**
3. **Where the failure surfaces in time** — while the export is in hand, or months later
   as a frozen aggregate.
4. **Data preserved per failure.**

Auditability decided it because the seeded table's whole value proposition is a stated
guarantee — every seeded row below every live one — that `validate` audits over landed
rows. Clamping and dropping both keep the load running by making that audit meaningless:
after a clamp, the audit passes over stamps the engine invented; after a drop, it passes
over a history quietly smaller than the export. An audit that cannot fail is not an
audit. Distinguishability then eliminates the partial-chunk option for the same reason
it eliminates a partial parse elsewhere in the landing path.

## Consequences

`validate` audits the ordering over every landed row rather than over the deduped view,
and that audit means something: no landed stamp was written by the engine, and no row was
silently discarded to make it pass.

Naming the offending value makes repair directed. The operator reads the stamp, sees
whether the export's range was wrong or its scale was, and re-cuts or re-exports.

The cost accepted is chunk-granular loss. One malformed stamp in a large export costs the
whole chunk — potentially millions of good rows — and the operator repeats work for a
single bad value. An export whose stamps sit on a different scale entirely cannot be
loaded at all until it is repaired outside the engine, which for a large dump is a real
data-engineering task with no support inside the system.

The unevaluable arm makes that stricter still: a source that does not carry the declared
ordering column in every batch is unloadable, even where most of its batches would have
conformed.

## Revisit triggers

- Chunk sizes grow to the point where one bad stamp routinely costs an unacceptable
  amount of work, which is the case a row-level quarantine with an auditable record would
  serve.
- A repair verb appears that can re-stamp an export outside the write path with its own
  audit trail, which removes the fabrication objection to clamping by moving it somewhere
  it can be recorded.
- A source class appears whose ordering column is legitimately absent from some batches
  and present in others, which the unevaluable arm currently treats as unloadable.
