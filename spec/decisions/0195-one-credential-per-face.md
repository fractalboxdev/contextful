# 0195 — A served face admits a verified capability credential and nothing else

**Status:** accepted 2026-09-18
**Decides:** `authority.issue.invariant.one-credential`

## Context

A capability credential carries a subject tuple, a set of grants, a validity window, an
audience and a possession-proof thumbprint. A checkpoint turns those into an admitted
authority, and every read surface and every row-landing effect takes that value as an
argument. Placement policy, table restriction, the audit record and per-principal pinning
all read members of it.

A shared secret compared by equality carries none of that. It has no subject, so there is
nothing to attribute a row to and nothing for an identity link to join on. It has no grants,
so the only sensible reading of a match is full authority. It has no expiry, so it is live
until an operator rotates it, and it has no audience, so a copy taken from one deployment
presents unchanged at another. It also has no possession proof: the secret is the whole
credential, so capturing a request captures the authority.

The pressure to keep one is real. A gateway in front of the engine has already
authenticated its caller and wants a cheap way to say so. An operator wants an escape hatch
when credentials are misconfigured. A health check wants a path that works before anything
is provisioned. Each of those is a request for a second admission path whose properties are
the ones above.

The owner authors raw statements. A secret that admits as the owner therefore drags an
owner-equivalent surface onto whatever network reaches the face, and it does so with no
subject in the audit record explaining who used it.

## Decision

A served face admits one thing: a verified capability credential. No static perimeter
bearer, no shared secret between a gateway and the engine, and no unauthenticated owner
path exists, behind a flag or otherwise. A gateway that has authenticated its caller trades
that assertion for a credential rather than asserting it by possession of a secret.

## Options considered

| Option | What it buys | What it costs |
| --- | --- | --- |
| **One admission path — a verified capability credential** *(chosen)* | Every admitted request yields a subject, a grant set, an expiry and an audit entry, and there is one path to reason about | Everything a caller needs has to be sayable as a grant, so surfaces that reached beneath the tables need grant-filtered tools reading through the registered relation |
| Keep a perimeter secret as an opt-in, off by default | Covers the gateway and bootstrap cases with no new machinery | Lost on what the credential carries. An opt-in nobody disables is a default with extra steps, and while it is on the face has a subjectless, expiryless, audience-free owner-equivalent path |
| Allowlist by network position — a trusted subnet or a local socket | No credential to distribute for the co-located gateway case | Lost on the same criterion: it authenticates a position rather than a principal, so the audit record names a network address and per-principal policy has nothing to read |
| Accept a gateway-signed assertion directly at each request | The gateway's work is not repeated, and the assertion does carry a subject | Lost on read-path independence and on uniformity: every read surface would then check two formats with different validity rules, and the engine would trust an issuer it does not pin for the population of its choice |
| Keep an unauthenticated owner path on a loopback interface only | An operator is never locked out | Lost on threat: anything running on the host reaches loopback, including the agent processes this engine exists to serve, and an owner path admits the one authority that authors raw statements |

## Criteria

1. **What the admitted thing carries** — subject, grants, expiry, audience, possession
   proof. *This criterion decides.*
2. **Number of admission paths to reason about** — how many ways a request becomes
   authority.
3. **Blast radius of a captured or copied credential.**
4. **Coverage of the gateway and bootstrap cases** without new machinery.

Criterion 1 decides because everything downstream of admission is written against the value
admission produces. Placement policy, row restriction, authorship and audit are not
decorations on top of an allow decision; they are reads of subject members. A path that
admits without producing those members does not bypass one check, it empties the model the
whole enforcement side is written in terms of. Criterion 4 is the live cost and is answered
by exchange rather than by a second path.

## Consequences

One path becomes one place to get right: the possession proof, the audience check, the
expiry re-read at each effect and the audit entry apply to every admitted request without
exception. A gateway integrates by trading its verified assertion for a short-lived,
least-privilege credential, so the caller it authenticated is the principal the rows carry.

The cost accepted is expressiveness pressure on grants. Anything an operator or a tool needs
has to be sayable as a grant over the registered relation, which is why the bare-star table
pattern exists, and surfaces that previously reached beneath the tables need grant-filtered
tools built for them. Some of those tools are work that a perimeter secret would have made
unnecessary.

Reversing this is easy to type and hard to undo: once a deployment is reachable with a
secret, every holder of that secret has been owner-equivalent for as long as it was live,
and the audit record cannot say who they were.

## Revisit triggers

- A legitimate caller shape appears that cannot be expressed as a grant over the registered
  relation, after the grant-filtered tools for it have been attempted.
- Bootstrapping a deployment is found to require a live face before any credential can be
  minted, which would be a circularity the single path does not resolve.
- Exchange latency at a gateway measurably dominates request cost, which would put the
  per-request trade under real pressure rather than anticipated pressure.
