# 0133 — A sidecar that derives a short-lived credential is a lease provider; one that forwards a stored credential is refused

**Status:** accepted 2026-09-18
**Decides:** `secret.resolve.refusal.forwarding-sidecar`

## Context

The decoupling postures order deployments by how far credential material sits from the
engine. The strongest of them, the broker posture, says the engine dispatches intent and a
component inside the customer boundary puts the credential on the request. The engine, in
that posture, never holds vendor material at all.

Two very different components both present as "a sidecar the engine talks to". One reads a
long-lived credential out of a store and hands the bytes back over a local socket. The
other mints, exchanges or signs — it takes a scope and returns material that expires,
material the engine could not have obtained by reading anything. From the wire both look
identical: a request goes out, material comes back.

The difference is what the engine ends up holding. In the first case it holds the same
long-lived credential a plaintext-at-rest deployment holds, arrived by a longer route. Every
guarantee the broker posture offers — no standing vendor credential in the engine's memory,
a compromise bounded by an expiry, revocation that bites at the next window — is absent, and
the deployment believes otherwise. In the second case the engine holds a grant with a clock
on it, which is exactly what the lease path already describes.

A related shape settles the same way from the other side. A deployment whose credential
store answers only on a platform-internal binding — reachable from the runtime and from
nowhere else — is often argued to be a lease deployment because the network position looks
privileged. What it does is serve stored bytes.

## Decision

A sidecar that reads a stored long-lived credential and forwards it outward raises
`SecretForwardingBroker`, at any distance and over any transport. A sidecar that mints,
exchanges or signs a short-lived scoped credential is a lease provider and stands. A
deployment whose credential store answers on a platform-internal binding alone is a broker
deployment, and every broker obligation binds it. The lease provider occupies no posture of
its own: it is the external-manager posture with the trust direction reversed.

## Options considered

| Option | What it buys | What it costs |
| --- | --- | --- |
| **Refuse forwarding, accept derivation** *(chosen)* | The posture a deployment claims matches what the engine holds; the engine's exposure to a stored credential is a property of the declaration rather than of the network path | A deployment whose secret infrastructure only reads and forwards adopts nothing until it grows a mint or exchange endpoint |
| Permit a forwarding sidecar over TLS | Existing read-and-forward infrastructure counts as a broker immediately | Lost on the criterion: the engine still holds a long-lived credential. Transport confidentiality bounds who else sees the bytes in flight and changes nothing about where they land |
| Permit forwarding on loopback only | Narrower than the TLS variant, and the bytes never leave the machine | Lost on the same criterion, one hop earlier. Loopback is where the strongest deployments run their mint endpoint, so distance cannot be what separates the two |
| Classify a platform-internal store as a lease deployment | No operator argues about which posture their managed store is | Lost on what the component does: it serves stored bytes, so the obligations that attach to holding stored bytes attach to it |

## Criteria

1. **What the engine ends up holding** — whether, at the end of a hydration, the engine's
   memory carries a long-lived credential or a grant with an expiry. *This is the criterion
   that decided it.* Every guarantee the broker posture is chosen for is a statement about
   what a compromise of the engine yields. A criterion that reasons about the path rather
   than the result cannot distinguish the two components, and the posture becomes a label.
2. **Transport confidentiality** — whether an observer between engine and sidecar reads the
   material. Real, and orthogonal: it bounds the observer, not the holder.
3. **Adoption effort** — how much infrastructure a deployment rebuilds to claim the posture.
   It outranks nothing here, because a posture claimed on rebuilt-nothing is the failure
   this record exists to refuse.
4. **Legibility to an operator** — whether a reader can tell from the declaration which
   posture is live. Served by the same test the first criterion uses.

## Consequences

Classification is a property a reviewer can check from the sidecar's interface: an endpoint
that takes a name and returns bytes is forwarding, one that takes a scope and returns
material plus an expiry is a lease provider. The postures stay four rather than growing a
fifth for leasing, so a deployment reasons about distance and about lifetime separately.

The accepted cost: a deployment whose existing secret infrastructure only reads and forwards
needs a mint or exchange endpoint before it can adopt the lease posture. That is real work
against a component the deployment already operates and already trusts, and the refusal
gives it nothing in return until the endpoint exists. Such a deployment sits in the
external-manager posture meanwhile, which is the posture it gets with no further choice.

The refusal fires at any distance, so a deployment cannot reach the broker posture by moving
a forwarding component closer to or further from the engine. Reversing that would mean
deciding how much distance is enough, a number nothing in the threat model supplies.

## Revisit triggers

- A credential form appears whose lifetime is short but which is stored rather than minted,
  so that reading it out of a store yields material already bounded by an expiry.
- A platform binding appears that constrains what a reader can do with the bytes it returns
  — a use-bound token rather than a stored one — making "serves stored bytes" no longer the
  right description of a platform-internal store.
- The proportion of deployments blocked from the lease posture by the missing mint endpoint
  is large enough that the posture is aspirational; the count per deployment is observable
  from which backend each selects.
