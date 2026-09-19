# D20 — Credentials are references, workload identity is the default, and leases lead

**Status:** accepted

## Context

A declaration is committed and widely read; a credential store is swapped across deployments; vendors change token lifetimes. Wherever a long-lived credential sits readable by the engine, one read of the host yields all of it.

## Decision

Declarations carry names, stores carry opaque bytes, connectors carry provider know-how, and the engine holds the shortest-lived material the deployment can mint.

- `connector.reference` accepts `${secret://<name>}` as the one placeholder; environment placeholders, malformed templates, pure-literal attach values and credential-shaped literals refuse at parse or validation.
- Every encrypted entry carries a record of grants, scope, creation, expiry and rotation point; an undocumented entry refuses.
- `connector.resolve` defaults to an external manager read through workload identity; inline material refuses outside development, as does a sidecar forwarding a stored credential and an adapter exposing provider exchange or lifetime logic. A name two adapters answer raises `SecretNameShadowed` at first hydration.
- `connector.lease` leads the chain for declared names with no fall-through; one mint-scoped credential bootstraps it through the adapters behind it. The mint client retries nothing; the run's schedule is the one retry layer.
- `connector.rotate` fails the run closed when the manager refuses a write-back.

## Options

| Option | Lost on | Cost |
| --- | --- | --- |
| Reference-only declarations, workload identity default, lease at the head *(chosen)* | — | Material sits in engine memory for a request; adding a credential takes a four-field record; a read-only manager cannot run the OAuth refresh loop. |
| Inline material or environment templates | Plaintext at rest | The committed file or process environment would become the secret. |
| A broker sidecar as the default | Operability | Every deployment would run a second failure domain for a property most do not need. |
| Lease provider last, first hit wins silently | Posture | A leftover environment variable would win over the mint or the manager unnoticed. |
| Provider rotation engines inside the store | Swappability | Every backend would reimplement every vendor's exchange. |

## Consequences

- Local development loses the paste-a-token path unless an explicit opt-in flag re-opens it.
- A configured bootstrap backend is a second backend to operate.

## Revisit

- Workload identity becomes unattestable on a runtime a deployment needs.
- A broker becomes operable at near-zero marginal cost.
- A runtime identity authenticates to the mint endpoint directly, removing the bootstrap credential.
