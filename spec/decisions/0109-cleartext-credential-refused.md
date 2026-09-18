# 0109 — A reference-resolved header refuses a cleartext endpoint, with loopback exempt

**Status:** accepted 2026-09-18
**Decides:** `connector.import.refusal.cleartext-attachment`

## Context

The host implements the outgoing-HTTP import itself and attaches operator-bound material
to permitted requests. A manifest carries the name of a reference; the host carries the
value behind it and injects it at the moment of the call. The connector never observes the
plaintext, which means the connector is also not the party that can decide whether the
wire is safe to put it on. That judgment belongs to the one place every request passes.

An endpoint is a single string in a configuration file. Changing `https` to `http` is a
four-character edit, passes every schema check, and produces a manifest that reads exactly
like the safe one. The failure it causes is silent and total: a bearer token on a cleartext
hop is readable by every device along the path, and it stays valid afterwards. Nothing the
engine does later withdraws it.

Local development runs against fixture servers, recorded-interaction replays and
hand-rolled stubs, none of which hold a certificate. A rule with no exemption for loopback
makes the authoring toolkit's own test kit unusable and pushes authors toward disabling
the check rather than satisfying it. Loopback has two spellings, and a rule that knows only
the IPv4 one refuses half the fixture servers a modern toolchain starts.

## Decision

A header whose value resolves through the reference scheme is marked sensitive. A request
carrying a sensitive header to a cleartext endpoint raises `SecretCleartextEndpoint`
ahead of socket I/O, so the material does not reach the wire. Loopback is exempt in both
its IPv4 and its IPv6 spelling. Every sensitive header that did travel is recorded in the
run's list of headers that carried material, so what was attached to which request is
answerable from the run record rather than reconstructed from the manifest.

## Options considered

| Option | What it buys | What it costs |
| --- | --- | --- |
| **Refuse the request, exempt loopback** *(chosen)* | The disclosure does not happen; the development loop keeps its fixture servers. | An internal vendor reachable only over plain HTTP needs a proxy or a tunnel. |
| Warn and send | No configuration is ever blocked; every vendor stays reachable. | Lost on irreversibility: the warning is written after the material has left, and a warning that follows disclosure is a record of the incident, not a control. |
| Refuse loopback as well | One rule with no exceptions; nothing to reason about at a boundary. | Lost on the development loop: a fixture server is the point of the test kit, and an author who cannot run it edits the check out. |
| Strip the sensitive header and send the request anyway | The request still goes out; nothing leaks. | Lost on diagnosability: the vendor answers `401`, and the author debugs an authentication problem that is really a transport problem. |

## Criteria

1. **Irreversibility of the disclosure.** Whether the outcome can be undone once it has
   occurred. *This criterion decided it.* Every other criterion here is about cost paid
   repeatedly in small amounts; this one is about a cost paid once and never recovered. A
   credential on a cleartext hop cannot be called back, and the operator who made the edit
   is not the party who learns it happened.
2. **Blast radius of one manifest edit.** How much protection a single character of text
   can remove.
3. **Development-loop cost.** Whether the rule is satisfiable without a certificate
   authority on a laptop.
4. **Attributability.** Whether the run record answers which requests carried material.

## Consequences

Putting a credential on the wire in the clear takes more than editing a URL. The run
record gains a per-request answer to which headers carried material, which the accountability
surface reads without re-deriving it from the manifest.

The accepted cost: an internal vendor speaking plain HTTP on a private network has no direct
path. The operator terminates TLS in front of it or tunnels it to loopback. The engine
cannot tell a private network from the public one and does not try.

Two limits stand. The loopback exemption is decided on the socket address alone, so an
operator who port-forwards a remote host to `127.0.0.1` has an unrefused cleartext hop off
the machine, and the engine cannot see it. And sensitivity is derived from resolution, so a
token pasted inline as a literal carries no mark and this refusal does not reach it — that
spelling is refused elsewhere, at parse.

## Revisit triggers

- A deployment shape appears where plain HTTP is the only reachable form for a class of
  vendors an operator cannot put a proxy in front of.
- The reference scheme gains a way for an operator to declare a named endpoint as sitting
  inside a trusted network boundary, which would give the refusal a third answer besides
  loopback and everything else.
- Loopback forwarding to a remote host is observed in a real deployment, which would make
  the socket-address test insufficient on its own.
