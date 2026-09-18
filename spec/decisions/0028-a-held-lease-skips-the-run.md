# 0028 — A run meeting a held lease is skipped by name and retried on the next tick

**Status:** accepted 2026-09-18
**Decides:** `sync.push.refusal.concurrent-local-push`, `sync.lease.refusal.held-lease`, `sync.lease.refusal.release-by-a-non-holder`

## Context

A lease serializes the one operation the data plane cannot absorb twice: advancing a
cursor a source hands out as a token. Everything else a run writes is append-only and
survives being done again. The lease is taken per pipeline, granted for a bounded life,
and renewed at a fraction of that life while the run is working.

Runs arrive from a reconciler that ticks on a schedule. This matters for what a contended
run should do, because the reconciler is already a retry loop: every pipeline whose
schedule is due is attempted on every tick, whether or not it was attempted last tick. A
run that does nothing this tick is a run that will be offered the lease again shortly,
without anyone arranging for it.

The holder of a lease may be another machine or another process on this machine. A
machine's own catalog serializes local processes; the bucket lease serializes machines.
Both answer the same question — is someone already doing this — and a caller meeting
either of them is in the same position.

Release is the mirror question. A lease object carries a holder's node id, and a machine
whose lease expired while it was paused may still attempt to release it, by which time the
object belongs to a successor. Whether release checks the holder decides whether a late
release can free a live lease.

## Decision

A run finding an unexpired lease raises `LeaseHeld`, naming the holder and the expiry
instant, is skipped, and is attempted again on the next reconciliation tick. Two pushes of
one store from one machine raise `SyncPushInFlight`, the second naming the holder of the
push guard. Releasing a lease whose holder is another node id raises `LeaseNotHeld` and
leaves the object untouched. No caller waits on a lease, and no caller frees one it does
not hold.

## Options considered

| Option | What it buys | What it costs |
| --- | --- | --- |
| **Skip, name the holder and the expiry, retry on the next tick** *(chosen)* | One log line carries who holds it and when it frees. The system self-heals at the granted life with no queue, no waiter and no second scheduler. | A pipeline whose runs are longer than the reconciliation interval reports skips routinely, and an operator reads skip lines rather than failures to tell contention from a stuck holder. |
| Block until the lease frees | The run eventually executes without waiting for another tick, and ordering is preserved. | Lost on unbounded waiting. A crashed holder pins a waiter for the full granted life with no signal, and the waiting process holds whatever it acquired before reaching the lease. |
| Queue the contended run for later dispatch | Contended work is not lost, and a backlog is visible as a queue depth. | Lost on duplication. The reconciler already re-attempts every tick, so a queue is a second scheduler with its own ordering, its own persistence and its own failure modes, answering a question the first one already answers. |
| Let a waiter force the lease after a timeout it picks | A stuck holder stops blocking progress without operator action. | Lost on safety. The expiry belongs to the lease object, which every party reads; a timeout chosen by the waiter is a second opinion about the same instant, and two waiters with different opinions produce two holders. |
| Let any caller release any lease | Recovery from a stuck holder is a single command. | Lost on exclusion. A paused holder resuming and releasing frees a lease a successor is actively working under, which is the precise state the lease exists to prevent. |

## Criteria

1. **Operator legibility** — whether a contended state is readable from one line of
   output. *This criterion decided it.* Contention is the ordinary condition in a
   multi-machine deployment rather than an exceptional one, so it is read far more often
   than any other lease state. A skip naming the holder and the expiry answers both
   questions an operator has — who, and until when — without a second query. The
   alternatives all answer those questions indirectly or not at all.
2. **Unbounded resource growth** — whether a contended run holds a process, a connection
   or a queue slot for an interval nobody bounded.
3. **Number of schedulers** — how many components decide when work is attempted.
4. **Whether exclusion survives a pause** — whether a holder that stalls past its expiry
   can affect a successor's state.
5. **Latency of contended work** — how long a skipped run waits. This one was outranked:
   the wait is bounded by the reconciliation interval, which is already the granularity at
   which the system schedules anything.

## Consequences

Contention costs nothing structurally. There is one scheduler, and a contended run is
indistinguishable from a run whose schedule has not come due yet, except in the log line
it emits. A crashed holder locks nobody out past the granted life, because the expiry is
in the object and every party reads it.

The accepted cost is noise. A pipeline whose run time exceeds the reconciliation interval
skips on most ticks, so its log is mostly skip lines, and an operator distinguishing
healthy contention from a stuck holder does so by reading the holder and expiry fields
across several lines rather than by seeing an error. That distinction is not surfaced
anywhere as a state, only as a pattern.

Release-by-a-non-holder being refused means recovering from a genuinely stuck holder has
no in-band command: the recovery is to wait out the expiry. That is a deliberate trade and
it is the expensive part to reverse, since a forced-release verb added later has to answer
what it does to the successor's fencing token.

## Revisit triggers

- Skip lines are observed to dominate the operator surface for a common pipeline shape,
  which argues for a contended-and-healthy state distinct from a skipped run.
- A pipeline appears whose per-run work is long relative to any workable reconciliation
  interval, so the retry-on-next-tick path effectively never grants it the lease.
- An operator recovery path for a stuck holder is required in band, which reopens whether
  a release can ever be performed by someone other than the holder.
