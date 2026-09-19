# D28 — Verification trusts only pinned or injected keys

**Status:** accepted

## Context

A verifier that lets the presented credential choose its key, its algorithm or its key source trusts the party under test. Custody postures range from a seed file to a hardware module, rotation needs key overlap, and admission runs on public material alone with no network call on the read path.

## Decision

The key a signature is checked against is chosen by the deployment, never by the credential, and an absent answer about keys is a refusal.

- `authority.issue` signs through one provider-agnostic signing port; seed file, secret reference, cloud KMS, HSM and remote oracle are adapters behind it.
- The pinned key's own scheme decides verification; a credential's algorithm claim naming another scheme refuses.
- `authority.verify` accepts a key set, from static pins or an opted-into published key route. A checkpoint that opted in and cannot obtain the set refuses every credential: the engine declines to start and a gateway answers 503; static pins rescue neither.
- `authority.exchange` verifies an external assertion against operator-injected material — a shared secret, a PEM public key, or a key-set document selected by `kid` — and makes no network call.

## Options

| Option | Lost on | Cost |
| --- | --- | --- |
| Deployment-chosen keys: signing port, pinned scheme, key set, injected material *(chosen)* | — | Port operations are the intersection of adapters; key-route availability becomes admission availability; operators rotate provider keys out of band. |
| Dispatch on the credential's algorithm claim | Downgrade | Every weaker routine the verifier supports would be reachable by asking for it. |
| A single pinned key | Rotation | No overlap would be expressible, making every rotation a flag day. |
| Fall back to static pins or a last-known-good set | Frozen key material | A retired key would keep verifying for as long as the route is down. |
| Provider discovery or introspection inside the exchange | Reproducibility | Verification would depend on what a remote document returned at that instant. |

## Consequences

- Two signature schemes each carry their own routine and tests.
- A provider rotation the operator misses fails the mint closed.
- Retiring a key from a set is an action, not an omission.

## Revisit

- An adapter capability, such as batch signing, becomes necessary and the port cannot express it.
- Key-route outages are observed as the leading cause of admission refusals.
