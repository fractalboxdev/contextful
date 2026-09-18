# 0198 — The pinned key's own scheme decides verification, and a credential's algorithm claim is cross-checked against it

**Status:** accepted 2026-09-18
**Decides:** `authority.issue.refusal.algorithm-mismatch`

## Context

A credential carries a signed algorithm claim naming Ed25519, the default, or ECDSA over the
P-256 curve. Two schemes exist because custody drives the choice: Ed25519 is what a seed file
and a software signer offer, and P-256 is what a hardware module, a platform keychain and
several cloud key-management services offer. A deployment's custody posture decides which
scheme its issuer key uses, and the credential has to say enough for a verifier to parse it.

The claim being present raises the question the wire format has to answer: does the claim
select the verification routine, or does the pinned key?

Letting the claim select is the familiar shape and the familiar failure. A verifier that
dispatches on a self-described field lets the credential choose how it is checked. Every
weakening the verifier supports becomes reachable by a credential asking for it — a claim
naming a scheme the issuer never used, or naming none at all, and the verifier obligingly
runs the routine that says yes. The signature covering the claim does not help, because the
signature is checked by the routine the claim just chose.

The pinned key is the other candidate, and it has a property the claim does not: it was
placed by the verifying deployment, not by the party presenting the credential.

## Decision

The pinned key's own scheme is authoritative. A verifier checks a credential's signature
under the scheme its pinned key carries, and cross-checks the credential's algorithm claim
against it; a claim naming a different scheme raises `SignatureAlgorithmMismatch`. No
credential selects the scheme its signature is checked under, so no downgrade is expressible.

## Options considered

| Option | What it buys | What it costs |
| --- | --- | --- |
| **The pinned key decides; the claim is cross-checked** *(chosen)* | Scheme selection is made by the verifying deployment, so the downgrade is not merely refused but unsayable, and the mismatch is named rather than presenting as a bad signature | Two schemes each carry their own routine and their own tests, and the cross-check is a rule an implementation has to actually run — a verifier that dispatches on the claim is a plausible-looking implementation of the same wire shape |
| Dispatch verification on the credential's algorithm claim | The standard shape; one verifier serves a mixed key population with no per-key knowledge | Lost outright on downgrade: the presented credential picks its own verifier, so every weaker routine the implementation supports is reachable by asking for it |
| Carry no algorithm claim at all; infer everything from the key | Nothing to disagree; no cross-check to forget to run | Lost on diagnosability: a credential minted under the wrong scheme fails as an invalid signature, which reads as tampering or corruption rather than as the configuration error it is |
| Pin one scheme for the whole corpus | One routine, no claim, no cross-check, no mismatch | Lost on custody: hardware modules, platform keychains and several managed key services offer P-256 and not Ed25519, so a single scheme decides which custody postures exist |
| Accept a set of schemes per verifier and admit any member | A mixed rotation across schemes works with no coordination | Lost on the same downgrade criterion, narrowed: the set is chosen once and a generous set is indistinguishable at admission from a deliberate one |

## Criteria

1. **Whether a downgrade is expressible** — whether anything a presenter controls influences
   which routine checks its signature. *This criterion decides.*
2. **Custody coverage** — which signing custodians the scheme choice permits.
3. **Diagnosability of a scheme mismatch** — whether the refusal names the cause.
4. **Implementation and test surface** — how many routines exist.

Criterion 1 outranks custody coverage even though coverage is what forced two schemes. The
reason is asymmetry of consequence: a missing custody posture is a deployment that cannot be
built, which is visible and blocking, while an expressible downgrade is a deployment that
works and admits credentials nobody minted. Criterion 3 is why the claim stays on the wire
at all — without it the same misconfiguration is indistinguishable from tampering.

## Consequences

Scheme agility belongs to the operator who places the key, and a credential carries its
scheme as an assertion to be checked rather than as an instruction to be followed. Adding a
third scheme later is a key-side change: a verifier pinned to a key of that scheme uses it,
and credentials claiming it against a key of another scheme are named and refused.

The cost accepted is the cross-check's fragility as a rule. It is one comparison, it is not
load-carrying in any single request — the pinned key already decided the routine — and an
implementation that omits it still verifies correct credentials perfectly. What it loses is
the named refusal, and an omission that produces no wrong answers is an omission that tests
must catch rather than usage.

Reversing toward claim-directed dispatch is a small change in a verifier and reopens the
downgrade class in full.

## Revisit triggers

- A third signature scheme is required by a custody posture, which tests whether the pinned
  key remains a sufficient selector across a mixed population.
- A rotation across schemes is needed at a verifier pinning a key set, where the set spans
  two schemes and the per-key selector has to be exercised rather than assumed.
- A verifier is found dispatching on the claim, which would argue for removing the claim
  rather than for cross-checking it.
