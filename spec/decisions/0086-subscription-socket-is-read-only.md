# 0086 — A run-stream connection authenticates before the upgrade and mutates nothing

**Status:** accepted 2026-09-18
**Decides:** `run.project.refusal.unauthenticated-upgrade`, `run.project.invariant.read-only-socket`

## Context

A subscriber watches a run over a long-lived connection that carries folded snapshots as the
run progresses. That connection is unlike every other surface in the system in two ways: it is
held open for the life of a run rather than for the life of a request, and it is established by
a protocol upgrade — a handshake that allocates a channel, registers the connection with the
per-run hub, and hands it a broadcast receiver of 256 entries before a single frame of
application traffic moves.

The projection the socket carries is already the weakest thing in the run path. It is
best-effort, it is discarded on process restart, and a lost notification leaves it incomplete
while execution proceeds from the catalog and the journal. Its outputs appear as references and
byte counts rather than data, so permission to watch a run's shape is a strictly smaller grant
than permission to read what the run produced.

Two questions follow from that shape. A connection that mutates nothing is a connection whose
only authorization question is whether this caller may observe this run; a connection that can
also act carries a second, per-message authorization boundary, evaluated on a transport whose
framing and ordering are not the request path's. And a credential checked after the upgrade is
a credential checked after the resources are already committed, on a path where an
unauthenticated caller can name an arbitrary run id.

## Decision

The run-stream socket authenticates on the HTTP request that precedes the upgrade, applying the
same capability check the query surface applies. An invalid or missing credential raises
`RunStreamUnauthorized`, answers `401`, and is never upgraded. A run the hub has not seen
answers `404`, so naming an arbitrary path allocates no channel and registers nothing. Once
established, the connection is read-only: the run is the only writer into its own projection,
and a stop, an approval or any other action travels as an authenticated route.

## Options considered

| Option | What it buys | What it costs |
| --- | --- | --- |
| **Authenticate the upgrade request; socket carries snapshots only** *(chosen)* | One authorization boundary on the connection, evaluated once, on the request path where the capability check already lives. An unauthorized caller allocates nothing. | A user interface needs a second transport for actions, and an approval gate is unavailable over the socket a viewer already holds. |
| Accept a whitelisted set of control messages over the socket | A viewer acts without a second transport; one round trip saved on a stop. | Loses on surface count: it adds a per-message authorization boundary to a connection whose entire guarantee is that it cannot mutate the run, and buys only a round trip an authenticated route already serves. The messages also need their own ordering and idempotence rules against a transport that provides neither. |
| Accept arbitrary commands over the socket | The socket becomes the complete run interface. | Loses on the same criterion, more sharply: the blast radius of a stolen connection becomes every action on the run rather than visibility of its shape. |
| Upgrade first, authenticate on the first frame | The handshake stays simple; auth logic lives in one place inside the protocol. | Loses on what an unauthorized client obtains: the channel, the hub registration and the ring are allocated before the credential is read, so an unauthenticated caller consumes per-run resources by opening connections. |

## Criteria

1. **Authorization surface count** — how many distinct places a decision about this caller is
   made on this connection. *This is the criterion that decided it.* The projection's value is
   that observing is provably weaker than acting; a socket that can act collapses that
   distinction and forces every future run action to be defensible twice, once on the request
   path and once in a message handler. The round trip the rejected options buy is not worth a
   second place to get authorization wrong.
2. **What a compromised client reaches** — given a leaked credential or a hijacked connection,
   the difference between reading a run's shape and changing the run's outcome.
3. **Resource allocated before the credential is read** — whether an unauthenticated caller can
   make the process do work.
4. **Interface ergonomics** — how many transports a viewer needs to be useful.

## Consequences

Reasoning about the socket is finished once the upgrade is authorized: nothing that arrives on
it can change durable state, so the ordering and delivery weaknesses of the transport are
confined to display. Rate limiting and revocation apply on the request path, where they already
apply to every other surface.

The cost accepted is an interface one: an operator watching a run and deciding to stop it makes
a second, separately authorized call, and any approval gate that might otherwise have been
answered inline over the open connection is answered elsewhere. The latency of that extra call
is unmeasured and assumed small relative to the human decision in front of it.

Reversing the read-only property later is expensive in the way a widened grant always is:
clients written against a socket that can act will have been issued credentials chosen for a
viewer, and narrowing them afterwards breaks working software.

## Revisit triggers

- An interaction appears whose latency budget the request path cannot meet — an approval that
  must be answered inside a step's timeout rather than after it.
- The capability model gains per-message authorization with the same guarantees the request
  path provides, which removes the surface-count objection rather than accepting it.
- Connection churn from action-driven reconnects becomes a measured cost on the hub.
