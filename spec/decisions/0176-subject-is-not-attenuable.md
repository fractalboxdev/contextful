# 0176 — The verified principal is fixed at the mint and is not attenuable onto another person

**Status:** accepted 2026-09-18
**Decides:** `authority.identify.refusal.subject-rebinding`

## Context

Derivation is offline and unmediated. A holder narrows what it holds by appending a block
and signing over the parent's transmitted bytes, with no round trip to the issuer and no
party present to approve the result. That is what makes delegation cheap: an agent fanning
work out mints one child per sub-agent in-process, and a checkpoint verifies the chain
without contacting anyone.

Every other member of the subject tuple is self-asserted. The agent, the host, the task
scope and the zone travel labeled as such, and a checkpoint treats them as claims rather
than as identity. `on_behalf_of` is the exception: it is checked at the mint against the
issuing identity provider, and it is the key an admission carries forward as the reader's
identity. Identity links join on it, table policy reads it, and the audit record names it.

Those two properties are in tension. Narrowing is safe to perform locally precisely
because narrowing cannot produce authority the parent did not have — a child's actions,
tables, tenant scope and validity window are each checked to lie inside the parent's, at
derivation and again at admission. A different `on_behalf_of` is not a narrowing. There
is no order on principals under which acting for one person is a subset of acting for
another; it is a sideways move into a set of rows the parent never held.

If derivation could set it, the verified member would become the one self-asserted member
that everything treats as verified — a holder would mint, offline and unobserved,
credentials reading as any person it cared to name, and the provider check at the mint
would bound nothing beyond the first hop.

## Decision

A derivation naming an `on_behalf_of` different from its parent's raises
`AuthoritySubjectRebound`. The verified member is fixed by the mint that checked it and
travels unchanged down every chain derived from it. A child repeating its parent's value,
or naming none, derives normally. A hop that acts for a different person runs through
issuer-mediated exchange, where the provider verifies the new principal.

## Options considered

| Option | What it buys | What it costs |
| --- | --- | --- |
| **Fixed at the mint; rebinding refuses** *(chosen)* | The one member everything reads as identity is the one member no holder can choose, and the provider check bounds the whole chain rather than its first hop | A tool legitimately acting for a series of people takes a round trip per person instead of deriving locally |
| Allow rebinding to any subject | Delegation expresses every impersonation pattern locally, at no cost | Lost outright on the threat: derivation is offline, so any holder mints credentials naming any person, and the verified member becomes self-asserted |
| Allow rebinding within a set the parent carries | Covers the support-tool pattern without a directory read | Lost on the same threat, narrowed: the set is chosen by whoever minted the parent, and a broad set is one mint away from the unrestricted case, with nothing at admission able to tell a legitimate set from a generous one |
| Allow rebinding, verified at admission against the directory | The rebinding is checked by the party that verifies identity | Lost on read-path independence: admission would call the identity provider per request, putting provider downtime back on the read path the exchange exists to keep clear |
| Treat `on_behalf_of` as self-asserted like the other members | One uniform rule for the whole tuple; no exception to hold in mind | Lost on what the member is for: it is the key links join on and audit names, so labeling it self-asserted removes the engine's only verified identity rather than protecting it |

## Criteria

1. **Unforgeability of the verified member** — whether a holder, acting alone, can produce
   a credential naming a person it was not minted for.
2. **Read-path independence** — whether admission requires contacting the identity
   provider.
3. **Expressiveness of legitimate delegation** — which real patterns lose their local
   derivation.
4. **Uniformity of the tuple** — how many rules the subject carries.

Criterion 1 decided it. Expressiveness is the live cost and criterion 2 rules out the one
option that would have recovered it. What settles criterion 1 above both is that
derivation happens with nobody watching and no record until the child is presented: a
rebinding that is wrong produces a correctly signed credential that verifies, reads as
another person, and lands their identity in the audit record. There is no later point at
which the forgery is detectable, because everything downstream is doing exactly what it
is supposed to do with a value it has no reason to doubt.

## Consequences

A chain's verified identity is decided once, by the party that verifies identity, and
every hop after that can only narrow. A checkpoint re-checks the whole chain and needs no
directory to do it. Sub-agent fan-out keeps its offline derivation, since a sub-agent acts
for the same person its parent does.

The cost accepted is that a legitimate impersonation pattern — a support tool or a batch
job acting for a series of users — cannot derive locally. It runs issuer-mediated
exchange once per user, which is a round trip and a mint per user rather than a local
signature, and it needs the issuing path reachable while it works. For a tool acting for
thousands of users in one pass that is the difference between an in-process loop and a
provisioning exercise.

Reversing this is not a configuration change. Any credential minted under a rebinding is
a credential whose named identity nobody verified, and its audit records are wrong in a
way that cannot be detected after the fact.

## Revisit triggers

- Issuer-mediated exchange becomes a bottleneck for a real per-user delegation workload,
  with a measured cost per user rather than an anticipated one.
- A derivation-time proof appears that binds a new principal without a directory read at
  admission, which would satisfy criteria 1 and 2 together.
- `on_behalf_of` stops being the key identity links join on, which would remove the reason
  it is treated differently from the self-asserted members.
