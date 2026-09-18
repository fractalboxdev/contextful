# 0341 — A request proves possession of the key the credential names

**Status:** accepted 2026-09-18
**Decides:** `authority.verify.invariant.possession-proof`, `authority.verify.refusal.possession-proof`

## Context

Authority travels as a value. A credential is bytes: they cross process boundaries, ride a
gateway that forwards them to the engine, sit on a durable run's own record while a step
sleeps, land in a proxy's access log, and get pasted into a terminal by a person debugging.
Every one of those is a legitimate part of how the system works, and every one is a place the
bytes can be taken from.

Bearer semantics make possession of the bytes the whole of the authority. The defenses already
in place narrow the window without closing it: a persisted issuance ceiling caps a lifetime,
an audience binds a credential to one deployment, a revocation epoch ends one early. Inside
those bounds a copied credential is the original, and nothing at admission can tell the two
apart, because there is nothing about the presenter in the bytes.

Binding to the connection does not survive the topology. A gateway forwards the credential it
received and the engine owns the final data authorization, repeating the decision over the same
bytes — so a proof carried by the transport is terminated at the first hop and the second
checkpoint sees an unaccompanied credential.

What a proof can honestly establish is also narrower than it is tempting to claim. A signature
made with a private key establishes that the party making the request holds that key. It says
nothing about what the party is — an agent, a service, a person at a terminal — and a check
sold as answering that question invites policy to be written against an answer it does not
give.

## Decision

Every credential carries a confirmation claim holding the thumbprint of a public key its client
possesses. Each request carries a signature over the method, the target, a digest of the body
and a nonce, produced with the matching private key. A request whose binding signature does not
check against the confirmation thumbprint raises `PossessionProofInvalid`. Holding the
credential without its private key admits nothing, and the check makes no claim about what the
agent is.

## Options considered

| Option | What it buys | What it costs |
| --- | --- | --- |
| **A confirmation thumbprint in the credential, a per-request signature from the matching key** *(chosen)* | A stolen credential is inert: it is exercisable only by the party holding a key that never travels with it, and the proof survives every forwarding hop because it rides the request. | Every client holds a key pair and signs each request, and a credential is minted against one client's key rather than being handed to whoever needs it. |
| Bearer bytes alone, defended by short lifetimes | Nothing to hold, nothing to sign; any client with an HTTP call can present one. | Loses on what theft yields: a copied credential is the original for its whole lifetime, and every log, run record and forwarded header is a place it can be copied from. |
| Mutual transport authentication in place of a credential-carried proof | The proof is handled by the transport, with no application-level signing. | Loses on reach across hops: the transport terminates at the gateway, so the engine — which repeats the authorization over the same bytes — receives a credential with nothing attached to it. |
| Bind the credential to a network origin or a client identifier | No key management, and a stolen credential is useless from another address. | Loses on what is provable: the binding is to a value the presenter also controls or that a legitimate client changes routinely, so it denies honest callers and stops nobody who can reach the same path. |
| Bind to an attestation of what the client is | Policy could distinguish an agent from a person and act on it. | Loses on what is provable, more sharply: no attestation available at a checkpoint establishes the nature of the caller, so the claim would be stated and not held. |
| Sign the request without a nonce | Simpler state at the checkpoint, and theft of the credential alone is still useless. | Loses on replay: a captured request checks forever while the credential is live, which is the case a separate window closes. |

## Criteria

1. **What possession of the credential alone yields.** *This criterion decides.*
2. **Whether the proof survives a forwarding hop** — whether both checkpoints on the served
   read path can check it.
3. **What the check can honestly claim** — whether the property asserted is the property
   established.
4. **Client cost** — key management and per-request signing.

Criterion 1 decides. Every other defense bounds how long or where a copied credential works;
this is the one that makes the copy insufficient. Criterion 2 eliminates the transport-level
answer, which is otherwise the cheapest, because the architecture forwards bytes rather than
connections. Criterion 3 is what keeps the claim narrow: the refusal is about a key, and the
spec says so rather than letting the check be read as an identity or an agent-detection
verdict. Criterion 4 is the cost.

## Consequences

A credential in a log, on a run record or in a chat message is not an authority anyone can
exercise, which changes what a leak is: recovery is revocation as hygiene rather than as an
incident. The proof composes with derivation — a child is bound to its own sub-agent's key
pair, so fanning work out hands each worker a credential nobody else can use.

The cost accepted is that every client holds a key pair, generates it before the mint and
presents its thumbprint to be stamped into the credential. That rules out handing a credential
to a plain browser fetch or a copy-pasted request, and it makes a lost key equivalent to a lost
credential: the remedy is a new mint. Embedding surfaces therefore mint per reader at their own
edge rather than distributing one credential to many clients.

Signing is per request, over the method, the target, a body digest and a nonce, so a client
library rather than a raw call is the practical way to reach the API.

## Revisit triggers

- A client class appears that cannot hold a key at all and matters enough to serve, which would
  put a narrower bearer path — scoped, short and single-purpose — back on the table.
- Key material becomes available to a checkpoint through a platform primitive that also
  attests the caller, which would widen what the check can honestly claim.
- Per-request signing shows up as a latency cost at a volume that matters, which would reopen
  the granularity of the proof rather than its existence.
