# 0033 — A cursor resolves by its declared kind and never by recency

**Status:** accepted 2026-09-18
**Decides:** `sync.merge.refusal.cursor-by-recency`, `sync.lease.refusal.wrong-cursor-kind-in-the-prefix`

## Context

A cursor records how far a pipeline has read from its source. Its kinds differ in what the
value means and in whether it can be re-read. A monotonic watermark is a row value — a
timestamp or an identifier already present in the data — and a pipeline holding a stale one
re-reads records it already has, which the fold removes. An opaque token is handed out by
the source, is valid once, and names a position no one else can reconstruct; a pipeline
holding a stale one has no way back to the records between the two positions.

That asymmetry is why some pipelines take a lease and others do not. The lease exists to
serialize the advance of a cursor a source hands out as a token, and a pipeline whose
cursor is a watermark needs no such exclusion, because a concurrent advance costs a
re-landed row rather than a lost one.

The merge that reconciles a bucket index has to assign a rule to every object class. Data
objects are append-only and their identifiers are unique, so they union and a duplicate is
harmless. The uniform temptation is to resolve everything the same way — last write wins,
which is cheap, needs no class analysis, and is correct for almost every key in the
bucket.

It is not correct for a cursor. Two writers advancing one token-kind cursor, resolved by
whichever copy was written last, silently discard the interval one of them consumed. There
is no error, no duplicate, and no artifact of the loss anywhere in the tree. The records
between the two positions were handed out once and will not be handed out again.

The bookkeeping prefix beside the lease is the second half of the question. The cursor
object at `cursors/<pipeline-id>.json` exists so that a successor to a lease resumes from
where its predecessor stopped. A value whose kind takes no lease has no business there:
nothing is serializing it, so its presence in that prefix asserts a coordination that is
not happening.

## Decision

A cursor entry resolved by whichever copy was written last raises `SyncCursorConflict`. A
cursor resolves by its declared kind, which permits no concurrent advance that skips
records. A cursor value whose kind takes no lease, reaching the `cursors/` prefix, raises
`LeaseCursorKindMismatch`; the values that live beside a lease are the ones a single writer
has to agree on.

## Options considered

| Option | What it buys | What it costs |
| --- | --- | --- |
| **Resolve by declared kind; keep the lease-bound prefix to lease-taking kinds** *(chosen)* | The one resolution that can lose records is unreachable, and the bookkeeping prefix holds only values something is actually serializing. | Two resolution rules live in one merge, and every object class needs an explicit assignment. A new key shape is a merge change rather than a default. |
| Last-write-wins across every object class uniformly | One rule, no class table, and correct for every append-only key in the bucket. | Lost on record skipping. A token-kind cursor resolved by recency discards the interval the losing writer consumed, with no error and no residue — silent data loss rather than a visible conflict. |
| Keep every cursor private to the machine that advanced it | No merge rule needed at all, and no cursor ever conflicts. | Lost on correctness under a lease handover. The successor to an expired lease would advance from a stale private position and could not deduplicate what the predecessor already consumed, which is the case the cursor object exists for. |
| Put a monotonic watermark beside the lease as well, for uniformity | One prefix holds every cursor, and the merge needs no kind test at that boundary. | Lost on necessity. A watermark is a row value the tree already carries and its pipeline takes no lease, so the object would be bookkeeping nothing coordinates, and its presence would suggest exclusion that is not held. |

## Criteria

1. **What an unsafe resolution costs** — whether being wrong loses records or duplicates
   them. *This criterion decided it.* A duplicate is removed by the fold and visible in the
   meantime; a skipped interval of a hand-out token is unrecoverable and leaves no trace,
   so the merge is designed around the one class where the error is permanent even though
   that class is a minority of the keys.
2. **Correctness across a lease handover** — whether a successor can resume without
   re-consuming or skipping.
3. **Truth of the bookkeeping prefix** — whether an object's location implies a
   coordination that is genuinely in force.
4. **Uniformity of the merge** — how many rules a reader has to hold to predict an
   outcome. This one was outranked: a second rule is a cost paid once by whoever reads the
   merge, while a skipped interval is paid by whoever needed the records.

## Consequences

A pipeline's cursor kind now settles three questions at once — whether
a lease is taken, how a conflict resolves, and whether the value belongs in the
lease-bound prefix — and those three stay consistent because the kind is declared once and
read everywhere rather than inferred.

The accepted cost is that the merge is no longer uniform. Each object class carries an
explicit assignment, and a key shape introduced without one falls to a default that is
correct for append-only data and wrong for anything positional. Adding a class is
therefore a change to the merge, reviewed as such, rather than a layout decision that
happens to work.

Reversing toward uniform recency resolution is a single-line change and would reintroduce
a loss mode with no observable signal, which is the expensive property of this decision:
its correctness is invisible when it is working.

## Revisit triggers

- A cursor kind appears whose value is neither re-readable nor hand-out-once, so neither
  existing rule applies to it.
- A source is adopted whose tokens can be replayed from an earlier position, which would
  collapse the asymmetry the two rules are built on.
- Class assignments are observed to be missed in practice for new key shapes, arguing for
  a merge that refuses an unassigned class rather than defaulting it.
