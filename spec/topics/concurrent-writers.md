---
topic: concurrent-writers
---

# Concurrent writers

Two writers race when both try to change the same state at once: two commits to one
table, two nodes running one pipeline, two machines pushing one bucket, or two callers
running one journaled step. **Contextful** settles every race with one primitive, a
linearizable conditional write: a write that succeeds only while a stored precondition
still holds ({{topology.coordinate.primitive}}). No consensus service runs beside it.
The operations that need a single writer form a closed list, and the pointer commit
that publishes a snapshot is one of them ({{topology.coordinate.inventory}}).

## Two commits to one table

Commits to one table on one store take the table's commit lock in turn. The lock covers
the commit sequence and the manifest write, while the commit writes its Parquet parts
outside the lock. A commit that waits past its bound fails as a transient error, and the
caller retries it ({{store.reserve.commit-wait}}).

## Two nodes running one pipeline

A writer lease gives one node exclusive ownership of a pipeline, a source partition or a
table compaction. A run that finds another node's unexpired writer lease skips its turn
and runs again at the next reconciliation tick ({{store.lease.held}}).

Expiry alone does not stop a paused holder from writing late. Each acquisition raises the
lease's commit fence by one, and every commit under the lease is a conditional write on
the holder's commit fence. A late commit from the earlier holder carries a lower commit
fence, loses its condition, and leaves its run unreadable
({{topology.coordinate.fenced-commit}}).

## Two machines pushing one bucket

A push commits the bucket manifest by a conditional replace on the version it read. A
push that loses reads the newer manifest, merges, and commits again. The number of
rounds that make no progress is bounded, so two pushers converge or stop
({{store.merge.retries}}).

## Two callers running one step

The journal records each step's value once. The first caller claims the step's key
before running the effect, and a concurrent caller waits for the recorded value instead
of running the effect a second time ({{run.journal.entry-key}}). A claim lives only as
long as its owner's lease: once that lease expires, the next caller takes the claim over
({{run.journal.claim-takeover}}). An owner that a newer owner has replaced checks its
standing immediately before each outbound request, so it sends nothing and publishes
nothing ({{run.items.fence}}).

## What each writer observes

| Race | The winner | The loser |
| --- | --- | --- |
| Two commits, one table | Commits under the commit lock | Waits, then fails as transient past the bound |
| Two nodes, one pipeline | Holds the writer lease | Skips the run until the next tick |
| A late commit after a lease transfer | The holder of the higher commit fence | Loses its conditional write; its run stays unreadable |
| Two pushes, one bucket | Commits the bucket manifest | Re-reads, merges and commits again |
| Two callers, one step | Claims the entry key and runs the effect | Reads the recorded value |
