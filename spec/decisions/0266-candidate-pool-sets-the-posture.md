# 0266 — Release posture follows the candidate pool, and a pool over other tenants' end users is off without a recorded consent contract

**Status:** accepted 2026-09-18
**Decides:** `disclosure.release.refusal.cross-party-pool`

## Context

A segment build takes a seed set, derives a profile from it, and ranks candidates out of a
pool by approximate nearest-neighbour search. The mathematics is identical whatever the pool
is. The same centroid, the same index, the same threshold at the requested size produce a
segment whether the candidates are the requesting tenant's own customers, a pool the
operator licensed and owns, or the end users of other tenants sharing the deployment.

What differs across those three is who is disclosed to whom. A tenant ranking its own base
learns about people it already holds rows for; nothing crosses a boundary and no new party
sees anything. A tenant ranking an operator-owned pool learns which of the operator's
candidates resemble its seeds, which is a disclosure from the operator to the tenant, and
the operator is present to consent to it — it owns the pool, its licensing and its consent
posture. A tenant ranking other tenants' end users produces, for every member of the
resulting audience, a disclosure to a party that person has no relationship with, consented
to by neither the person nor the tenant that holds them.

The tempting place to hang the posture is the method: score it this way and it is safe,
score it that way and it is not. That reads the wrong variable. Nothing about a centroid or
a nearest-neighbour cut changes who ends up holding information about whom; the pool does,
entirely, and the method is constant across all three cases.

The third case also has a consent problem the deployment cannot solve by configuration. The
people whose membership would be disclosed are not party to the deployment's manifest. A
flag set by an operator, or by a tenant, is a party asserting a permission it does not hold.
Whatever makes that case legitimate is an agreement recorded outside the engine, between the
parties who actually hold the relationship.

## Decision

Posture follows where candidates come from. A pool drawn from the requesting tenant's own
base crosses no owner boundary and carries no additional posture. A pool the operator owns
carries the audience-size gate, activation-only output and no readback, with consent and
licensing owned by the operator. A pool drawn from other tenants' end users adds per-tenant
isolation and a recorded cross-party consent contract; that pool kind is off by default, and
a build against it without a recorded contract raises
`DisclosureCrossPartyPoolUnconsented`. The engine verifies the contract's presence and its
binding to the pool, not its legal validity.

## Options considered

| Option | What it buys | What it costs |
| --- | --- | --- |
| **Posture keyed on the candidate pool, cross-party off by default** *(chosen)* | The control matches the variable that actually decides who is disclosed to whom, so each of the three cases carries exactly the obligations its boundary crossing creates. | A deployment wanting cross-tenant lookalikes carries per-tenant isolation plus a consent artifact the engine cannot evaluate, and a tenant reading only the manifest cannot tell a legitimate contract from a placeholder one. |
| Keying posture on the scoring method | One knob, visible in the code that computes the segment; easy to reason about from the algorithm. | Lost on boundary crossing: identical mathematics over a different pool changes the disclosing party and the disclosed-to party, so the knob is measuring something that does not vary with the risk. |
| A uniform maximum posture for every pool | Nothing to classify, nothing to get wrong; the strictest case sets the rule. | Lost on proportionality: it imposes escrow-grade isolation and a consent artifact on a tenant querying its own base, where no boundary is crossed at all, which trains operators to route around the control. |
| Cross-party pools on with an opt-out | Removes friction for the deployment shape most likely to be sold; consent is still expressible. | Lost on whose consent it is: the default is asserted by the operator, and the consent it stands in for belongs to people who are not party to the deployment's configuration. |
| Refusing cross-party pools outright | No unverifiable consent artifact enters the system, and the hardest case cannot be misconfigured. | Lost on necessity rather than principle: a deployment where the relationship genuinely exists and is documented has no path, and the refusal would be routed around outside the engine where nothing observes it. |

## Criteria

1. **Where a disclosure crosses a party boundary** — for each pool kind, which party learns
   something about a person held by another party. **This criterion decided.** It is the only
   variable among the candidates under consideration that changes across the three cases;
   the scoring method is constant, so keying on it would be keying on noise.
2. **Whose consent the default asserts** — whether a default setting stands in for a
   permission held by someone absent from the configuration.
3. **Proportionality** — whether a control's cost tracks the risk it answers, so the strict
   case stays strict without making the benign case expensive.
4. **Verifiability by the engine** — what the engine can actually check about a consent
   artifact, as opposed to what an operator asserts about it.
5. **Reachability of the legitimate case** — whether a deployment with a real relationship
   has a supported path rather than an unobserved workaround.

## Consequences

Classification becomes a declaration about a pool rather than a review of an algorithm, so
adding a scoring method touches no posture and changing a pool's provenance changes the
obligations automatically. The benign case — a tenant over its own base — stays cheap, which
keeps the expensive controls credible where they apply.

The cost accepted: the engine verifies the presence and binding of a consent contract and
not its validity. A recorded artifact that is legally worthless passes the check, so the
refusal narrows the failure to "nobody wrote anything down" rather than to "nobody had the
right". That is a real gap and it is not closable inside the engine; the strength of the
control is exactly the strength of the deploying organization's contracting process.
Per-tenant isolation on the cross-party path is also the most expensive posture the system
carries, and a deployment that turns it on pays for it continuously.

Reversing the default is a one-line change. Reversing the keying — moving posture back onto
the method — would rewrite every pool declaration in every deployment.

## Revisit triggers

- A pool kind appears whose boundary crossing is not one of the three, for example a pool
  contributed jointly by several tenants under one agreement.
- A scoring method is proposed whose output discloses more than the pool membership does, at
  which point the method stops being a constant.
- A recorded consent contract acquires a machine-checkable form, making validity rather than
  presence something the engine can assert.
