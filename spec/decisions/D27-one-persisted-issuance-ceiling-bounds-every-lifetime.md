# D27 — One persisted issuance ceiling bounds every lifetime

**Status:** accepted

## Context

A credential's lifetime bounds a stolen credential, the rotation grace a retired key must stay verifiable, and the earliest instant a withdrawn format can stop verifying. Minting and verification carry no network dependency, so the bound travels with the project rather than with a service.

## Decision

One number, versioned with the project, sets the longest lifetime every mint path produces, and every other time window is derived from it.

- `authority.issue` reads a persisted issuance policy tracked in version control: a ceiling of at most 24 h and a default audience.
- An explicitly requested lifetime above the ceiling refuses; `authority.exchange` clamps a configured lifetime down to the ceiling, with an exchange ceiling of 3600 s, and never up.
- `authority.revoke` refuses a rotation grace window shorter than the effective ceiling, override included, and validates against the recorded previous ceiling while credentials minted under it can be live.
- Withdrawing a credential format runs through revocation, expiry or key rotation at a declared cutover instant; no flag or rollback restores it.

## Options

| Option | Lost on | Cost |
| --- | --- | --- |
| One persisted ceiling; refuse explicit excess, clamp configured excess *(chosen)* | — | A ceiling change is a commit and deploy; one condition has two behaviors; a clamped deployment gets no signal beyond the credential's expiry. |
| Per-mint flag or environment variable | Uniformity | The ceiling would differ per host and be invisible in review. |
| A policy service consulted at mint | Offline minting | Issuance would inherit a network availability requirement. |
| Clamp explicit requests silently | Caller expectation | A caller would plan work around a lifetime it does not hold. |
| Refuse a configured lifetime above the ceiling | Availability | A stale policy value would become a total sign-in outage. |

## Consequences

- Rotation cadence is floored by the longest lifetime the project issues.
- A format withdrawal cannot be immediate under the ordinary instruments.
- Raising the ceiling prints a reminder to re-validate the rotation grace.

## Revisit

- A deployment needs lifetimes above 24 h for a verified reason.
- Operators are observed surprised by clamped exchange sessions.
