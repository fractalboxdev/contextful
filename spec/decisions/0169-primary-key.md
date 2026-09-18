# 0169 — A derive output table is keyed on the unit reference and the cue sequence

**Status:** accepted 2026-09-18
**Decides:** `derive.land.refusal.missing-primary-key`

## Context

The derive tier reads a population of parent rows — episodes, documents, images — and
lands one output row per passage it recovers from each. The tier is re-entrant by
construction: a unit that came back `unavailable` is attempted again under the per-status
ceiling, a unit whose parent row changes is re-derived, and an engine that improves is
pointed at material it has already seen. Every one of those paths sends the same unit
through the same builder a second time.

The store deduplicates a landed batch against the table's declared key and nothing else.
A table with no `primary_key` is never deduplicated; its view stays a plain union over
every batch the pipeline has ever landed. Nothing about that is visible at land time —
the rows are all well-formed, the counts all rise, and the defect surfaces as a reader
finding the same sentence quoted four times with four identical citation keys.

The output rows are not one per unit. A transcribe unit yields one row per passage plus,
where it yields nothing, a single marker row at `cue_seq = -1`. The unit reference alone
therefore identifies a set of rows rather than a row.

## Decision

A derive output table declares `primary_key = ["unit_ref", "cue_seq"]`. A table
declaring any other key, or none, raises `DerivePrimaryKeyMissing` at build rather than
at land. Re-deriving a unit replaces that unit's passages in place; the marker row shares
the key space at a sequence number no passage occupies, so a unit that later succeeds
replaces its own marker.

## Options considered

| Option | What it buys | What it costs |
| --- | --- | --- |
| **`["unit_ref", "cue_seq"]`, refused at build** *(chosen)* | Re-deriving a unit is idempotent; the key set is uniform across every pipeline of the tier, so one anti-join and one fold read every derive table | A pipeline cannot choose its own key, and a task whose natural arity is not one-row-per-sequence has to express itself in this shape |
| No declared key | Nothing to author; the manifest is shorter | Lost on accumulation: the view is a plain union, and a re-derived unit doubles every passage beside the originals with no signal that it happened |
| Key on `unit_ref` alone | Reads as the obvious identity — one unit, one subject | Lost on arity: one unit lands many rows, so the key collides on every passage after the first and the batch is refused or collapsed to one passage |
| Key on the citation key `segment_id` | Byte-identical to what a feed source lands, so the union is free | Lost on completeness: a marker row carries no `start_s` and therefore no citation key, so the failure population falls outside the key entirely |

## Criteria

1. **Idempotence under re-derivation** — whether sending a unit through the tier twice
   leaves the store in the state one pass would have left it in.
2. **Arity** — whether the key identifies a row rather than a set of rows.
3. **Coverage of the failure population** — whether marker rows sit inside the same key
   space as content rows, so the fold that reads status reads one table.
4. **Authoring freedom** — how much a pipeline gets to decide for itself.

Criterion 1 decided it. Arity rules out the unit alone and coverage rules out the
citation key, but both of those are defects a reviewer notices on the first batch. The
accumulation defect is silent: it produces correct-looking rows, rising counts and a
green run, and it is discovered by a reader who has already been served a quadrupled
answer. A defect that only presents downstream of the corpus outranks one that presents
at the point of the mistake, so the key is fixed by the tier and refused at build.

## Consequences

A pipeline author no longer chooses the key of a derive output table, and a future task
whose output is genuinely one row per unit still declares a `cue_seq` and lands it at a
constant. The tier gains a single anti-join and a single marker fold that work against
every derive table, because the key is the same one everywhere.

The cost accepted is that a derive table's key set is fixed by the tier rather than
chosen per pipeline. A third-party connector wanting derive's re-entrancy and a
different key shape has neither.

Reversing this is expensive in the corpus rather than in the code. The citation key
shape, the marker-row sequence number and the outstanding-set anti-join all read the same
two columns; changing the key changes all three and every landed table has to be rebuilt
under the new one.

## Revisit triggers

- A derive task lands rows whose natural identity is not a sequence within a unit, and
  expressing it as one produces a constant column on every row.
- The store gains per-table key declarations that admit more than one key, so a table
  can be deduplicated on the tier's key and read through another.
- Marker rows move out of the output table, which would drop criterion 3 and re-open the
  citation key as a candidate.
