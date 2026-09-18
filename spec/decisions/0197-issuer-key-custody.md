# 0197 — Minting runs through one provider-agnostic signing port, and custody is an adapter behind it

**Status:** accepted 2026-09-18
**Decides:** `authority.issue.interface.signing-port`

## Context

Minting is the one moment signing material is reached. Everything else in the authority path
holds public key material alone: a checkpoint verifies, a replica verifies, a holder derives
by signing with its own key over a parent it cannot forge. The signing seed exists at exactly
one point in the system.

Deployments want very different things at that point. A developer working locally wants a
seed file and no ceremony. A modest production deployment wants a reference the process
resolves at mint time, so nothing durable sits on disk beside the binary. A regulated
deployment wants the seed inside a key-management service or a hardware module. The
strictest wants a signing oracle: the host sends bytes, the custodian returns a signature,
and the seed never crosses the custodian's boundary at all, with each mint recorded there as
a signing call.

Those four are not four features. They are four answers to one question — who holds the
seed — and they differ in the strength they buy, not in what minting does. The credential
that comes out is byte-identical whichever one answered.

The risk in letting them differ structurally is that each additional path that touches
signing material is a path that has to be right. A branch per custody posture means the
lowest-assurance branch is present in the binary that a regulated deployment runs, and the
audit question stops being "what does the mint do" and becomes "which branch ran".

## Decision

Minting runs through a provider-agnostic signing port. A seed file, a resolved secret
reference, a cloud key-management service, a hardware security module and a remote signing
oracle are adapters behind that one interface, and the minting path is unchanged by which
one is bound. Custody posture is a binding an operator chooses, not a variant of the mint.

## Options considered

| Option | What it buys | What it costs |
| --- | --- | --- |
| **One signing port, custody as a bound adapter** *(chosen)* | Exactly one code path reaches signing material, and raising custody is a deployment change rather than a code change | The port's operations are the intersection of what every adapter offers, so an adapter capability no interface member expresses — batch signing, a key handle held across mints — is unreachable |
| One concrete seed-file implementation, with stronger custody added later | Simplest thing that mints; nothing abstract to design up front | Lost on reversal cost: the stronger postures are the ones a regulated deployment requires to exist at all, and retrofitting them means rewriting the mint rather than binding an adapter |
| A branch per custody posture inside the mint | Each posture uses exactly the calls its custodian offers, with no intersection to respect | Lost on the count of paths reaching signing material: the weakest branch ships in every binary, and answering what a mint does requires knowing which branch ran |
| Require the oracle posture everywhere | The seed never crosses a boundary in any deployment | Lost on the local shape: a developer cannot mint offline, and a test suite needs a custodian standing up before it can produce a credential |
| Bind the adapter at compile time rather than at run time | No unbound adapter present in a deployment's binary | Lost on effort and distribution: one artifact per custody posture multiplies the build and release surface, for a property the deployment's own configuration already establishes |

## Criteria

1. **Number of code paths that reach signing material** — how many places have to be correct
   for the seed to stay where it belongs. *This criterion decides.*
2. **Cost of raising custody** — whether moving from a seed file to an oracle is a
   configuration change or a rewrite.
3. **Offline mintability** — whether a mint works with no external service.
4. **Expressiveness for a particular custodian's capabilities.**

Criterion 1 outranks the rest because the mint is the system's only contact with a secret
whose compromise mints arbitrary credentials — including ones naming any principal, since
the mint is where the principal is stamped. A single path is auditable by reading it once;
several paths make the audit conditional on configuration, and the weakest of them is the
one that decides the answer. Criterion 4 is the accepted cost and is discussed below.

## Consequences

Custody becomes an operational decision with no code consequence. A deployment starts on a
seed file, moves to a resolved reference, and moves to an oracle, each time by binding a
different adapter, and the credentials it mints do not change shape. Testing the mint needs
no custodian, because a test binds an adapter of its own behind the same port.

The cost accepted is the intersection. The port expresses what every adapter can do, so a
custodian offering something richer — signing a batch in one call, holding a key handle
across mints, attesting the signing environment — offers it to nobody through this
interface. Supporting a signature scheme is a separate question from supporting a remote
signer, and neither is widened by the other.

The unmeasured quantity is latency. A remote oracle puts a network round trip inside the
mint call, and the mint's cost under that posture is unknown rather than bounded; local
postures make it a local signature. That matters at the mint rate an exchange-heavy
deployment produces, which is per reader per request rather than per operator per day.

## Revisit triggers

- Mint latency under the oracle posture is measured and lands high enough to show at the
  exchange rate a per-reader deployment produces.
- A custodian's capability that the port cannot express becomes a requirement rather than a
  convenience — an attestation over the signing environment would be the shape.
- A second point in the system needs signing material, which would end the property that one
  path reaches it and change what the port is protecting.
