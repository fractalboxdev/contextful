# 0340 — A signature that does not check under a pinned key admits nothing

**Status:** accepted 2026-09-18
**Decides:** `authority.verify.refusal.bad-signature`

## Context

The wire shape is a version-tagged, dot-delimited envelope: a version segment, base64url
claims, the issuer's signature, and — once a holder has derived a child — an attenuation
segment and the holder's own signature. Each signature covers the segments as transmitted: the
issuer signs the first pair, a holder's derivation signs the leading four. A verifier checks
over bytes it can see rather than over a re-serialization of a parsed structure.

The keys a signature is checked against are the deployment's own: static pins, or a key set
published by the project and refreshed on a lifetime. Nothing in the credential selects them. A
signature that does not check under that material means one of several things the checkpoint
cannot tell apart — bytes altered in transit, a credential minted by an issuer this deployment
does not trust, a key rotated out, a fabricated segment appended by a holder.

The interesting question is not whether to refuse. It is what a partial failure yields. A chain
is a parent plus appended blocks, and each appended block narrows: actions, table patterns,
tenant scope, aggregate constraints and the template allowlist are each no broader than the
parent's. So the segment most attractive to corrupt is the last one. If a verifier that cannot
check the final holder signature falls back to the longest verifying prefix, it admits the
parent — which is the broader credential, and which the presenter demonstrably does not hold
the proof for. The failure mode of a tolerant verifier is not a denial; it is an escalation
performed by appending garbage.

## Decision

A segment whose signature does not check against a pinned key raises `SignatureInvalid`, and
the credential admits nothing. The refusal covers the whole envelope rather than the failing
segment: no prefix is admitted, no claim from a verified segment survives, and the admitted
authority value is never constructed.

## Options considered

| Option | What it buys | What it costs |
| --- | --- | --- |
| **Refuse the whole credential** *(chosen)* | What admits is exactly what the deployment's key material vouches for, end to end, and appending an unverifiable block reaches nothing. | A rotation that retires a key turns back every credential minted under it at once, so grace has to cover the longest lifetime in circulation rather than being managed per credential. |
| Admit the longest verifying prefix | A truncated or corrupted tail degrades gracefully, and the parent's grants still serve. | Lost on the direction of the error: the prefix is the broader credential, so a holder widens itself by appending a block no verifier can check. |
| Admit with authority reduced to nothing | Fails closed without an error arm, and nothing is ever wrongly widened. | Loses on legibility: a credential that admits and reads nothing is indistinguishable from an unprovisioned link or a withdrawn grant, so the defect surfaces far from its cause. |
| Resolve the verifying key from the credential — an issuer identifier or an embedded key | A rotation needs no deployment-side change, and a new issuer is reachable without provisioning. | Loses on the trust anchor: the credential would name the material that vouches for it, so anyone able to mint a key pair can mint an admissible credential. |
| Defer the check to the first effect rather than at admission | One pass over the bytes, at the point authority is actually exercised. | Loses on mediation: unverified bytes travel past the checkpoint as an admitted value, and every surface that reads it before an effect runs is trusting a signature nobody checked. |

## Criteria

1. **Whether an admitted credential is vouched for end to end by material the deployment
   pinned.** *This criterion decides.*
2. **Direction of the error under a partial failure** — whether a checkpoint that cannot verify
   part of a chain withholds authority or hands back more of it.
3. **Legibility** — whether the outcome tells the presenter what went wrong.
4. **Rotation cost** — what a key change does to credentials in circulation.

Criterion 1 decides, and criterion 2 is what eliminates the otherwise-reasonable prefix
fallback. Narrowing is the only thing an appended block does, so every failure to verify one
sits on the side of more authority rather than less, and a verifier that resolves such a
failure by verifying less is one that resolves it by granting more. Criterion 3 rules out the
silent variant, which fails closed and reports nothing. Criterion 4 is the cost, and it is
carried by the rotation grace rather than by relaxing the check.

## Consequences

Admission has one answer for every way an envelope can fail to check, and it produces no
value: a refused credential yields no subject, no grants and no session, so nothing downstream
has a partially-trusted shape to handle. The chain-final proof composes with this — a holder of
a child lacks the proof the parent prefix verifies under, and truncating back to that prefix
yields no verifiable credential either.

The cost accepted is rotation rigidity. Retiring a key refuses, at once, every credential
minted under it, including ones mid-flight in a durable run. That is why the verifying side
accepts a set rather than a single pin, why a published set is refreshed on a lifetime with one
refresh-and-retry on a signature failure, and why the grace covers the longest lifetime any
mint path produces.

A second cost is diagnostic: the presenter learns that the credential did not check and not
which segment failed against which key, so a rotation mid-flight and a corrupted byte look the
same from outside. The checkpoint's own telemetry is where the two are separated.

## Revisit triggers

- Chains grow deep enough that refusing the whole envelope over one block becomes a real
  availability problem for holders who did not author that block.
- A block class appears that is provably non-narrowing by construction, which would make a
  prefix admission safe against criterion 2 for that class.
- Issuer discovery becomes a solved problem in the deployment — a key distribution path with
  its own trust anchor — which would reopen where verifying material comes from.
