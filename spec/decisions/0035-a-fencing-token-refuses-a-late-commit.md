# 0035 — A lease grants a monotonic fencing token and a commit below the object's token is refused

**Status:** accepted 2026-09-18
**Decides:** `sync.lease.refusal.stale-fence`

## Context

A lease is granted for a bounded life and expires without action from anyone, which is
what keeps a crashed holder from locking a pipeline out forever. A live holder renews at a
fraction of that life, so a long backfill keeps its hold mid-write. Expiry and renewal
together handle the two ordinary cases: the holder is working, or the holder is gone.

They do not handle the third. A holder that pauses — a stalled process, a suspended
container, a host that loses its clock for a while — does not know time passed. Its lease
expires, a successor takes it with a conditional replace on the tag it read, and the
successor begins writing. The original holder then resumes, believes it still holds the
lease, and commits objects under the pipeline it thinks it owns.

Nothing in the exclusion mechanism catches that. The conditional create excludes a second
acquirer; the tag-checked replace makes noticing an expiry and taking the lease one
indivisible step. Both operate at acquisition. The paused holder acquired legitimately and
is not acquiring again — it is writing, and the write path has no opinion about leases.

What distinguishes the two writers is that the successor acquired later. The lease object
already has somewhere to record that: acquisition can yield a number that increases every
time the lease is granted, the holder can stamp it on what it commits, and the commit path
can compare. The paused holder's number is below the one the lease object now carries, and
that comparison is available from data both parties already read.

Wall-clock time is the alternative discriminator and the weaker one. Two machines
disagreeing by seconds is ordinary, an unsynchronized reading is indistinguishable from a
synchronized one, and the pause being discussed is exactly the condition under which a
machine's clock is least trustworthy.

## Decision

Acquisition yields a fencing token that increases monotonically per pipeline, and the
holder stamps it on each object it commits under that pipeline. A commit whose fencing
token is below the one the lease object carries raises `LeaseFenced`, so a holder that
paused past its expiry lands nothing after a successor took over. The token rides on the
lease object and on the cursor object beside it, so the value a commit compares against is
one the store already holds.

## Options considered

| Option | What it buys | What it costs |
| --- | --- | --- |
| **A monotonic token granted at acquisition and compared at commit** *(chosen)* | Exclusion survives a pause. The discriminator is a number both parties read from the same object, with no clock and no second service in the path. | Every object a holder commits carries a token, so the commit path reads the lease object it would otherwise not need to, and a token comparison is one more refusal for a caller to handle. |
| Granted life and renewal alone | No token to propagate, no extra read at commit, and the mechanism is already there. | Lost on the stall window. The holder believes it still holds the lease and the store has no way to disagree, so a resumed process writes over a successor's work with no error on either side. |
| A wall-clock check at commit against the expiry instant | Uses a value the lease object already carries, with no new concept. | Lost on clock trust. The comparison would be against an unsynchronized reading taken by the machine whose sense of time is in question, and two machines disagreeing by seconds is the normal case rather than the failure case. |
| Fencing at the object store's own access policy | No application-level bookkeeping; the backend enforces it for every writer, including ones outside this code. | Lost on portability. The bound has to hold on every backend, including one with no conditional write at all, and the policy surface differs per product where the lease object does not. |
| Refuse the whole run when a holder detects it paused | Simpler than per-object tokens, and the pause is caught at one point. | Lost on detectability. A paused process cannot reliably detect its own pause; the only party that knows a successor took over is the store, and asking the store is the token comparison. |

## Criteria

1. **Whether exclusion survives a pause** — whether a holder that stalls past its expiry
   can still affect a successor's state. *This criterion decided it.* A lease whose
   guarantee holds only while every holder is scheduled is not an exclusion mechanism but
   an optimistic convention, and the operations it protects — advancing a hand-out cursor,
   committing under a pipeline — are the ones whose corruption is unrecoverable.
2. **Trust placed in a clock** — whether correctness depends on two machines agreeing
   about time.
3. **Portability across backends** — whether the property holds where no conditional write
   exists at all.
4. **Cost per commit** — reads and fields added to the write path. Real, and outranked by
   a window that otherwise has no cover at all.

## Consequences

The lease becomes a genuine exclusion rather than a best-effort one: a successor can take
over from an expired holder knowing the predecessor cannot land anything afterwards. The
token is a number on the lease object and on the cursor beside it, so a cursor adopted at
acquisition carries the fence that produced it and an operator reading either object sees
which generation wrote it.

The accepted cost is on the write path. Every committed object carries the token, and the
commit reads the lease object to compare — a read it would not otherwise need. Callers
gain one more refusal to handle, and it arrives late, after work is done, which is the
most expensive place for a refusal to land. A paused holder therefore discards whatever it
computed during the pause rather than salvaging it.

Reversing this is expensive because objects already carry the token and consumers may read
it. Removing the comparison silently restores the stall window without changing anything
an operator can see.

## Revisit triggers

- A backend is adopted whose access policy can enforce a generation bound for every
  writer, making the application-level comparison redundant rather than additive.
- The commit-path read becomes a measurable cost on pipelines committing many small
  objects, arguing for caching the token with a bounded staleness.
- A pause long enough to expire a lease is observed frequently enough that discarding the
  paused holder's work is itself the dominant loss, which would argue for a resumable
  handover rather than a refusal.
