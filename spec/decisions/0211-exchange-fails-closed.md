# 0211 — Every refusal on the exchange path mints nothing

**Status:** accepted 2026-09-18
**Decides:** `authority.exchange.refusal.assertion-invalid`

## Context

The exchange turns an external assertion into local authority. Its inputs are the
assertion, the operator's injected verifying material, and a policy naming the expected
issuer, an optional expected audience, a map from capability subject members onto verified
claims, a role claim, a role-to-grants map and a default grant set. Its output is a signed
capability credential that every later read admits without contacting the provider again.

That output is not recoverable. Once minted, the credential verifies against pinned issuer
keys for its whole lifetime, and no downstream layer re-examines the assertion that
produced it. Whatever the exchange decides is what the engine believes about the reader
from then on.

The path has several distinct ways to be partially valid, and each one is a different
temptation. A signature that does not check. An assertion past its own expiry. An issuer
or audience the policy does not name. And the quiet one: a mapped claim the verified
assertion simply does not carry, because the provider changed a claim template, or because
the subject map names a claim this provider spells differently.

That last case is where the failure direction matters most. The policy's default grants
exist for a verified role that matches no entry in the role map, and they are empty. But a
missing *mapped claim* is a different condition: the subject the credential would carry is
incomplete. Minting anything at all on that condition means a claim-template change on the
provider's side silently produces credentials with a subject nobody intended — and the
subject is what tenancy, authorship and the audit record are all built on.

Retrying against a second issuer is the other temptation, and it is what a client library
does by habit when a first check fails. The policy names one expected issuer. Trying
another is the exchange deciding, on its own, to trust something the operator did not
declare.

## Decision

A bad signature, a lapsed assertion, an untrusted issuer or audience, or a mapped claim
absent from the verified assertion mints nothing and raises `ExchangeAssertionInvalid`.
Every refusal on this path fails closed: no path through the exchange produces a
credential from an assertion that failed any check, and none produces a credential with a
subject member the assertion did not supply. The default grant set is reached only by a
verified role matching no entry in the role map, and it is empty.

## Options considered

| Option | What it buys | What it costs |
| --- | --- | --- |
| **Refuse on every failed check; no path mints** *(chosen)* | One property to verify: nothing that failed produces authority, and the subject a credential carries was present in a verified assertion | A provider-side claim-template change takes the exchange down until the subject map is updated, rather than degrading to a narrower credential |
| Mint with the default grants when a mapped claim is missing | Readers keep signing in through a provider-side claim change; the blast radius of a template edit is bounded | Loses on silent grant: a claim-template error becomes a mint carrying an unintended subject, and the default set's emptiness is an implementation detail one policy edit away from not being empty |
| Retry against a second configured issuer on failure | Survives a provider rotating its issuer without a policy edit | Loses on declaration: the policy names one expected issuer, and trusting a second is the exchange widening its own trust set |
| Mint a credential with the subject member omitted | The reader is admitted with whatever the assertion did supply | Loses on the same silent-grant criterion: tenancy and authorship read that member, and an absent member is not a narrower value, it is an unbound one |

## Criteria

1. **Whether any refusal path can produce a credential.** *(decided it)* Every other
   criterion trades availability against blast radius. This one determines whether the
   exchange has a property at all. A single path that mints on a failed check makes every
   statement about what the exchange guarantees conditional on an enumeration of paths,
   and that enumeration is what grows.
2. **Whether a subject member can be unbound.** Tenancy isolation rests on a
   parameter-bound filter over the account member; an unbound member has no correct
   reading.
3. **Whether trust widens without a declaration.** The issuer-retry option fails here.
4. **Availability of sign-in during a provider-side change.** Conceded, and it is the
   whole cost of this decision.

## Consequences

The exchange has one verifiable property, stated in one sentence and testable per failure
mode: an assertion that failed a check leaves no credential behind. Downstream contracts
can treat a minted credential's subject as having been present in a verified assertion,
which is what makes the tenancy filter's bound predicate meaningful.

The cost accepted is availability during a provider-side change. A provider that renames
or drops a claim the subject map names takes the exchange down for every reader until an
operator updates the map. There is no degraded mode: sign-in stops rather than narrowing.
For an embedding application that is a visible outage, and the recovery is a policy edit
rather than a retry.

Adding a degraded mint later would be cheap to implement and would retire the property
entirely, since the guarantee holds only while no path mints on a failure.

## Revisit triggers

- Provider claim-template changes are observed causing exchange outages more than once
  per provider per year, making the no-degraded-mode stance the dominant source of
  downtime.
- A subject member appears that is genuinely optional — one no filter, authorship record
  or audit entry reads — at which point its absence stops being an unbound value.
- A deployment needs two issuers simultaneously while a provider rotates its issuer, which
  the single expected issuer cannot express.
