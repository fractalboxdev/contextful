# 0207 — A rotation grace window shorter than the effective issuance ceiling is refused

**Status:** accepted 2026-09-18
**Decides:** `authority.revoke.refusal.short-grace`

## Context

The signing key that verifies capability credentials rotates every 90 d. Rotation is not
withdrawal: the retiring version stays verifiable for a declared grace window, and
compromise response runs on the scoped epoch and the immediate-retire path instead. The
grace window exists for exactly one population — credentials already minted under the
retiring version and still inside their lifetime when the new version takes over.

A project persists an issuance policy naming the longest lifetime any mint path produces,
at most 24 h. Every mint path is bounded by that number: an explicitly requested lifetime
above it is refused outright, and a configuration-derived exchange lifetime clamps down to
it. So the ceiling is the exact upper bound on how long a credential minted an instant
before a rotation can still be presented.

That makes the relationship arithmetic rather than judgment. A grace window below the
ceiling leaves an interval in which a credential is inside its own validity and its
signing key is no longer verifiable. The holder did nothing wrong, the checkpoint is
behaving correctly, and the failure appears as an unexplained `SignatureInvalid` at a
random point after a rotation the holder never observed.

Two things make the pairing easy to get wrong. The ceiling and the cadence are declared
in separate places by separate acts — the issuance policy and the rotation policy — and
the ceiling moves. Lowering it records the previous value and the instant of the
lowering, and credentials minted under the old, larger value stay live until they can
have lapsed. The number a grace window has to cover is therefore the effective ceiling,
not whatever the policy currently reads.

## Decision

A declared grace window shorter than the effective issuance ceiling raises
`RotationGraceTooShort`, and so does an explicit override below that ceiling. The
validation runs against the persisted issuance policy rather than against a flag, and
against the recorded previous value while credentials minted under it can still be live.
Raising the ceiling prints a reminder to re-validate the rotation grace, so the two
numbers cannot drift apart unobserved.

## Options considered

| Option | What it buys | What it costs |
| --- | --- | --- |
| **Refuse a grace window below the effective ceiling, override included** *(chosen)* | A credential valid on its own terms always verifies; rotation becomes invisible to holders | Rotation cadence is floored by the longest lifetime the project issues, so a long ceiling keeps retired key versions verifiable longer |
| Warn on a short window and rotate anyway | Nothing is blocked; the operator keeps full control of the cadence | Loses on the guarantee: the failure it warns about lands on holders as an unexplained signature refusal, far from the act that caused it |
| Allow an explicit override for operators who accept the gap | Covers a deliberate short rotation during an incident | Loses on redundancy: the incident case is already served by the scoped epoch and immediate retirement, which are designed to be abrupt, so the override buys only a way to do the wrong thing |
| Validate the window against a configured flag rather than the persisted policy | One local number to read; no cross-file dependency | Loses on truth: the flag can disagree with the ceiling every mint path actually enforces, and the validation then certifies nothing |

## Criteria

1. **Whether a credential valid on its own terms can fail to verify.** *(decided it)* The
   other criteria are about operator freedom and convenience. This one is about whether
   the credential's own expiry means anything. A holder reasons about a lifetime; if a
   rotation can shorten it invisibly, every lifetime becomes an upper estimate and no
   client can distinguish a lapse from a defect.
2. **Where the failure surfaces relative to the act that caused it.** A short window's
   damage arrives after the rotation, to a party with no view of it.
3. **Operator freedom over the rotation cadence.** Conceded: the cadence is floored, and
   an operator wanting faster rotation lowers the issuance ceiling first.
4. **Whether an urgent short rotation stays expressible.** Met by the epoch and the
   immediate-retire path, which is why the override loses nothing real.

## Consequences

Rotation becomes a background operation with no holder-visible effect, and a client can
treat its credential's expiry as the whole truth about its lifetime. The two policies
become coupled in a direction operators can act on: to rotate faster, shorten what you
issue.

The cost accepted is that a retiring key version stays verifiable for at least the
longest lifetime the project issues — up to 24 h at the maximum ceiling — so the interval
during which two key versions verify is floored by a number chosen for usability. That
interval is not a compromise window, because compromise runs on the epoch and on
immediate retirement rather than on waiting for a window to lapse, but it does mean a
routine rotation does not shrink the set of live signing keys as promptly as an operator
might expect.

Raising the ceiling is now expensive in a second place: it invalidates a grace window
that was correct when written, and the reminder exists because the failure is otherwise
silent until the next rotation.

## Revisit triggers

- The issuance ceiling is observed running at or near 24 h in real deployments, making
  the floored rotation interval a day wide in practice.
- An override request arrives that the epoch and immediate-retire path genuinely do not
  serve.
- Effective-ceiling tracking across a lowering is measured to be the source of refusals
  on windows that are in fact adequate, indicating the recorded-value guard outlives its
  usefulness.
