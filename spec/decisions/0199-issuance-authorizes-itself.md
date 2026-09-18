# 0199 — A mint endpoint requires a grant carrying the admin action

**Status:** accepted 2026-09-18
**Decides:** `authority.issue.refusal.unauthorized-mint`

## Context

Minting is the most consequential action a served face can offer. A mint produces a
credential carrying a subject the issuer stamps, including the verified principal rows will
be authored by, and grants up to whatever the persisted issuance policy permits. Whoever can
mint can reach everything anyone can reach, under any principal the mint will stamp.

So the mint endpoint needs authorization, and there is a strong pull toward giving it a
mechanism of its own. The reasoning is that the mint is special, that the credential system
is what it produces, and that authorizing the producer with its own product sounds circular.

It is not circular. An `admin` grant is a grant like `read` and `write`: it names an action,
it sits inside a subject tuple, it narrows under derivation, it expires, it is revocable by
epoch, and its use lands an audit record naming the principal who made it. A mint secret has
none of those properties, and the ways it lacks them are the ways a mint endpoint is
dangerous: it never lapses, it names nobody, it cannot be narrowed to one project or one
lifetime, and its use is indistinguishable in the record from any other use of it.

Mutual transport authentication is available and useful at this endpoint, and it answers a
different question. A client certificate names a machine and carries no grant, so it can
narrow who reaches the endpoint without deciding who is authorized at it.

## Decision

A mint endpoint requires a grant carrying the `admin` action, optionally behind mutual
transport authentication, and raises `IssuanceUnauthorized` otherwise. Issuance authorizes
itself with the same primitive as every other action.

## Options considered

| Option | What it buys | What it costs |
| --- | --- | --- |
| **An `admin` grant, optionally behind mutual transport authentication** *(chosen)* | Mint authority expires, narrows, revokes and audits like every other authority, and the endpoint adds no primitive | The first admin credential cannot come from the endpoint, so a mint path that runs from inside the project tree with the seed in hand has to exist alongside it |
| A dedicated mint secret or bearer | Trivially separable from the capability system; no bootstrap circularity | Lost on revocability and attribution: the secret never lapses, narrows to nothing, names nobody in the audit record, and its holder is owner-equivalent for as long as it is live |
| Mutual transport authentication alone | Strong client authentication with no credential distribution | Lost on subject: a client certificate names a machine, yields no grant to narrow and no principal to attribute, so the audit record for a mint says which host called and not who decided |
| Expose the mint on a local socket with no authorization | Nothing to configure; the operator on the box can always mint | Lost on what reaches a local socket: every agent process this engine serves runs there, and an unauthorized mint path admits the authority that stamps any principal |
| A separate mint role checked against an operator directory | Familiar administrative model; no capability needed to mint | Lost on read-path independence and primitive count: it introduces a directory this engine deliberately does not hold, and puts a second authorization system beside the one every other action uses |

## Criteria

1. **Whether mint authority carries the properties every other authority carries** —
   expiry, narrowing, revocation, an attributable subject. *This criterion decides.*
2. **Number of authorization primitives in the system.**
3. **Attribution of a mint** — whether the audit record names a principal.
4. **Bootstrap simplicity** — whether the first credential is reachable without a special
   path.

Criterion 1 outranks bootstrap simplicity, which is the one thing the rejected options do
better. The reason is what a mint produces: a credential that admits somewhere. An
unrevocable, unexpiring, unattributed mint authority is not one over-privileged endpoint, it
is a standing ability to manufacture privilege that no revocation epoch reaches and no audit
record explains. Criterion 4's cost is paid once per deployment; criterion 1's cost would be
paid for the deployment's lifetime.

## Consequences

An operator's mint authority lapses on the same schedule as everything else they hold,
narrows to a project when it should, and appears in the audit record under their name.
Revoking a compromised administrator is the revocation that already exists rather than a
secret rotation across every face. Mutual transport authentication stays available as an
additional narrowing on who may reach the endpoint at all.

The cost accepted is bootstrap. The endpoint cannot be the only mint path, because the first
admin credential has no admin credential to authorize it. That path is a mint run from inside
the project tree with the signing material in hand — where persisted-ceiling enforcement
binds it — and it is a second way to mint that exists precisely because this decision closed
the easy one. An operator who loses every admin credential recovers through custody of the
seed rather than through the endpoint.

Reversing toward a mint secret is straightforward and gives up revocation, expiry and
attribution on the system's most consequential action simultaneously.

## Revisit triggers

- The in-tree bootstrap path is found to be reachable in a deployment where it should not
  be, which would make the second mint path the weaker of the two.
- An automated issuance workload needs a mint authority that outlives the issuance ceiling,
  which the `admin` grant's own expiry cannot express.
- Revocation stops reaching admin grants promptly enough to matter at the mint, which would
  undercut the property this decision was chosen for.
