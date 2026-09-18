# 0016 — A table name resolving to no schema is a catalog error rather than an empty result

**Status:** accepted 2026-09-18
**Decides:** `store.lay-out.refusal.unknown-table`, `store.fold.refusal.unknown-table-in-a-pass`

## Context

A table exists in the store once a `schema.json` declares it. A run that pulls no rows
still commits, so a declared table that has never landed a row is an ordinary and expected
state: it registers as a zero-row relation over its declared columns plus the injected
provenance set, and a count over it correctly answers zero.

That makes zero rows a meaningful answer, which is exactly why a misspelled table name
cannot be allowed to produce the same answer. If an unknown name resolved to an empty
relation, a typo in a query, a manifest or a scheduled command would read as a healthy
quiet stream. Every downstream aggregate then reports zero honestly — a sum of nothing, a
join matching nothing, a retrieval returning nothing — and each of those numbers is
individually correct and collectively meaningless. Nothing in the result distinguishes
"this stream is quiet" from "this table does not exist".

The compaction pass has the same exposure with a worse audience. A pass runs on a schedule
or from an operator command, over a list of table names that came from a manifest. A name
in that list with no schema behind it is a configuration defect, and a pass is the least
watched moment in the system: nobody reads the output of a compaction that appears to have
succeeded.

Warning rather than refusing is the tempting middle. It fails on the same ground as the
empty relation: the signal the situation already lacks is attention, and a warning line
inside a successful command's output supplies none.

## Decision

A table name that no `schema.json` in the tree declares raises `StoreUnknownTable` rather
than resolving to an empty result, at every surface that resolves a name. A table name in
a compaction pass that resolves to no schema raises `StoreUnknownTable` and halts the
command. "Landed nothing" and "no such table" therefore carry different shapes wherever a
caller can observe either.

## Options considered

| Option | What it buys | What it costs |
| --- | --- | --- |
| **Refuse an unknown name at resolution** *(chosen)* | A typo and a quiet stream are never confusable at any surface | A caller probing whether a table exists handles an error rather than reading a row count, and a pass naming a not-yet-created table fails rather than waiting for it |
| Resolve an unknown name to an empty relation | Every query runs; a not-yet-created table is tolerated | Lost on distinguishability: a typo reads as a healthy empty table and every downstream aggregate reports zero honestly, with no surface carrying the difference |
| Warn on an unknown name and continue | Keeps a scheduled pass green while still emitting something | Lost on distinguishability in practice: a warning inside a successful scheduled pass is read by nobody, so it supplies exactly the attention the situation already lacks |
| Refuse at read but tolerate in a pass | A scheduled pass never fails on a manifest typo | Lost on distinguishability at the least-watched surface: the pass is where a wrong name survives longest, so tolerating it there is tolerating it where it costs most |

## Criteria

1. **Distinguishability** — whether an operator can tell a quiet stream from a misspelled
   target. *(the one that decided it)* A zero-row answer is the correct reply for a
   declared table that has landed nothing, so the two outcomes need distinct shapes.
   Tolerance of a not-yet-created table was the competing criterion and lost, because a
   missing table is a state an author can fix in one place, while an indistinguishable
   wrong answer propagates into every derived number with no marker on it.
2. **Where attention is available** — whether the failing surface is one anybody reads.
3. **Blast radius of a wrong answer** — how far a silently-zero result travels.
4. **Tolerance of ordering** — whether a caller can name a table before it exists.

## Consequences

A misspelled table name fails immediately and names itself, at query time and at
compaction time alike, and the zero-row relation stays available as an honest answer about
a declared table.

The cost accepted falls on callers that treat a read as an existence probe: they handle an
error instead of reading a row count, which is more code at each such site. A pass whose
manifest names a table that a pipeline has not yet created fails rather than waiting for
it, so ordering between manifest changes and pipeline deployment becomes something an
author sequences rather than something the system absorbs.

Bulk operations become all-or-nothing on name validity. A pass over many tables halts on
the first unknown name, so a batch of otherwise-valid work does not proceed until the
manifest is corrected.

## Revisit triggers

- A supported deployment pattern creates tables lazily on first write, making a name that
  does not yet resolve an ordinary intermediate state rather than a defect.
- An existence-probe surface is introduced that answers the question directly, at which
  point read-time refusal stops being the only way to ask it.
- The pass acquires a per-table result shape that reports an unknown name distinguishably
  without halting, matching what the empty-fold outcome already does for quiet tables.
