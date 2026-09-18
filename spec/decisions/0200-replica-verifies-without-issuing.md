# 0200 — A replica is provisioned with the issuer's public key and holds no signing material

**Status:** accepted 2026-09-18
**Decides:** `authority.issue.refusal.replica-mint`

## Context

A replica is a copy of a store placed where reading it is cheap — near a team, near an
agent fleet, on a machine that keeps working when the primary is unreachable. It exists to
serve reads, and serving a read means admitting a credential, which means holding verifying
material.

Verifying material is public. A checkpoint holds public key material and no secret precisely
so that compromising one mints no credential and widens none. That property is what makes
placing a replica somewhere less controlled an acceptable thing to do at all: the replica
holds data it is allowed to serve, and a key that checks signatures.

Signing material is the opposite. It is the one secret in the authority path, it lives at
one point, and holding it means being able to stamp any subject the issuance policy permits
— including the verified principal that rows are authored by. A replica that could mint
would be a copy of the store and a copy of the ability to manufacture access to it, sitting
wherever replicas get placed.

The pull toward letting replicas mint is availability. A replica exists partly so that work
continues when the primary is unreachable, and issuance is the one operation that then
cannot be performed locally.

## Decision

A replica is provisioned with the issuer's public key and holds no signing material; a mint
attempted there raises `ReplicaCannotIssue`. A credential minted by the primary admits at the
replica unchanged, so the replica serves every credential the primary issues without being
able to produce one.

## Options considered

| Option | What it buys | What it costs |
| --- | --- | --- |
| **Public key only at the replica; minting stays at the primary** *(chosen)* | A stolen replica yields data it was already allowed to serve and no ability to reach anything else | An operator at a replica cannot mint locally and depends on the primary's availability for issuance |
| Give each replica its own issuer key | Fully local issuance; no dependency on the primary for minting | Lost on verifiability: a credential minted at a replica either fails at the primary, or the primary pins a key it does not control and inherits every replica's custody as its own |
| Replicate the signing seed to every replica | One key population, local issuance everywhere | Lost outright on the threat this decision is about: the seed's blast radius becomes the union of every replica's physical and operational security, and a replica is placed where controls are weaker |
| Proxy mints from the replica through to the primary | Issuance appears to work locally; the seed stays put | Lost on the offline property replicas exist for: the proxy needs the primary reachable, which is the condition under which local issuance was wanted |
| Give a replica a short-lived delegated signing capability | Local issuance, bounded exposure | Lost on effort and on what it actually is: it is signing material with a timer, so a compromised replica mints freely inside its window, and the machinery to bound and revoke that window is a second issuance system |

## Criteria

1. **What a compromised replica yields** — whether taking one gets more than the data it
   already served. *This criterion decides.*
2. **Verifiability of a credential across the deployment** — whether one credential admits
   everywhere it should.
3. **Availability of issuance when the primary is unreachable.**
4. **Number of parties in custody of signing material.**

Criterion 1 decides because a replica's whole purpose is to be placed where the primary is
not — closer to users, on hardware with different controls, in a location chosen for latency
rather than for security. Every other option turns that placement decision into a decision
about where the system's only secret lives. Criterion 3 is the real cost and is bounded:
issuance is an administrative act at a rate measured in credentials per operator, while
reading is the per-request path, and reading keeps working.

## Consequences

Replicas can be placed on the merits — latency, locality, survivability — without each
placement being a custody question. A credential minted once at the primary admits at every
replica, so a reader travelling between them carries one credential. Compromising a replica
costs the data that replica served and nothing beyond it.

The cost accepted is that issuance is a single-point operation. A replica cut off from the
primary serves every outstanding credential until it lapses and can mint nothing new, so a
partition that outlasts the issuance ceiling leaves that site unable to onboard a caller. The
ceiling is at most 24 h, which sets the scale of that outage directly.

Reversing toward replica-side signing is not a configuration change: any credential minted at
a replica is a credential whose custody chain includes wherever that replica was placed, and
that cannot be established after the fact.

## Revisit triggers

- A deployment needs issuance at a site partitioned from the primary for longer than the
  issuance ceiling, as an operational reality rather than an anticipated one.
- Replica placement becomes as controlled as the primary's, which removes the asymmetry this
  decision rests on.
- A signing scheme appears that lets a replica produce a credential the primary can verify as
  bounded and replica-scoped, which would satisfy criterion 1 and criterion 3 together.
