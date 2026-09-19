# D30 — Credentials are sender-constrained, with a nonce replay window

**Status:** accepted

## Context

A bearer credential is copied into logs, run records and forwarded headers, and every copy is the original for its whole lifetime. Transport authentication terminates at the gateway, while the engine repeats authorization over the same bytes.

## Decision

Holding a credential admits nothing without proof of the key it names, and a captured request is not re-sendable.

- `authority.verify` requires every credential to carry a confirmation thumbprint of a client public key. Each request carries a signature over method, target, body digest and nonce from the matching private key; a signature that does not check against the thumbprint refuses.
- A nonce repeating inside the checkpoint's replay window refuses. The window is held locally per checkpoint; a nonce outside it is not remembered.
- The proof establishes key possession and makes no claim about what the client is.

## Options

| Option | Lost on | Cost |
| --- | --- | --- |
| Confirmation thumbprint, per-request signature, local nonce window *(chosen)* | — | Every client holds a key pair and signs each request; a capture replayed at a different checkpoint is caught only by the credential's other bounds. |
| Bearer bytes defended by short lifetimes | Theft yield | A copied credential would act as the original until expiry. |
| Mutual TLS in place of a credential-carried proof | Reach across hops | The engine behind the gateway would receive an unbound credential. |
| A monotonic counter per client | Concurrency | Parallel honest requests would arrive out of order and refuse. |
| A replay store shared across checkpoints | Locality | Admission would call a shared service on every request. |

## Consequences

- A credential is minted against one client's key rather than handed to whoever needs it.
- Each checkpoint holds nonce state sized by its window and request rate.

## Revisit

- Cross-checkpoint replay is observed within a credential's lifetime.
- A client class cannot hold a private key.
