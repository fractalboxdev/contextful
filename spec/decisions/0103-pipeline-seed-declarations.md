# 0103 — A seeded table declares a key and an explicit event-time ordering

**Status:** accepted 2026-09-18
**Decides:** `pipeline.seed.refusal.seed-declarations`

## Context

A seed loads consumer-held history — an export, a dump, a years-old archive — into a
table the live connector also writes. Seeded rows travel the same tables, the same
normalize stage, the same chain, the same guard and the same write path as the live
connector, so the fold and the ordering that govern them are the table's own
declarations rather than anything the seed adds.

Two of those declarations do work a seed depends on and a live-only table can survive
without.

The key is what folds duplicates. Ingested tables are append-only files under a run
column, and the read is a fold by the declared key. A seed load is exactly the operation
that produces duplicates: it is retried after a partial failure, re-run after a reset,
and loaded twice by an operator who is not sure the first attempt took. With no declared
key, both copies of every row survive the fold, both are real rows, and every aggregate
over the table reads roughly double. Nothing fails. Nothing is logged. The number is
simply wrong.

The ordering column decides which row wins per key, and its default is the injected
ingest-time column. Ingest time cannot be backdated: every seeded row is stamped at the
instant it loaded, which is after the live connector started. A seed topped up later
therefore wins every overlapping key against live rows that are newer in the world, and
the table reports old values as current. The fix is not a seed-side override but an
event-time column the source itself carries — which is also what the read view orders
by.

Both failures are silent, and both surface as a frozen or doubled number months after
the load, long after the export is gone and the operator has moved on.

## Decision

A seeded table declaring no `primary_key`, or leaving `order_by` at the injected
ingest-time default rather than naming an event-time column, raises
`PipelineSeedDeclarationMissing`. The refusal lands at plan time and again at validate
time, so the declaration is settled while the operator still holds the export.

## Options considered

| Option | What it buys | What it costs |
| --- | --- | --- |
| **Refuse at plan and at validate** *(chosen)* | Both silent-wrongness modes are impossible; the operator answers while they still hold the export and the context | A source with no natural event-time column cannot be seeded until one is derived, and an export with no stable key cannot be seeded at all |
| Leave the key optional | Any export loads immediately, including one whose rows carry no natural identity | Lost on silent wrongness: a retried load lands its rows twice, both survive the fold, and every aggregate reads roughly double with nothing failing |
| Let the ordering default stand | One fewer declaration; the default is already correct for many live-only tables | Lost on correctness of the fold: ingest time cannot be backdated, so a seed topped up after the live connector started wins every overlapping key on load order and the table serves stale values as current |
| Warn rather than refuse | Nothing is blocked; the operator is told | Lost on when the symptom appears: the warning is read at load time and the damage appears months later as a frozen aggregate, by which point the warning is unfindable and the export is gone |

## Criteria

1. **Whether the failure mode is a wrong number or a failure** — whether the absence
   produces something that looks correct. *(decided it)*
2. **Distance in time between the cause and the symptom.**
3. **Breadth of loadable sources** — how many exports the requirement excludes.
4. **Declaration burden on the pipeline author.**

The first criterion decided it, and the second is why. Every other seed refusal in this
contract trades a failed load against a bad one; here both missing declarations produce
a silently wrong aggregate rather than any failure at all, and the symptom arrives
months after the cause. A refusal while the operator is watching is the only point at
which the question is cheap to answer. Breadth and burden are real and are the cost this
takes on, but they are visible immediately, and a source that cannot be seeded is a
known gap rather than a wrong answer.

## Consequences

A seeded table's fold and ordering are both stated facts, so the parity guarantee —
seeding then running the live connector over an overlapping window yields the row count
and per-key winners a pure live backfill yields — rests on declarations rather than on
defaults that happen to hold.

Declaring an event-time column also aligns the seed with the read: the ceiling is
evaluated on the same column the read view orders by, so the ordering guarantee is
auditable over landed rows.

The cost accepted is a narrowed set of seedable sources. An export with no natural
event-time column cannot be seeded until one is derived — from a filename, a directory
partition, a record's own content — and that derivation is the operator's work, not the
engine's. An export with no stable key cannot be seeded at all, and that is a hard stop:
a transaction log with no identifier, a flattened report, a scrape with no record id.
For those, the history stays outside the store.

Relaxing this later is cheap in code and unrecoverable in data: rows landed under a
relaxed rule are indistinguishable from correctly-declared ones, and the doubled or
frozen aggregate they produce cannot be separated from a true one afterwards.

## Revisit triggers

- A class of exports appears whose rows carry a defensible derived key the engine could
  compute rather than the operator declare, which removes the hard stop without
  restoring the silent duplicate.
- A read surface gains a fold that is safe without a declared key, which is the premise
  the key requirement rests on.
- Operators are observed declaring a synthetic ordering column to satisfy the refusal
  rather than a real event time, which means the refusal is being answered rather than
  met, and the ordering guarantee is hollow.
