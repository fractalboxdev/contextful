# 0079 — A pending execution owner pins the connector build, and a terminal status releases it

**Status:** accepted 2026-09-18
**Decides:** `run.own.refusal.pinned-plan-changed`

## Context

One durable execution owner exists per live table and per backfill chunk. It holds the
connector identity including its component world, the pipeline content hash and the input
hash. Recorded work keys on the execution id, so several attempts under one owner resolve the
same recorded values, and a resumption reads back what an earlier attempt wrote rather than
re-entering the effect.

That is exactly the arrangement a rebuilt connector breaks. Recorded work is output the old
build produced. Replaying it into a new build means the new code receives values it never
computed, at step boundaries it may not have, with a component world it may not share. The
divergence is invisible: no step fails, the run completes, and the destination holds rows that
are half one build's work and half another's.

So a pin is needed. The question is what a later fire compares the build against, and the
candidate answers differ in what happens to a pipeline that is perfectly healthy. Pinning on
the health of the previous run sounds equivalent — a successful run and a run with no pending
work are usually the same pipeline — but they are opposite in the case that matters. A finished
fire leaves nothing to replay; there is no recorded work a new build could receive. A pin that
survives past completion turns every ordinary connector rebuild into a permanent refusal, on a
pipeline that has nothing at risk.

## Decision

While an execution owner is pending, a changed connector identity, component world or pipeline
content hash raises `ExecutionPinMismatch` ahead of replay — terminal and non-retryable —
naming the pipeline, the recorded identity and the one in front of it. Publishing a position
and retiring its execution owner share one catalog transaction, so a terminal status releases
the pin and a later fire receives a fresh execution id even where the position is byte-identical.
Success, and a failure that landed zero batches, release the pin; pending, running, partial
failure and cancellation hold it.

## Options considered

| Option | What it buys | What it costs |
| --- | --- | --- |
| **Pin while an execution owner is pending** *(chosen)* | The refusal covers exactly the window where recorded work could be replayed into a build that did not produce it, and a healthy pipeline rebuilds freely. | Position publication and owner retirement have to share one catalog transaction, and an operator whose pending work pins a build they no longer hold restores it and resumes, or rewinds explicitly. |
| Pin on the health of the previous run | One field to read, and no transaction coupling between publishing and retiring. | Loses on rebuildability. A finished fire leaves nothing to replay, so pinning past it refuses every connector rebuild on a pipeline with nothing at risk — a permanent refusal for a state that is safe. |
| Adopt the new build silently on resume | Rebuilds always work and nothing ever refuses. | Loses outright. Recorded output is replayed into code that never produced it, the run reports success, and the divergence is present only in the data. |
| Discard recorded work when the build changes | Correctness without a refusal: the run simply starts over on the new build. | Loses on cost and on effects. Recorded work includes vendor calls already made and batches already pulled; discarding it re-enters effects the ledger already settled. |

## Criteria

1. **What the refusal protects.** Whether the window it covers is the window where replay can
   diverge.
2. **Rebuildability of a healthy pipeline.** Whether an ordinary connector rebuild is possible
   without operator intervention.
3. **Effects re-entered.** Whether the answer causes an already-performed vendor call to happen
   again.
4. **Legibility of the refusal.** Whether the operator can act on it from what it names.

Criteria 1 and 2 point at the same place and together decide it: both are questions about
pending recorded work, and pending work is present in exactly the cases where divergence is
possible and absent in exactly the cases where a rebuild is safe. Pinning on run health
satisfies criterion 1 and fails criterion 2 completely, which is what rules it out rather than
any weighing.

## Consequences

Connector rebuilds are ordinary on a pipeline that has settled, and refused on one that has
not, with the refusal naming both identities so the operator can see which build the recorded
work came from. A run pins its connector identity at admission and a replay resolves the
artifact from that pin, so a rebuild after admission reaches no in-flight run either.

The cost accepted is a transactional coupling: publishing a position and retiring its owner
must land together, or a crash between them leaves a pin over work that is finished. That
constraint reaches the backfill path too, where completing a chunk publishes and retires the
same way.

The operator cost is real in the failure case. Pending work pinning a build that has been
garbage-collected or rebuilt over leaves two paths: restore the recorded build and resume to
completion, or rewind the chunk explicitly, retiring the owners its window covers. Neither is
automatic, and the second discards recorded work deliberately.

## Revisit triggers

- Connector builds become content-addressed and retained such that restoring a recorded build
  is always possible, which removes the stuck case entirely.
- A partial-failure state is observed holding pins long enough that the rebuild refusal is the
  dominant operator cost on this path.
- A new run status is introduced, which by the classification's own terms declares which side
  of the pin release it falls on.
