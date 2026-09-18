# 0192 — An explicitly requested lifetime above the ceiling is refused, and a configuration-derived one is clamped

**Status:** accepted 2026-09-18
**Decides:** `authority.issue.refusal.lifetime-above-ceiling`

## Context

Two things ask for a lifetime, and they differ in who is present to be told the answer.

A caller asks explicitly. It names a number at the mint, it holds the returned
credential, and it plans around what it believes it has: a run that expects to last
hours, a job that schedules its own refresh, a consumer that caches. The caller is right
there, synchronously, able to receive an error and act on it.

The exchange derives a lifetime from configuration. An exchange policy carries a
time-to-live, applied to every credential minted for every reader signing in through the
embedding application. Nobody is present at that mint in any meaningful sense — the
reader is signing in, the operator who wrote the policy is elsewhere, and the credential
is a step in a sign-in flow rather than a thing anyone asked for by number.

Both can exceed the persisted ceiling. The ceiling is the project's statement of the
longest lifetime any mint path produces, and it can be lowered after a policy was
written. So the same condition — requested lifetime above the ceiling — arrives in two
situations with different consequences: a caller that would silently plan on a lifetime
it does not have, and a sign-in path that would fail for every reader over a number an
operator can fix in one line.

## Decision

An explicitly requested lifetime above the persisted ceiling raises
`IssuanceLifetimeAboveCeiling` and is never quietly shortened. A configuration-derived
exchange lifetime is clamped down to the ceiling instead. The ceiling holds in both
cases; what differs is whether the condition is reported or absorbed.

## Options considered

| Option | What it buys | What it costs |
| --- | --- | --- |
| **Refuse the explicit request; clamp the configured one** *(chosen)* | A caller learns its expectation is unmeetable at the moment it asks. A sign-in flow keeps working through a policy that has drifted above the ceiling. | Two behaviors for one condition, which a reader of the system has to learn; and a clamped deployment runs with no signal at the mint beyond the credential's own expiry. |
| Clamp both | One rule, always available, never fails a mint. | Loses on expectation: a caller that asked for twelve hours believes it holds twelve hours, plans a long-running unit of work around it, and discovers the truth at an expiry in the middle of that work. |
| Refuse both | Perfectly uniform, and every mismatch is visible. | Loses on availability: an exchange policy whose value drifted above a lowered ceiling would fail every reader's sign-in, turning an operator's stale number into a total outage of the read face. |
| Return the granted lifetime and leave the caller to inspect it | Honest, uniform, and never fails. | Loses on expectation again, in a subtler way: it makes correct behavior optional, and a caller that does not inspect behaves exactly as in the silent-clamp case. |
| Refuse the explicit request, and refuse the exchange only at policy load | The operator learns at load rather than at a reader's sign-in. | Loses on when the condition arises: the ceiling can be lowered after the policy loaded, so the mismatch appears while the process is already running and serving. |

## Criteria

1. **Whether the holder learns its actual lifetime before relying on it** — whether a
   party can act on a belief about a credential that the mint quietly contradicted.
   *This criterion decides.* A shortened credential is safe; a caller that thinks it is
   longer is not, and the failure surfaces mid-work rather than at the mint.
2. **Availability of the sign-in path** — whether one stale configured number can take
   the read face out of service for every reader.
3. **Uniformity of behavior for one condition** — one rule rather than two. Given up.
4. **Where an operator's mistake surfaces** — at the operator, ideally, rather than at a
   reader.

## Consequences

Callers that request lifetimes get a hard, named error they can handle, and can probe
the ceiling by asking for what they want and reading the refusal.

The exchange keeps serving readers through a ceiling change, which matters most exactly
when the ceiling is being tightened — the one moment where a failing sign-in path would
be attributed to the tightening and reverted.

The accepted cost is silence on the clamped path. A deployment whose exchange policy
exceeds the ceiling issues shorter credentials than its own configuration says, and
nothing at the mint says so; the only evidence is the expiry on the credentials
themselves. An operator reading the policy file gets a number that is not what is
happening.

The asymmetry is also a hazard in itself. Anyone adding a third mint path has to decide
which side of this line it sits on, and the criterion — is there a caller present to be
told — is the thing to apply, not the list of two cases.

## Revisit triggers

- A configuration check runs at policy load and on every ceiling change, surfacing the
  mismatch to the operator, which would remove the silence that is the accepted cost
  here and make refusing both viable.
- A third mint path appears whose caller presence is ambiguous, forcing the criterion to
  be stated as a rule rather than applied case by case.
- Clamped exchanges are observed producing credentials too short for readers' sessions,
  making the silence operationally expensive rather than merely untidy.
