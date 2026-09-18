# 0261 — A cross-owner hashed join keys on a per-pair pepper, re-randomized for the pairing and rotated per join

**Status:** accepted 2026-09-18
**Decides:** `disclosure.set-mode.refusal.pepper`

## Context

Two owners holding disjoint datasets match on a key without either seeing the other's rows:
each writer hashes the key at write time, and the join keeps the rows present on both sides.
The security of that arrangement rests entirely on what goes into the hash alongside the key.

The keys that matter in practice are exactly the ones whose value space an adversary
generates offline — email addresses, telephone numbers, national identifiers. The same
premise already governs a digest inside one deployment: a hash over an exhaustible class is
invertible by enumeration, so the digest alone protects nothing. Crossing a trust boundary
does not weaken that premise; it adds a second party who holds the secret.

A static pepper fails on exactly that. It is one value, shared with every counterparty and
reused across every join, so recovering it once reverses every join ever run under it, in
both directions, retroactively. The attacker is not hypothetical — the counterparty holds
the pepper by construction.

Stronger constructions exist and are known. A private set intersection over a
Diffie-Hellman key exchange removes the shared secret entirely and gives the property by
mathematics rather than by custody. A trusted-execution join moves the match inside an
attested enclave. Both are real, and both are substantially more machinery than a keying
rule.

## Decision

A cross-owner hashed join keys on a per-pair pepper, re-randomized for the pairing,
exchanged through escrow and rotated per join. A join keyed on a static pepper raises
`DisclosureStaticPepper`. A key space an adversary enumerates end to end is carried into the
join alongside a group-size floor, and the matched rows reach a consumer as an aggregate
rather than as matches.

## Options considered

| Option | What it buys | What it costs |
| --- | --- | --- |
| **Per-pair pepper, re-randomized for the pairing, rotated per join** *(chosen)* | A recovered pepper compromises one pairing and one join rather than every join ever run, and digests do not correlate across counterparties. | Pepper exchange and per-join rotation are operational work the escrow arrangement carries, and the strength claim degrades under repeated joins against the same counterparty. |
| A static pepper shared across pairings and joins | No exchange protocol, no rotation, no escrow role; digests are stable and reusable. | Loses outright on whether the digest survives an attacker holding the pepper: one recovery reverses every join ever run under it, in both directions. |
| Private set intersection over a Diffie-Hellman key exchange | The property holds by construction rather than by custody, with no shared secret to recover. | Loses on implementation cost for a property the per-pair pepper already floors. Named as the upgrade path rather than rejected on merit. |
| A trusted-execution join inside an attested enclave | Strongest of the three, and the match itself never leaves the enclave. | Loses on cost and on dependency: it binds the setting to specific hardware and an attestation chain. Held as the fork for very-high-risk pairings rather than the floor. |
| Hash with no pepper, relying on the group-size floor alone | One less secret to manage; the floor still bounds what a consumer reads. | Loses on the enumeration criterion: the digest is invertible offline, so the counterparty recovers the raw key regardless of what the aggregate returns. |

## Criteria

1. **Whether the digest survives an attacker holding the pepper who can enumerate the key
   domain.** *This criterion decides.* The counterparty holds the pepper by construction, so
   any scheme whose protection rests on the pepper staying secret from them is already
   defeated — what remains is how far a single recovery reaches, and per-pair rotation is
   what bounds it.
2. **Blast radius of one recovered secret.** One pairing and one join, rather than the
   history.
3. **Implementation and operational cost.** Decides between the floor and the upgrades.
4. **Hardware and attestation dependencies.** Rules out an enclave as the floor.

## Consequences

The escrow gains a defined role and a narrow one: it carries per-pair peppers between two
owners, holds no signing material, and sees ciphertext where a tenant's columnar files pass
through it. Rotation per join means digests are not stable across runs, so a hashed key is
useful for the join that minted it and nothing else — which also means no durable identifier
accumulates on either side.

The accepted cost is operational. Every join begins with an exchange, and a pairing that
runs frequently rotates frequently; a deployment that finds this burdensome will be tempted
to reuse a pepper across runs, which is the refused shape wearing a different name.

The strength claim degrades under repetition. Repeated joins against the same counterparty
over overlapping key sets leak intersection structure regardless of rotation, and the
per-pair pepper says nothing about that. That limit is stated rather than papered over, and
it is the reason the upgrade path is named.

Reversing toward a static pepper is retroactively catastrophic in a way the others are not:
it makes every past join reversible by whoever holds the one value.

## Revisit triggers

- A private set intersection implementation lands whose cost is comparable to the hashed
  join, which would move the floor rather than extend it.
- A pairing appears whose risk profile justifies attested-enclave machinery, taking the
  named fork.
- Repeated joins against one counterparty become the normal pattern rather than the
  exception, which is the case the per-pair pepper is weakest on.
