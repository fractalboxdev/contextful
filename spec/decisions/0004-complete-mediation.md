# 0004 — Every access to data traverses the enforcement stack, and no configuration disables it

**Status:** accepted 2026-09-18
**Decides:** `topology.compose.refusal.mediation`

## Context

The enforcement stack spans both halves: the host's capability allowlists and the journal
on the run path; the statement guard, the visibility semi-join, row and column
restriction, masking and the audit chain on the read path. A record passing between the
halves passes enforcement on both sides of the crossing it uses. The stack's layers are
composed in a fixed order, and its correctness depends on nothing reaching data around it.

The failure this guards against does not announce itself. A path that reads a row without
entering the stack returns rows that look exactly like correct rows — same columns, same
types, same plausible values — with masking not applied, the visibility semi-join not run,
and no entry in the audit chain. There is no error, no latency change, and no output a
reviewer would flag. The same is true of a disabled stack: the process starts, the health
check passes, and queries answer.

Every surface that holds the only convenient handle to some data is a candidate for such a
path. A diagnostic endpoint, an export job, a background compaction that reads parts
directly, a test harness promoted into production use — each has a plausible reason to
read beneath the stack, and each reason is locally reasonable.

## Decision

The enforcement stack is invoked on every access to data. Complete mediation is an
obligation carried by the composition rather than a setting: a reachable path that returns
a row without traversing the stack is a defect. A code path reading or releasing a row
without entering the stack raises `MediationBypass` at review and at the dependency audit.
An operator switch that disables mediation does not exist — there is no flag, no
environment variable and no build feature that turns the stack off.

## Options considered

| Option | What it buys | What it costs |
| --- | --- | --- |
| **Mediation as a property of the build, with no disable switch** *(chosen)* | Both failure modes — a bypassed path and a disabled stack — become audit findings rather than silent correct-looking output. | A surface holding the only handle to data writes an adapter through the stack rather than reading around it, which is a network hop or a generated artifact with a staleness check. |
| An operator flag marking a trusted path | Solves the awkward cases immediately; the operator knows their own deployment. | Lost on failure mode: the flag's blast radius is invisible at the moment it is set, and the deployment that sets it looks healthy and answers queries. A flag set once for one job stays set. |
| Per-surface opt-in enforcement | Each surface pays only for the enforcement it needs, and new surfaces start simple. | Lost on the same criterion, and it multiplies the places the property must be re-established — every new surface is a fresh chance to omit it, with no single point that fails. |
| Enforcement asserted by code review alone, with no audit rule | No tooling to build; reviewers already read the diffs. | Lost on detectability over time: a bypass introduced during a refactor reads as a call-site move, and no later review revisits the question. |

## Criteria

1. **Failure mode** — what the system does when the property is violated, and whether the
   violation is visible. **This criterion decided.** A bypassed stack returns
   correct-looking rows and a disabled stack comes up healthy, so neither failure
   announces itself; the only defense is removing the states in which it can occur.
2. **Number of places the property must hold** — one composition rule against one rule per
   surface.
3. **Operator escape** — whether a deployment with an awkward requirement has a supported
   path. The chosen option is the most restrictive here and lost this criterion
   deliberately.
4. **Implementation effort** — not decisive; the adapter cost is real but bounded.

## Consequences

The audit can state the property as a single question about the dependency graph, and a
violation is named at a code path rather than inferred from behavior. Masking, the
visibility semi-join and the audit chain apply uniformly, so a data-release question has
one answer across every surface.

The cost accepted: a surface that holds the only handle to data pays for going through the
stack. That is a network hop where the data is local, or a generated artifact carrying a
staleness check where a direct read would have been immediate. Some legitimate operational
needs — a fast bulk export, an offline diagnostic — are therefore slower or more elaborate
than they would otherwise be, and an operator facing an unmediated requirement has no
switch to reach for.

Reversing this is unusually expensive: once code is written assuming mediation is
unconditional, adding a disable path means re-establishing what each layer guarantees under
both settings, and every audit finding since would need re-reading.

## Revisit triggers

- An adapter through the stack cannot meet a stated throughput requirement for a surface
  the product needs, and no in-stack optimization closes the gap.
- The audit reports bypasses that are all in one category of tooling, indicating the stack
  lacks an interface that category legitimately needs.
- A layer of the stack is found to be a no-op on some path, meaning traversal is being
  satisfied without enforcement being applied.
