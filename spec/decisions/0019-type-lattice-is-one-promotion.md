# 0019 — The schema lattice models one promotion and refuses every other pair at the write

**Status:** accepted 2026-09-18
**Decides:** `store.reconcile.refusal.incompatible-pair`

## Context

A table is a set of Parquet files written at different times by different runs, and its
schema is reconciled across that whole set rather than fixed once. Every read hands the
scan an explicit file list with union by name, so a column resolves to the common
supertype of the files that carry it, and a column that widened mid-life reads at the
widened type on every row — including rows in files physically written narrower.

That mechanism has exactly one pairing it can carry: `Int64` with `Float64` resolving to
`Float64`. Both are numeric, the scan's own type resolution produces the widened result
with no per-column cast, and the outcome is defined for every value, with a documented
precision loss above 9007199254740992. A JSON type absorbs its partner because both sides
land as UTF-8, which is not a promotion at all.

Every other disagreement has no defined result at read time. A string in one file and a
timestamp in the next has no common supertype the scan can produce without inventing a
parse; a boolean and an integer have no agreed mapping; a list and a scalar have no shape
in common. Whatever the scan does with such a pair, it is doing it per row, over files
already written, for a reader who did not cause the disagreement and cannot fix it.

The timing of the refusal is the second question. A batch is the last moment where the
party that caused the type change is the party receiving the error, and the last moment
before the disagreement is durable.

## Decision

The lattice models one promotion, `Int64` with `Float64` to `Float64`, and it is the one
pairing that produces physically mixed Parquet. Any other pair of observed types for one
column raises `StoreSchemaIncompatible` at the write, naming the column, the stored type
and the arriving one. The generated relation carries no per-column cast: widening happens
in the scan's own type resolution.

## Options considered

| Option | What it buys | What it costs |
| --- | --- | --- |
| **One promotion; refuse every other pair at the write** *(chosen)* | Every column in every file set has a defined read-time type, and the party that changed the type gets the error | A source that legitimately changes a column's type has its write refused, and its author emits the new type under a new column name |
| Widen every clash to text | No write is ever refused on a type change | Lost on read-time recoverability: it pushes a cast into every downstream query, and an identifier's leading zeros become meaningful in one file and absent in the next, so equality across files stops working |
| Store a variant column carrying both physical types | Preserves both representations exactly | Lost on read-time recoverability: the projection then differs per row and no single Arrow type describes the column, so nothing downstream can declare its shape |
| Fail the read instead of the write | Keeps ingestion unconditional | Lost on who receives the error: the rows have already landed, the refusal lands on a reader who did not cause it, and the table stays unreadable until an author intervenes |

## Criteria

1. **Read-time recoverability** — whether a disagreement has a defined result the scan can
   produce for every value. *(the one that decided it)* The integer-to-float pair is the
   one that survives union by name with a defined result and a bounded, documented loss;
   every other pair either invents a parse or has no single type. Ingestion availability
   was the competing criterion and lost, because a refused write is a source-side fix with
   the full original data still in hand, while an undefined read-time type is a defect in
   every later query.
2. **How much physically mixed Parquet a scan must unify** — the cost the read path pays
   per additional supported pair.
3. **Attribution** — whether the error reaches the party whose change caused it.
4. **Downstream shape stability** — whether a consumer can declare a column's type.

## Consequences

Any column a scan resolves has one Arrow type, and downstream consumers can declare it.
Widening costs nothing at query time because it happens in type resolution rather than in a
projection, so no query carries a cast that would reproduce the same arithmetic twice.

The cost accepted lands on sources that legitimately change a column's type — a field that
was a numeric code and becomes a string identifier, a date arriving as text after arriving
as a timestamp. That write is refused, and the author emits the new type under a new column
name, leaving the table with two columns describing one concept and every consumer choosing
between them.

The one supported promotion is itself lossy above 9007199254740992, and that loss sits in
the lattice rather than in any query, so it cannot be avoided by writing the read
differently.

## Revisit triggers

- The scan gains a type resolution that produces a defined result for a second pair over
  every value, without a per-row cast — decimal widening is the likely candidate.
- Sources in practice change column types often enough that new-column-per-change leaves
  tables with many near-duplicate columns, making the refusal the larger cost.
- A column format arrives whose physical representation makes a variant column describable
  by a single Arrow type.
