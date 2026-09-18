# 0073 — A capability the profile does not wire is a typed refusal rather than a panic

**Status:** accepted 2026-09-18
**Decides:** `run.journal.refusal.unwired-capability`

## Context

The run path reaches its substrate through one interface carrying six capabilities: start or
resume a run for a content-hashed plan reference, record a step's output, commit a cursor
under its kind, suspend on an awakeable with a timeout, attach a retry schedule to a step,
and describe what the driver provides. Not every build wires all six. A single-process local
profile has no reason to carry suspension machinery; a profile that delegates heavy steps to
another host carries it and may not carry something else. The interface is one type, so the
compiler cannot tell a caller which members the running build actually serves.

That gap is discovered at the moment of the reach, and the reach happens at an arbitrary
depth inside a run that has already done work. A pipeline can pull three batches, land two,
and then suspend on an approval gate the profile never wired. Whatever happens at that
instant happens to a run holding recorded steps, an execution owner and a position that has
not moved.

The failure taxonomy the engine already carries is a tagged type crossing every port, and
engine-level failures are members of it — a step failure carrying its label, an unresolvable
blob, the three suspension outcomes, a storage failure. A missing capability is the same
shape of fact: something the engine itself could not do, discovered at a call site, needing
to travel back through the same branch-by-tag path every other failure travels.

## Decision

A capability the running profile does not wire raises `CapabilityUnwired` when a caller
reaches for it. The refusal is a member of the shared failure taxonomy, so it branches by tag
like any other engine-level failure and the ordinary close path settles the outbound request
ledger, writes the failed run row and leaves the position alone. The driver answers a
describe call naming the capabilities it serves, so a caller establishes the wiring before it
depends on it rather than after. Each profile's wiring is an explicit list.

## Options considered

| Option | What it buys | What it costs |
| --- | --- | --- |
| **Typed refusal at first reach** *(chosen)* | The failure travels the run's own failure path: a run row, an error kind, a settled ledger, an untouched position. The gap names itself and the profile it was found in. | A describe surface a caller has to consult, and a wiring list per profile that reviewers keep honest. |
| Panic on the missing member | Nothing to carry: no error variant, no describe surface, no wiring list. | Loses on containment. A panic unwinds past the ledger settle and the run-record close, so the run leaves an opening row and nothing else — indistinguishable from a process that died. |
| Silent no-op returning a default | The run continues, and a profile mismatch never stops anything. | Loses outright. A suspension that never suspends resumes immediately with an empty payload; the run reports success having done none of the work, and the store holds rows produced from a gate nobody passed. |
| Compile-time capability typing | The mismatch becomes unrepresentable, with no runtime arm at all. | Loses on reach and on cost: the substrate is selected at build configuration and crossed by a guest component boundary, so the capability set is not in the caller's type at the point that matters. |

## Criteria

1. **When a mismatch surfaces.** How much of a run has already happened when the gap is
   discovered, and whether the run can close cleanly from there.
2. **Containment.** Whether the failure travels the same path every other failure travels, or
   escapes the paths that settle durable state.
3. **Honesty of the reported outcome.** Whether a run that could not do its work reports
   that.
4. **Discoverability before the reach.** Whether a caller can establish the wiring ahead of
   depending on it.

Criterion 3 decides it. Criteria 1 and 2 separate the typed refusal from the panic, but both
of those answers at least stop. The silent no-op is the one that produces a run reporting
success over work it never did, and a store cannot distinguish that row from a real one
afterwards — the damage outlives the run, which the other two do not.

## Consequences

A profile mismatch becomes an ordinary failed run: readable in the run record by error kind,
retryable or not by the same classification every other failure uses, and attributable to a
named capability rather than to a stack trace. The wiring list per profile is now a
reviewable artifact, which is a cost — it is one more thing to keep in step with the
interface when a seventh capability is added.

The describe surface is the accepted cost on the caller's side. A caller that wants to fail
early rather than at the reach has to ask, and a caller that does not ask still reaches the
refusal at depth. Nothing forces the early question, so the describe surface is available
rather than mandatory, and a long run can still spend minutes before discovering the gap.

Reversing this is cheap on the refusal itself and expensive on what has grown around it:
every profile's wiring list and every caller consulting describe assume the capability set is
runtime data.

## Revisit triggers

- The substrate interface stops being selected at build configuration, so the capability set
  becomes visible in the caller's type before the reach.
- A profile ships with a capability list that drifts from what it wires, and the drift
  reaches a run rather than a review.
- Runs are observed reaching an unwired capability at a depth where the wasted work matters
  more than the containment, making an admission-time capability check worth its cost.
