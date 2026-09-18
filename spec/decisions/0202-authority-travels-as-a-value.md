# 0202 — Verification produces an admitted-authority value every read surface and every row-landing effect takes as an argument

**Status:** accepted 2026-09-18
**Decides:** `authority.verify.invariant.admitted-authority`

## Context

Admission is where bytes become authority. What happens after admission is where authority
becomes consequence: a read surface narrows rows through the registered relation a grant
compiles into, a write lands rows stamped with the verified principal, a placement check
compares the caller's zone against what the data admits, and an audit record names the
subject tuple that was carried.

Each of those steps needs the same thing — the admitted subject and the grants it narrows to
— and each of them could get it in one of two ways. It can be handed the value, or it can go
and find it.

Going and finding it is what makes a daemon dangerous. A face serving several principals at
once is one process holding, at various moments, whatever the last admission produced. If an
effect reads the environment, re-parses whatever credential is lying around, or consults a
task-local, then two callers writing through that one daemon are two writes resolving
whatever the process holds. The resulting rows are well formed, carry a verified principal,
and name the wrong person. Nothing downstream can detect it, because the value is genuine.

The same reasoning applies to fabrication. An effect that can construct an authority is an
effect that can act under one no verification produced, and every check upstream of it
becomes advisory.

## Decision

Verification produces an admitted-authority value carrying the normalized subject tuple and
the grants it narrows to. Every read surface and every row-landing effect accepts that value
as an argument; nothing downstream re-reads the environment, re-parses a credential or
consults a task-local. Two callers writing through one daemon carry two authors, and neither
carries the daemon's.

## Options considered

| Option | What it buys | What it costs |
| --- | --- | --- |
| **Verify once, carry the result as an argument** *(chosen)* | Attribution is exact on a face serving many principals, and an effect's authority is visible in its signature rather than in its environment | The admitted type has to expose no constructor that fabricates one, or the property is unenforced; and every effect in the authority path carries an extra parameter |
| Re-read the environment or a task-local at each effect | No parameter threading; an effect reaches authority from anywhere | Lost on attribution: two callers writing through one daemon both pick up whatever the process holds, and the rows that result are indistinguishable from correct ones |
| Re-verify the credential at each effect | Freshest possible check; no stale value carried | Lost on cost and on what it does not fix: signature checks per effect multiply the per-request work, and an effect that re-verifies can still be handed a credential it chose, so fabrication stays open |
| Carry the raw credential and let each effect parse it | One value threaded, no new type to define | Lost on uniformity of interpretation: normalization, grant narrowing and timestamp decoding would run per effect, so two effects can read one credential two ways — the failure the single timestamp grammar exists to prevent |
| Make the value ambient but bind it per request at the face | Threading stays out of internal signatures | Lost on enforceability: ambient binding is a convention, and an effect reached from a path that forgot to bind picks up the previous request's value with no error |

## Criteria

1. **Attribution exactness on a face serving several principals concurrently** — whether an
   effect can act under the wrong verified principal. *This criterion decides.*
2. **Whether an effect can act under an authority no verification produced.**
3. **Per-request verification cost.**
4. **Call-site burden** — how many signatures carry the value.

Criterion 1 decides because its failure is silent and unrecoverable. A wrong-principal write
produces a row that is well formed, carries a genuinely verified principal, satisfies every
downstream check and is wrong — and per-principal placement policy will then pin it
according to a person who had nothing to do with it. Criterion 2 is close behind and is
addressed by the same structure, provided the type refuses to be constructed. Criterion 4 is
the accepted cost and is mechanical.

## Consequences

An effect's authority is part of its signature, so reading what an effect acts under means
reading its parameters. A daemon serving many callers keeps them separate by construction
rather than by discipline. The audit record names the subject that was admitted for that
request, not the one the process most recently saw.

Two costs are accepted. The first is structural: the admitted type must expose no
constructor producing a value without a verification — no default, no assume-admitted escape
— because a single such constructor makes the property a convention again. The second follows
from admission being a moment while authority is re-read at each effect: expiry, the
revocation epoch and the policy version are evaluated against the carried value at the effect
about to act, so an execution long enough to straddle a lapse stops mid-way rather than
completing on the state it began under. Partial completion is the price of not acting on
lapsed authority.

Reversing toward ambient authority is easy to write and removes the one property that keeps
a multi-principal face honest.

## Revisit triggers

- Parameter threading blocks a legitimate effect shape that cannot receive the value —
  a callback invoked by a subsystem that has no place to carry it would be the case.
- Long-running executions straddling a lapse become common enough that partial completion is
  an operational problem rather than a rare one.
- A constructor producing an admitted authority without a verification appears anywhere in
  the tree, which would mean the property is already unenforced rather than merely at risk.
