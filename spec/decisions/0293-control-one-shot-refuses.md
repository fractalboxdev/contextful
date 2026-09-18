# 0293 — A one-shot evaluation refuses an unresolvable control source rather than running static

**Status:** accepted 2026-09-18
**Decides:** `control.fire.refusal.one-shot-control-source`

## Context

The `cycle` command evaluates the armed set once and exits, over the same due-ness test,
the same fire watermark and the same stage order a daemon uses. It exists so that a batch
invocation — a host crontab, a container that runs and stops, a step in someone else's
pipeline — can drive the engine without holding a process. A cycle beside a warm daemon
reads what that daemon wrote and repeats none of its work.

A daemon under a configured control source keeps its last readable armed set when a poll
fails, and holds its cadence across an outage. That behavior rests entirely on the daemon
having an armed set to keep: it has been polling, it has a cursor, and the set it is
running was applied at some earlier instant by the control plane.

A one-shot process has none of that. It starts with an empty cursor, resolves its control
source once, and exits. There is no previous armed set, so "keep what is already in place"
names nothing — the process holds no prior state to be static about. Applying the daemon's
rule to it collapses into arming an empty set, which is the behavior the daemon's rule was
chosen specifically to avoid.

The other candidate is worse in a different direction. Under a configured control source,
local pipeline schedules stay unarmed by contract; the manifest supplies the control
wiring, the runtime configuration and the job blocks, not the entry cadences. A one-shot
run that reads local schedules when the control plane is unreachable fires a cadence the
control plane never applied, from a document that was not meant to be the authority.

A one-shot invocation also has a caller. Its exit status is read by whatever scheduled it,
and that caller is the natural place for a failure to land — a non-zero exit is visible in
exactly the way a log line inside a long-lived daemon is not.

## Decision

A configured control source that does not resolve under one-shot evaluation raises
`CycleControlSourceUnresolved` and the cycle exits non-zero, having armed nothing. The
daemon's fail-static behavior is not applied, since a one-shot process holds no previous
armed set to hold on to, and local pipeline schedules are not read, since a configured
control source displaces them whether or not it can be reached. A cycle with no configured
control source is unaffected and arms from its local manifest as before.

## Options considered

| Option | What it buys | What it costs |
| --- | --- | --- |
| **Refuse and exit non-zero** *(chosen)* | The failure lands on the caller that scheduled the invocation, at the moment it happens, in the one channel a batch invocation actually has. | A batch run beside a briefly unavailable control plane fails rather than doing the subset of work it could have, so a transient outage costs a whole cycle. |
| Arm an empty set and exit zero | Uniform with the daemon's rule; no new error identifier; never fails a caller. | Lost on visibility: a batch invocation reports success having run nothing, and a crontab entry that succeeds while ingesting nothing is the exact failure the daemon's fail-static rule exists to prevent. |
| Fall back to local manifest schedules | The cycle does work rather than nothing, from a document that is present on the machine. | Lost on authority: the run fires a cadence the control plane never applied, and a document explicitly displaced by the control source becomes the thing that decided what ran. |
| Retry the control source within the invocation, then refuse | Absorbs a short outage without failing the caller. | Lost on timing: a one-shot invocation has a caller waiting and often a timeout, so the retry window is either too short to help or long enough to be its own failure mode. It composes with the chosen option rather than replacing it. |

## Criteria

1. **Whether the process holds a prior armed set** — the precondition the daemon's rule
   depends on. **This criterion decided.** Fail-static is not a policy that can be applied
   uniformly; it is a statement about preserving existing state, and a process with no
   existing state cannot perform it. Every other consideration is downstream of the two
   invocations being genuinely different in kind rather than in duration.
2. **Where the failure surfaces** — a one-shot invocation has an exit status its caller
   reads, which a daemon does not.
3. **Authority of what runs** — whether a cadence the control plane never applied can fire.
4. **Work completed under a transient outage** — the chosen option completes none, which is
   the cost below.

## Consequences

The two invocation shapes stay honest about their difference rather than pretending to one
policy: a daemon preserves cadence across an outage because it has cadence to preserve, and
a cycle reports that it could not determine what to run. A batch caller gets a real signal
in the channel it already watches, so a control plane that has been unreachable for a week
is visible in a crontab's mail or a pipeline's red step rather than in nobody's logs. This
sits beside the other stated difference between the two evaluations — that a missed window
defaults to `fire-once` for jobs, and an entry that has never fired is due at once — so the
one-shot path has a short, enumerable list of divergences rather than an implicit drift.

The cost accepted: a cycle that lands during a control-plane restart does nothing and
fails, even though the maintenance jobs in its local manifest would have been safe to run
and were not in question. Work that could have happened does not, and a caller that retries
crudely may re-fail for the whole outage. A deployment running frequent cycles against a
control plane with routine maintenance windows sees intermittent red for reasons unrelated
to its data.

## Revisit triggers

- Cycle invocations fail frequently enough against routine control-plane maintenance that
  the non-zero exits stop being read.
- A bounded in-invocation retry is measured against real caller timeouts and shown to
  absorb the common outage without exceeding them.
- One-shot evaluation gains durable memory of a previously armed set — persisted by a
  daemon on the same machine and readable by the cycle — at which point fail-static has
  something to be static about and the premise of this decision no longer holds.
