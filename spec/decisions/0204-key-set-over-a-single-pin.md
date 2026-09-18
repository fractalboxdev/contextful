# 0204 — A verifier accepts a set of issuer keys, reached as static pins or as a published key route

**Status:** accepted 2026-09-18
**Decides:** `authority.verify.interface.key-set`

## Context

The signing key that verifies capability credentials rotates every 90 d, and immediately on
suspected compromise. A verifying checkpoint holds public key material and no secret, and
admission runs with no call to an issuer or a policy service — that independence is what
keeps issuer availability off the read path.

Rotation and a single pinned key do not compose. Credentials are minted continuously and
live up to the issuance ceiling, so at any moment during a rotation there are live
credentials signed by the old key and live credentials signed by the new one. A verifier
holding one key admits exactly one of those populations. Making the switch means refusing
everything outstanding, or coordinating every verifier and every minting path to flip within
the same instant. At a 90 d cadence that is a recurring flag day, and on the compromise path
it is a flag day under pressure.

Accepting a set resolves it: both keys are trusted for the overlap, and the old one is
removed once nothing signed by it can still be live. The remaining question is how a verifier
learns the set's contents, and the two answers differ in what they demand of the operator.
Comma-separated static pins are a configuration change per rotation, applied wherever
verifiers run. A key route published by the project — unauthenticated, per-key opt-in,
returning the current and the non-retired versions — makes the rotation self-propagating and
introduces a dependency with its own availability.

## Decision

A verifier accepts a set of issuer keys rather than a single key. Comma-separated static pins
cover a manual rotation; a project-published key route — unauthenticated, per-key opt-in,
returning the current and the non-retired versions — covers a scheduled one. A deployment
picks the source that matches its rotation cadence, and both produce a set the verifier
treats identically.

## Options considered

| Option | What it buys | What it costs |
| --- | --- | --- |
| **A set, sourced from static pins or an opted-into published route** *(chosen)* | A rotation has an overlap window in which both populations admit, and a deployment chooses whether it pays in operator effort or in a fetched dependency | A set trusts every key in it, so retiring a key is an action rather than the absence of one, and the published route carries the availability and staleness behavior the verifier must handle |
| A single pinned key | Simplest possible verification; exactly one thing is trusted | Lost on rotation: no overlap is expressible, so a 90 d cadence means recurring flag days and a compromise rotation refuses every outstanding credential |
| Static pins only, no published route | No fetch, no availability question, full offline verification | Lost on operator burden at a scheduled cadence: every verifier is reconfigured and redeployed per rotation, and the rotation is only as prompt as the slowest deployment to pick it up |
| A published route only, no static pins | One place to change a key; nothing to distribute | Lost on the offline and local shapes, and on startup dependency: a deployment running one key from disk would need a key service before it can verify anything, and the route becomes required infrastructure for every profile |
| Fetch the key per verification rather than caching a set | Always current; a retirement takes effect at the next request | Lost on read-path independence: it puts an issuer-side service in the per-request path, which is the property public-key-only verification exists to preserve |
| Trust any key the issuer presents alongside the credential | No distribution at all; rotation is invisible | Lost outright on what pinning is for — the presenting side would choose the key its signature is checked against |

## Criteria

1. **Whether a rotation has an overlap window** — whether outstanding credentials keep
   admitting while the signing key changes. *This criterion decides.*
2. **Read-path independence** — whether admission requires a call to an issuer or a policy
   service.
3. **Operator effort per rotation**, at the 90 d cadence and on the compromise path.
4. **Viability of an offline or single-key deployment** with no key service.

Criterion 1 decides because without an overlap the rotation cadence is not a policy, it is a
scheduled outage. Everything else here is a choice about how the set is supplied; the set
itself is what makes rotating at all compatible with credentials that outlive the switch.
Criterion 2 is why the route is cached with a refresh rather than consulted per request, and
criterion 4 is why static pins remain a first-class source rather than a fallback.

## Consequences

Rotation becomes routine. A deployment on static pins adds the new key, waits out the
outstanding credentials, and removes the old one. A deployment on the published route picks
up a new key at the first credential minted under it, without a redeploy. Both keep
verification a local operation against material the deployment holds.

Two costs are accepted. A set trusts every key in it, so a compromised key keeps admitting
until it is removed everywhere — retiring is positive work, and forgetting it leaves a key
live that nobody is minting under. And the published route is a dependency: it is refreshed
on a 300 s lifetime with a single-flight, throttled failure-forced refresh, it serves the
last-known-good set only while total age stays under 1 h, and opting into it and then failing
to obtain one refuses every credential — the engine declines to start and a gateway answers
`503`. A static pin rescues none of that, because no credential a caller could present would
succeed.

Reversing toward a single key is a configuration change that reintroduces the flag day at the
next rotation.

## Revisit triggers

- Key-set refresh failures reach a rate where the staleness ceiling is hit in normal
  operation rather than during an incident.
- A rotation is found to have completed with a retired key still present in a deployed set,
  which would argue for the route carrying retirement more actively than by omission.
- The rotation cadence changes enough to alter the overlap window's relationship to the
  issuance ceiling, which is what makes the set's membership bounded in time.
