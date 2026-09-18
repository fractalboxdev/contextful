# 0134 — A credential backend holds opaque versioned ciphertext and no provider know-how

**Status:** accepted 2026-09-18
**Decides:** `secret.resolve.refusal.know-how-in-an-adapter`

## Context

Turnover needs two things that live in different places. Storage is generic: ciphertext under
a logical name, with versions, written by something and read by something else. The rules
that govern the bytes are specific to one vendor — how long a token lives, which endpoint
exchanges a refresh token, which error code means expired, which header carries the
rate-limit window, how many requests may be in flight, how a cursor is spelled.

A credential store is the obvious place to put the second set, because the store is where
the credential already is. Managed stores encourage it: several offer rotation functions
that hold a provider's exchange shape. Once an adapter exposes such an entry point, the
store has learned the vendor, and every other adapter has to learn the same vendor before a
deployment can move between them.

The connector is the other candidate. It already carries the vendor's auth policy, its
rate-limit and retry blocks, and its code, all pinned by connector id and version. A vendor
that changes a lifetime or an error code is a change to knowledge the connector already
owns.

The choice is therefore not where the knowledge can live but which of the two artifacts a
vendor change should move through: a versioned connector that a deployment already re-pins
routinely, or a credential store that a deployment redeploys rarely and under review.

## Decision

A credential backend holds opaque versioned ciphertext under a logical name and nothing
else. Token lifetime and exchange rules, the refresh endpoint and its scopes, expired-token
error codes, rate-limit headers, in-flight caps, retry policy, pagination and cursor kind
live in the connector, pinned by connector id and version. An adapter exposing a
provider-specific exchange, refresh or lifetime entry point raises
`SecretProviderKnowHowInBackend` at load. The host drives the OAuth loop from the
connector's declared auth policy and writes the result back as a blind versioned put; the
manager learns neither the provider nor the lifetime. A vendor changing a lifetime, an
error code or a rate-limit header is answered by a connector version bump.

## Options considered

| Option | What it buys | What it costs |
| --- | --- | --- |
| **Know-how in the connector, bytes in the store** *(chosen)* | A vendor change is a connector version bump; adapters stay swappable, since every one of them implements the same two verbs | The host drives the refresh loop itself, so the store cannot warn that a token is near expiry |
| Provider-specific rotation engines inside the store | Rotation runs where the credential lives, with no material crossing into the engine for a refresh | Lost on swappability: the store learns every vendor's exchange shape, and changing backends means reimplementing all of them against a different managed rotation model |
| Split the know-how between store and connector | Each side holds what it is naturally good at — the store the schedule, the connector the exchange | Lost on ambiguity: two places to update on a vendor change, and no rule stating which wins when they disagree |
| One adapter per provider-and-store pair | Every combination works exactly as its vendor needs | Lost on multiplication: the adapter count is vendors × stores, and a new store has to be written against every vendor before a deployment can move to it |

## Criteria

1. **Blast radius of a vendor change** — what a deployment redeploys when a vendor shortens
   a lifetime or renames an error code. *This is the criterion that decided it.* Vendor
   changes arrive unannounced and frequently; a credential-store redeploy is the slowest,
   most reviewed change a deployment makes. Putting a frequent, small, vendor-scoped edit
   behind that path means it lands late or does not land.
2. **Adapter swappability** — whether a deployment moves between managed stores without
   reimplementing anything. The two-verb interface makes the answer yes by construction.
3. **Where material sits during a refresh** — the rotation-in-the-store option wins here,
   since the exchange happens without the bytes crossing into the engine. It loses anyway,
   because the engine already hydrates that credential for ordinary reads, so the refresh
   is not what first brings material into the process.
4. **One place to update** — that a reader of a vendor's behavior has a single artifact to
   open. The split option fails this directly.

## Consequences

Adding a managed store is writing two operations against it, with no vendor knowledge
involved, and a deployment running on one manager links none of the others. A vendor's
behavior is readable in one artifact, pinned by version, and a run that misbehaves can be
tied to the connector version that governed it.

The accepted cost: the host drives the refresh loop itself and writes back as a blind
versioned put, so the store cannot warn that a token is near expiry. An opaque `expires_at`
hint the host supplied beside the ciphertext is the only clock the store carries, and it is
a number the host put there rather than a lifetime the store read. A managed store's own
expiry notifications, dashboards and near-expiry alarms do not fire for these entries, and
what an operator sees instead is the inventory command reading records.

Reversing this is expensive in one direction only. Moving know-how out of connectors and
into adapters later means every adapter gains every vendor, which is the multiplication the
fourth option was rejected for.

## Revisit triggers

- A managed store offers a rotation interface that is generic over providers — taking the
  exchange as data rather than as store-side code — at which point the swappability
  objection no longer holds.
- The count of refresh failures that a store-side expiry alarm would have caught, and the
  host-driven loop did not, is large enough to outweigh the redeploy cost.
- A vendor's exchange requires material the engine is not permitted to hold, forcing the
  exchange to run inside the store's own boundary.
