# 0289 — A failed control poll keeps the armed set already in place

**Status:** accepted 2026-09-18
**Decides:** `control.reconcile.refusal.fail-static`

## Context

Under a configured control source the snapshot is the sole source of the pipeline schedule
set. Local schedules stay unarmed; the local manifest supplies only the control wiring, the
runtime configuration and the job blocks. So a daemon that cannot read its control plane
has no second opinion about what it is supposed to be running — the question is not which
of two schedules wins, but what a daemon does with the one it already holds when the
authority for it goes quiet.

The poll is a small, frequent, networked read: a pointer object whose body is a version
integer, and a snapshot document fetched when that version advances. Every layer of it
fails transiently. An object store returns `5xx`. A pointer is read mid-write. A control
server restarts. A snapshot lands with a truncated body and does not parse. At a thirty
second poll interval, a deployment sees these often enough that the failure path is a
normal operating state rather than an exception.

The three candidate behaviors differ in how their failure presents to an operator. Arming
nothing produces a deployment whose process is up, whose health route is green, whose logs
show a poll error, and which ingests not one row — and ingesting nothing looks exactly like
an upstream that has no new rows, which is the single hardest state in this system to
notice. Falling back to local manifest schedules produces a deployment running a cadence
the control plane never applied, which is worse than stale: it is wrong in a direction
nobody authored. Keeping the last readable set produces a deployment running a cadence that
was correct as of some earlier instant and may now be out of date.

## Decision

An unreadable pointer, a snapshot that does not parse, and a control plane answering `5xx`
raise `ControlSnapshotUnreadable`, log a diagnostic, and leave the armed set already in
place running unchanged. The armed cursor is not cleared, no entry is pruned, no entry is
added, and next-fire instants are untouched. A deployment holds its last-known-good cadence
across an arbitrarily long control-plane outage and resumes reconciling on the first beat
that reads a parsable pointer and snapshot. The local manifest's pipeline schedules stay
unarmed throughout: a failed poll is not a reason to start reading a source the control
source displaced.

## Options considered

| Option | What it buys | What it costs |
| --- | --- | --- |
| **Keep the last readable armed set** *(chosen)* | Ingestion continues across a transient outage; the failure is bounded to cadence freshness, which is the cheapest thing to be wrong about here. | A control plane that stops publishing permanently leaves a deployment on an old cadence indefinitely, visible only in a log line. |
| Fall back to an empty schedule | The deployment never runs an entry the current control plane would not have armed. | Lost on invisibility of the failure: a transient read error stops every entry while the process reports healthy, and a store that ingests nothing is indistinguishable from an upstream with nothing new. |
| Fall back to local manifest schedules | Something keeps running, sourced from a document on the machine. | Lost on authority: a control-managed deployment silently runs a cadence the control plane never applied, and the operator reading the applied document sees the wrong answer. |
| Fail static for a bounded window, then arm empty | Bounds the staleness a permanent outage can cause. | Lost on the same invisibility, deferred: the empty schedule still arrives silently, now at an instant nobody is watching, and the bound is a number with no principled value. |

## Criteria

1. **Cost of a wrong answer in each direction** — a slightly stale cadence against
   ingesting nothing at all. **This criterion decided.** The two errors are not
   symmetric: stale cadence produces data that is late, and an empty schedule produces no
   data while every other signal reads healthy. Lateness is recoverable by the next beat;
   a silent stop is discovered only when someone queries for rows that were never landed.
2. **Frequency of the triggering condition** — transient poll failures are routine, so the
   failure path's behavior dominates the steady state rather than decorating it.
3. **Authority of what runs** — whether the running cadence is one the control plane ever
   applied. Local fallback fails this outright.
4. **Observability of the degraded state** — whether an operator can tell the deployment is
   degraded. The chosen option is weakest here and pays for it below.

## Consequences

Ingestion survives control-plane maintenance, restarts and object-store blips with no
operator action, and a reconciler outage costs cadence freshness rather than data. The
armed cursor becomes the deployment's durable memory of intent, which makes it the one
piece of reconciler state worth persisting between beats.

The cost accepted: a control plane that stops publishing does not stop the deployment. A
control source decommissioned, repointed at an empty store, or left broken after a
reconfiguration leaves every daemon subscribed to it running its last cadence forever, and the
only evidence is a repeating log line. An entry an operator believes they removed keeps
firing. Cadence staleness is unbounded by construction, and detecting it requires reading
logs or comparing the armed cursor against the applied version from outside.

Reversing this is cheap in code and expensive in operations: flipping to fail-empty is a
few lines, but every deployment that had been quietly riding a stale cadence stops at once.

## Revisit triggers

- The armed cursor's version is exposed on the health surface alongside the applied
  version, making stale-cadence detectable without log reading — at which point a bounded
  fail-static window costs much less.
- Control-plane outages are observed lasting long enough that entries fire against upstream
  configuration that no longer exists.
- A deployment is found to have run a retired entry for longer than one cadence after the
  operator removed it from the applied document.
