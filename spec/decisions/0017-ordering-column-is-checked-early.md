# 0017 — An ordering column is validated against the declaration and the injected set before the first batch

**Status:** accepted 2026-09-18
**Decides:** `store.declare.refusal.order-by-unknown-column`

## Context

A keyed table declares `order_by`, the column the fold reads to pick the surviving row per
key. It defaults to the injected ingest stamp, which every row carries, and a declared
value names either a producer column or one of the engine's injected columns.

A declared value naming neither is not a read error. Union by name widens a column some
file carries and invents none no file carries, so an ordering column absent from every
file enters the scan through a zero-row branch — the ordering expression resolves against
a literal null rather than failing. Every row in a key then compares equal on the ordering
term, and the window keeps whichever row the tiebreaker or the scan order happens to
surface. The fold still runs, still reports success, still writes a snapshot with one row
per key.

The defect is invisible in every count. Row counts are right, key counts are right, the
snapshot is well formed. What is wrong is which row survived, and there is no artifact
that records the intended winner to compare against. A restated row that should have
superseded its predecessor may or may not have; that answer varies per key and per pass.

The fold is also one-way. Once the surviving rows are materialized and the folded runs fall
outside the retention window, the losing rows are gone, so a wrong winner discovered later
is not recoverable by re-running with a corrected declaration.

## Decision

An `order_by` naming a column that neither the table's declaration nor the engine's
injected set carries raises `StoreOrderByUnknownColumn` at validation, ahead of the first
batch. Validation compares the declared name against the declared columns and the injected
provenance set, so an unresolvable ordering column is refused before any row lands under
it.

## Options considered

| Option | What it buys | What it costs |
| --- | --- | --- |
| **Refuse at validation, before the first batch** *(chosen)* | No row is ever folded under an ordering column that cannot resolve | A table whose ordering column arrives from a source only after the first batch declares the column later rather than up front |
| Fall back to the injected ingest stamp | Every declaration works; the fold always has a real ordering term | Lost on how long a wrong winner survives: the fallback is correct often enough that the declaration is never fixed, and write-time order is the wrong winner for a restated row that arrives out of order |
| Fail at the first fold | Catches the same defect with no validation pass | Lost on timing: runs have landed by then, the pass is the least-watched moment, and the table is already unable to fold until a declaration change |
| Warn at validation and proceed | Surfaces the problem without blocking a deployment | Lost on how long a wrong winner survives: the pass succeeds, the snapshot looks correct, and the warning is one line in output nobody re-reads |

## Criteria

1. **How long a wrong winner survives** — the interval between the defect existing and
   anything being able to show it. *(the one that decided it)* An unresolvable ordering
   column degrades the fold to an arbitrary winner per key, which no count exposes and no
   artifact records, and the fold is one-way. Ergonomics of declaring a column absent from
   the source was the competing criterion and lost, because it costs an author one
   ordering change and the alternative costs silently wrong data.
2. **Recoverability** — whether the outcome can be corrected after the fact.
3. **Detectability** — whether any observable number differs when the defect is present.
4. **Declaration ergonomics** — whether a table can state its intent up front.

## Consequences

A declared ordering column is known to resolve before any row is folded under it, so
last-write-wins means what the declaration says at every pass. The refusal names the
column, which makes a typo a one-line fix at the moment it is introduced.

The cost accepted is ordering friction on schemas that grow. A table whose ordering column
arrives from a source only after the first batch cannot declare it up front; its author
lands rows under the default ingest stamp and declares the intended column once it exists,
accepting that the folds in between used write-time order.

The injected set becomes part of the validation contract. Adding an injected column widens
what an `order_by` may legally name, and removing one refuses a declaration an
existing table already carries.

## Revisit triggers

- A source pattern in common use introduces its ordering column only after first ingest,
  making up-front declaration impossible for a whole class of tables rather than an
  occasional one.
- The fold gains a way to record the ordering term it actually applied per pass, which
  would make a degraded ordering detectable after the fact and weaken the survival-time
  argument.
- Runs are retained long enough, by default, that a fold under a wrong ordering column is
  reversible by re-folding from the run files.
