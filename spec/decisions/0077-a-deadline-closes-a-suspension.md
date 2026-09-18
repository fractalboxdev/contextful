# 0077 — A deadline transition persists, and a late resolution is refused by name

**Status:** accepted 2026-09-18
**Decides:** `run.suspend.refusal.expired-token`

## Context

A suspension carries a deadline: the creation instant plus a time-to-live, both handed in by
the caller as RFC3339 Zulu strings. The core reads no wall clock and computes no instant of
its own — every instant it compares against arrives from outside, injected by whichever
surface is asking. That is what makes the suspension machinery testable and what keeps a
clock out of a core that otherwise needs none.

It also means the instants arriving at the registry are not ordered. The resume route, the
state route, the runner's own poll and a control-plane sweep each supply their own instant,
taken from their own clock at their own moment, and they reach the registry in whatever order
the network delivers. A poll carrying an instant from a moment before another reader's poll
can arrive after it.

The question is what the deadline means under those conditions. If expiry is a predicate
recomputed on every read against the instant in hand, then two readers holding instants on
opposite sides of the deadline read opposite states from the same durable row, and a reader
with a lagging clock reads a closed suspension as still open — after another reader has
already treated it as closed and let the run move on.

## Decision

Expiry is evaluated on poll against the injected instant, and the transition persists. A
pending suspension observed past its deadline transitions to `timed_out` and stays there; a
later poll carrying an earlier instant still reads `timed_out`. A token whose deadline has
passed raises `AwakeableTimedOut` on resolution, answering `410` on the resume route, so a
late external party learns the suspension closed rather than resuming a run that has moved on.
The state route evaluates the deadline on its read, so observing a suspension is what closes an
expired one.

## Options considered

| Option | What it buys | What it costs |
| --- | --- | --- |
| **Lazy evaluation, sticky transition** *(chosen)* | One durable answer per suspension. Two readers holding different instants agree, and the core still reads no clock. | A suspension no one polls stays nominally pending until something reads it, so expiry is observed rather than scheduled. |
| Recompute the predicate on every read | No state transition to write, and no row ever holds a stale verdict. | Loses on agreement. A poll carrying an earlier instant reads a closed suspension as pending and resumes a run that another reader already let continue. |
| An eager timer firing at the deadline | Expiry happens on time whether or not anyone is looking. | Loses on footprint. It puts a clock and a scheduler inside a core built to read neither, and every deploy target then owes a timer that survives restart. |
| A control-plane sweep closing expired rows on a tick | Expiry is bounded by the tick without a timer in the core. | Loses on reach and on cost: it adds a durable background job for a transition that is free at the moment of the next read, and the read still has to handle a row the sweep has not reached. |

## Criteria

1. **Whether two readers can disagree about one suspension's state.** Agreement across
   readers holding unordered instants.
2. **Clock footprint in the core.** Whether the decision introduces a clock, a timer or a
   scheduler where none exists.
3. **Promptness of closure.** How long after the deadline the state changes.
4. **Legibility to the late external party.** Whether a party posting after the deadline
   learns why it was refused.

Criterion 1 decides it. The deadline is computed from caller-supplied instants that arrive out
of order, so any rule recomputing the verdict per read is a rule that returns different answers
to different readers about durable state — and the run on the other side of that disagreement
has already continued. Criterion 2 rules out the eager timer that would also have satisfied
criterion 1. Criterion 3 is what is given up.

## Consequences

The registry holds one verdict per suspension and every surface reads the same one. Testing is
exact: a test supplies the instants and the transition is reproducible with no sleeping and no
clock control.

The accepted cost is promptness. A suspension nobody polls and nobody resolves sits nominally
pending past its deadline, and its run stays suspended, until something reads it. Expiry is
therefore observed rather than scheduled, and the delay is bounded only by whatever next
touches the row. A deployment that wants bounded closure arranges a reader, which is a
configuration choice rather than a property of the core.

A late external party receives `410` and the written value is never established, so the
distinction between "you were too late" and "someone else answered first" is carried by two
different error identifiers rather than one.

## Revisit triggers

- Suspensions are observed sitting expired-but-pending long enough that the runs behind them
  hold resources that matter.
- A deploy target arrives that already owns a durable timer, making eager expiry free rather
  than an addition to the core.
- Instant sources are brought under one authority such that reads become ordered, which
  removes the disagreement that stickiness exists to prevent.
