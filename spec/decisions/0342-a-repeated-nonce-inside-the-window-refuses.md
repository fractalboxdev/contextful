# 0342 — A binding nonce repeating inside the window refuses the request

**Status:** accepted 2026-09-18
**Decides:** `authority.verify.refusal.replayed-nonce`

## Context

The binding signature a client makes covers the method, the target, a digest of the body and a
nonce. Those four values are what a checkpoint compares, and three of them are properties of
the request rather than of the moment it was made. A request captured on the wire, out of a
proxy log, or from a client's own retry buffer therefore checks as well the tenth time as the
first: the signature is valid, the thumbprint matches, the credential has not expired.

That leaves the possession proof establishing who composed the request and not that this
sending of it is new. The exposures are concrete rather than theoretical. A write lands rows,
so a re-sent write lands them again under a principal who authored one. An execute fires a run,
so a re-sent execute fires a second. Idempotency at the application level covers some of that
and is a property of each surface rather than of admission.

Admission's own constraints shape the answer. A verifying checkpoint holds public material and
no secret, and runs with no call to an issuer or a policy service — so anything that makes a
request unique has to be decidable from the request plus state the checkpoint already holds.
Checkpoints are also plural: a gateway and the engine both admit, and a deployment runs several
of each.

Memory is the other constraint. Remembering every nonce a checkpoint has ever seen is not
available, so the check is bounded by a window, and the window is simultaneously what makes it
affordable and what bounds the guarantee.

## Decision

A request whose binding nonce repeats inside the checkpoint's replay window raises
`PossessionProofReplayed`, so a captured request is not re-sendable while its credential is
still live. The window is the checkpoint's own, held locally, and a nonce outside it is not
remembered.

## Options considered

| Option | What it buys | What it costs |
| --- | --- | --- |
| **A per-checkpoint replay window over seen nonces** *(chosen)* | A captured request is single-use at the checkpoint that saw it, decided locally, with bounded memory. | The window is per checkpoint, so one capture replayed against a different checkpoint of the same deployment is caught by the credential's other bounds rather than by this one. |
| No nonce; rely on the credential's lifetime and audience | Nothing to remember, and admission is a pure function of the request. | Loses on replay: a captured request is re-sendable for the whole life of the credential, which is the exposure the binding signature otherwise closes for theft of the bytes alone. |
| A monotonic counter per client | Exact single-use semantics with one number per client instead of a set. | Loses on concurrency: a client issuing parallel requests delivers them out of order, so honest traffic is refused, and every checkpoint owes durable per-client state. |
| A freshness check on a signed timestamp alone | Bounded memory without tracking anything, and clock skew is the only tuning. | Loses on replay inside the tolerance: duplicates within the accepted skew check fine, and the skew has to be generous enough to cover real clients. |
| A replay store shared across every checkpoint | One capture is single-use deployment-wide, not per checkpoint. | Loses on locality: admission would call a shared service on every request, which puts a network hop and a dependency into the one path built to run on public material alone. |

## Criteria

1. **Whether a captured request is re-sendable while its credential is live.** *This criterion
   decides.*
2. **Whether admission stays local** — decidable from the request and state the checkpoint
   already holds.
3. **State a checkpoint keeps** — memory and durability.
4. **Concurrency** — whether honest parallel traffic from one client is admitted.

Criterion 1 decides, and criterion 2 bounds the mechanism that serves it. A shared store is the
only option that closes replay across a fleet, and it does so by making every admission depend
on a service the checkpoint otherwise never calls — which trades a bounded exposure for an
availability coupling on the whole read path. Criterion 4 eliminates the counter, which is
otherwise the cheapest exact answer, since parallel requests from one client are ordinary.
Criterion 3 is what makes the window finite.

## Consequences

Each request is single-use at the checkpoint that admitted it, for the length of the window, so
capture-and-resend stops being a way to duplicate a write or re-fire a run. The check needs no
coordination: a checkpoint starting cold has an empty window and refuses nothing it has not
itself seen.

The cost accepted is the window's edges. A capture replayed against a different checkpoint of
the same deployment is not caught here — what bounds it there is the credential's expiry, its
audience and the revocation epoch — and a capture held past the window and replayed at the
original checkpoint is likewise outside the check. The window is therefore sized against the
lifetimes in circulation rather than against convenience.

A client that reuses a nonce across two requests is refused on the second, so nonce generation
is the client's obligation and a client library that draws them from a weak source produces
intermittent refusals that read as flakiness.

## Revisit triggers

- Replay across checkpoints is observed to matter — a capture re-sent to a sibling checkpoint
  reaching an effect — which would reopen the shared-store cost.
- Credential lifetimes are shortened enough that they bound replay as tightly as the window
  does, which would make the window redundant at its current size.
- A deployment shape appears where checkpoints already share a low-latency store on the request
  path, removing the coupling that eliminated the shared option.
