# 0020 — A primary-key column takes no float promotion, refused at the write and at the first fold

**Status:** accepted 2026-09-18
**Decides:** `store.reconcile.refusal.key-widening`

## Context

The lattice carries one promotion, `Int64` with `Float64` to `Float64`, and that promotion
is lossy above 9007199254740992: an integer of 9007199254740993 reads back exactly until a
float batch lands on that column, after which it reads 9007199254740992. On an ordinary
column that is a rounded number — visible, describable, and wrong only in the last digits.

On a column a table keys on, the same residual is a deleted row. Both read plans partition
by the key: the deduplicating relation numbers rows within a key and keeps the first, and
the fold materializes that choice on disk. Two adjacent identifiers past the exact-integer
range collapse to one double, so they become one partition, and the window keeps one row.
The other row is not marked, not moved and not counted — it is absent from the snapshot,
and the folded runs are collected once retention passes.

The trigger is small. A single out-of-range JSON number arriving on that column in one
batch flips the reconciled type for the whole file set, and identifier columns are exactly
where large integers appear: snowflake ids, database sequences past 2^53, hashes rendered
as numbers.

The refusal needs two arms because there are two ways to arrive. A batch can widen a key on
a table whose snapshot already declares that key, and a table can reach its first fold with
a key column already reconciled to float from batches that landed before any snapshot
declared the key.

## Decision

A primary-key column takes no `Float64` promotion. Reconciliation raises `StoreKeyWidened`
on the widening batch, ahead of any Parquet, for a key a snapshot declares, and the first
fold raises it again on a column already reconciled to `Float64`. A key holding values past
the exact-integer range is emitted as a string.

## Options considered

| Option | What it buys | What it costs |
| --- | --- | --- |
| **Refuse the promotion on a key, at the write and at the first fold** *(chosen)* | No two distinct keys ever collapse into one partition | A source that legitimately widens a key column is refused rather than degraded, and its author changes the emitted type |
| Document it as an operator obligation | No code, no refusal, full flexibility | Lost on irreversibility: the trigger is a single out-of-range number in one batch, invisible at the moment it lands, and the resulting row loss is one-way once the fold runs |
| Cast each column to the reconciled type in the relation | Makes the widening explicit in the read plan | Lost on where the loss lives: the projection reproduces the same rounding, because the loss sits in the lattice rather than in the projection |
| Force every key to text | Removes the failure mode entirely, for every table | Lost as over-broad: it changes every existing keyed table's physical type and comparison semantics to prevent a case the refusal already catches, and the guidance to emit a large key as a string stays available to any author who wants it |

## Criteria

1. **Irreversibility of the consequence** — whether the outcome can be undone after it
   happens. *(the one that decided it)* The fold is one-way: the collapsed row is dropped
   from the snapshot and the runs that held it are collected on retention. On an ordinary
   column the same residual is a rounded number that remains visible and correctable at the
   source. Flexibility for a widening source was the competing criterion and lost, because
   its cost is one type change at the producer and the alternative's cost is a row nobody
   knows is missing.
2. **Visibility of the trigger** — how much has to go wrong for the loss to occur.
3. **Where the loss lives** — whether any read-side change can avoid it.
4. **Breadth of the remedy** — how many unaffected tables a fix disturbs.

## Consequences

A key's identity survives the whole file set, and the deduplicating relation partitions on
values that are distinct on disk and distinct in the plan. The refusal names the column, so
the author knows which producer field to emit as a string.

The cost accepted is that a source legitimately widening a key column is refused rather
than degraded: its write fails and its author changes the emitted type, which for an
already-landed table means a new key column rather than an in-place change.

The guard has a hole by construction. A table with no snapshot has no dedup arm to corrupt,
so the write-time arm keys off a snapshot's declared key and the first-fold arm is what
catches a table that reconciled to float before any snapshot existed. A table that is
keyed, has landed float-widened keys, and is never folded carries the latent collision
without raising anything until its first pass.

## Revisit triggers

- The lattice gains a wider integer promotion with no precision loss in the range keys
  occupy, which removes the collision rather than refusing it.
- The fold gains a way to detect that two source values mapped to one partition and report
  it, making degradation observable instead of silent.
- Keyed tables are routinely folded on a schedule from creation, closing the unfolded-table
  hole and making the first-fold arm redundant rather than necessary.
