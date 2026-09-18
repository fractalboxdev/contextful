# 0074 — Opening a position measured against a renamed field is refused before a request goes out

**Status:** accepted 2026-09-18
**Decides:** `run.advance.refusal.field-rename`

## Context

A watermark position carries the name of the column it was measured against beside the value,
serialized as `{"field": "<name>", "at": <value>}`. It carries that name for one reason: the
value alone is meaningless without knowing which column produced it. A high-water mark of
`2026-04-01T00:00:00Z` says nothing until it is attached to `updated_at` rather than
`created_at`, and the two columns in one table hold values that overlap heavily without being
comparable.

The manifest declares which field the pipeline reads incrementally. That declaration is
editable, and editing it is a normal thing an operator does — a vendor exposes a better
column, an earlier choice turns out to advance too slowly, a table is restructured upstream.
The stored position, meanwhile, is durable state from the last successful pass and carries the
old name.

The comparison itself is a filter sent to the source: admit rows whose declared clock field is
newer than or equal to the stored position. Nothing at the source can tell that the number
being compared was measured somewhere else. The engine's frontier rule holds a committed
position forward or level and never rewinds it, which means a position that has jumped ahead
because it was compared against the wrong column stays jumped ahead. Every row between the old
column's high-water mark and the new column's is skipped, permanently, with no gap visible in
any count.

## Decision

Opening a position whose stored field name differs from the manifest's declared incremental
field raises `CursorFieldMismatch` before a request goes out. No comparison is performed, no
window is computed, and the pipeline does not start. An operator changing the declared field
also clears or rewrites the position, which is an explicit action rather than a side effect of
the first fire after the edit.

## Options considered

| Option | What it buys | What it costs |
| --- | --- | --- |
| **Refuse at open, before the request** *(chosen)* | The mistake is bounded to a pipeline that did not run. The operator learns the position and the declaration disagree, naming both. | A legitimate rename needs an explicit second action, and the refusal reads as pedantry until the reason is stated. |
| Store the value without its field name | Nothing to compare, nothing to refuse, one less serialized field. | Loses on the size of the mistake and on detectability: a rename then compares new values against the old field's high-water mark, silently, with no artifact anywhere recording that the two came from different columns. |
| Compare anyway and warn | The pipeline keeps running and the operator has a log line. | Loses on the size of the mistake. The skip is unbounded and permanent; a warning is read after the frontier has already moved past the rows nobody will fetch again. |
| Silently reset the position on mismatch | Correctness is preserved without operator action — the stream re-reads from nothing. | Loses on cost. A full re-read of a large stream is initiated by an edit that did not ask for one, and the operator learns about it from the bill or the wall clock. |

## Criteria

1. **Size of the mistake each answer produces.** How many rows can be silently skipped, and
   whether the skip is bounded or unbounded.
2. **Detectability after the fact.** Whether the damage leaves any artifact a later reader can
   find.
3. **Cost imposed without being asked for.** Whether the answer spends time or money the
   operator did not request.
4. **Cost of the legitimate case.** How much friction a rename that was genuinely intended
   pays.

Criterion 1 decides it, with criterion 2 behind it. Comparing across fields is an unbounded
silent skip whose only evidence is rows that are absent — the one damage shape this system has
no way to notice later. Criterion 4 is real and is paid; a rename costing one extra command is
recoverable, and a permanent hole in a table is not.

## Consequences

A stored position is now self-describing: reading it tells you what it means. The refusal fires
ahead of any network call, so a mismatched pipeline costs nothing at the vendor and the error
names the pipeline, the stored field and the declared one.

The cost accepted is the operator's. A rename that was fully intended is refused on the next
fire and takes a deliberate action to clear. That action is also the decision point for whether
the stream re-reads from nothing or is seeded to a translated value — which is the right place
for a human, since only the operator knows whether the two columns' values are comparable at
all.

Nothing detects a rename that keeps the column name and changes its meaning. A vendor
repurposing `updated_at` from a modification stamp to a sync stamp passes this check exactly as
a well-behaved source does.

## Revisit triggers

- A source begins declaring several incremental fields per table, so one stored name is no
  longer the whole comparison.
- Renames become frequent enough that the explicit clearing step is the dominant operator cost
  on this path, making a declared translation worth specifying.
- The frontier rule changes such that a position can be rewound safely, which removes the
  permanence that makes a wrong comparison unrecoverable.
