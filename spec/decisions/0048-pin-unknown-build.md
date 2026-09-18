# 0048 — An unknown or collected build identifier is refused, naming the oldest pinnable build

**Status:** accepted 2026-09-18
**Decides:** `read.resolve-pin.refusal.unknown-build`

## Context

A read may name a build identifier per table and resolve that table to exactly the state the
build published; an unnamed table resolves to the latest published state. Every response
touching a published model echoes `contextful.resolved`, mapping each such table to its build
identifier and watermark whether or not the request passed a pin.

The echo exists for one check: a consumer compares resolved build identifiers across every
query of one derivation and fails that derivation where two differ, rather than writing rows
stitched from two states. A pin is the mechanism that makes those identifiers agree — a
derivation reading four tables over twenty queries pins each table once and every query
resolves identically.

Published builds do not live forever. Retention collects old ones, so a long-running
derivation's pinned build can age out mid-derivation. That is the moment this decision governs:
a pin the store can no longer honor.

Serving something anyway is the tempting move, and the echo is what makes it dangerous rather
than merely approximate. A consumer that receives a different build identifier than it pinned
can detect the substitution only if it compares against its own pin, not merely across
queries — and a substitution applied consistently across every remaining query passes the
cross-query comparison the echo was designed for. A derivation would then complete, report
success, and carry rows from two states.

## Decision

An unknown or collected build identifier raises `PinnedBuildUnavailable` and names the oldest
identifier still pinnable. A pin is never quietly widened to the latest state and never
resolved to a nearest surviving build.

## Options considered

| Option | What it buys | What it costs |
| --- | --- | --- |
| **Refuse, naming the oldest identifier still pinnable** *(chosen)* | A derivation whose pinned state aged out stops rather than completing wrongly, and the refusal carries the information needed to re-pin. | A long-running derivation breaks at the moment its build is collected rather than degrading, and retention of published builds becomes the operator's lever on how long a derivation may run. |
| Serve the latest state for an unknown identifier | Nothing ever fails on a stale pin. | Loses on observability — a derivation reading a moved state reports success, and rows from two states land under one result. |
| Serve the nearest surviving build | The substitution is small and the answer is close. | Loses on the same criterion, more insidiously: a silently substituted state is undetectable in the rows, and "close" is not a property any consumer declared it wanted. |

## Criteria

1. **Observability of a broken pin** — whether a consumer can tell that its pin stopped
   holding. Both rejected options fail this, and both fail it in a way the cross-query echo
   comparison does not catch.
2. **Recoverability** — whether the caller learns enough from the failure to proceed. Naming
   the oldest pinnable identifier turns the refusal into a re-pin instruction.
3. **Availability of a long derivation** — whether a derivation survives a retention boundary.
   This is the criterion the chosen option loses on.
4. **Honesty of the resolved echo** — whether the echo can be trusted as a statement about
   which state produced the rows. Any substitution makes it a statement about what the store
   chose instead.

Observability decides it. The whole apparatus of pins and the resolved echo exists so a
consumer can prove its derivation read one state; a substitution that defeats that proof
removes the reason to have built any of it.

## Consequences

A derivation either reads one state or fails saying which pin it could not honor, and the
failure is at the query rather than in the output. The resolved echo remains a true statement
about the state that produced the rows, which is what makes the cross-query comparison worth
performing at all.

The accepted cost is availability, and it lands on the operator. Build retention is now the
setting that decides how long a derivation may run, and a derivation longer than the retention
window fails no matter how it is written. The refusal is abrupt: there is no warning as a
pinned build approaches collection, so a consumer learns at the moment of failure rather than
before it.

Reversing this is cheap to implement and would retroactively weaken every derivation that
trusted the echo, since past results could no longer be said to have read one state.

## Revisit triggers

- A derivation-length requirement exceeds any reasonable retention window, making refusal the
  normal outcome rather than the exceptional one.
- Published builds acquire a pin-hold mechanism that exempts a build from collection while a
  consumer declares interest, which would remove the collision between retention and pin
  lifetime.
- The resolved echo is extended with a signal distinguishing a requested pin from a resolved
  one, which would make a substitution detectable and therefore arguable.
