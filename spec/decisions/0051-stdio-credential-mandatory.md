# 0051 — The process transport requires an explicit credential or an explicit owner flag

**Status:** accepted 2026-09-18
**Decides:** `read.embed.refusal.stdio-credential`

## Context

One client library covers four deployment shapes, and the transport changes only who
injects the credential, never what the credential is worth. A per-request capability token
runs the read under its own grants inside the engine, identically on every transport.

The process transport is the shape where nobody injects it. A local consumer spawns the
engine as a child over newline-framed JSON-RPC: no listener, no issuer key, no backend hop
in front of it. The other three shapes have a party whose job is to attach a token — a host
backend, a service binding, an exchange route — and a missing token there is a bug in a
component that was written to supply one. Here, a missing token is a caller who typed
nothing.

Inside the engine an unset token does not resolve to nothing. It resolves to the owner
context, which is the ungated branch: every tool, including run execution and memory
writes. The child's working directory has already selected a real project store by walking
up for its manifest, so the ungated branch is pointed at real data.

That makes forgetting one line the difference between a read-only consumer and a consumer
holding write and execute authority over the store, with nothing in the session saying so.

## Decision

Over the process transport a credential is mandatory: either a capability token string, or
an explicit owner flag naming the escalation. A client with neither raises
`StdioCredentialMissing` and opens no session. The owner context stays reachable, and
reaching it is a thing the caller wrote down.

## Options considered

| Option | What it buys | What it costs |
| --- | --- | --- |
| **Token or an explicit owner flag, or refuse** *(chosen)* | The ungated branch is entered only on purpose, and the refusal names the omission rather than proceeding under it. | Every local consumer writes one extra line, including for a trivially local read. |
| Treat an absent token as read-only | The convenient default is also the safe one; a quick local read needs no ceremony. | Loses on truthfulness: the engine's own resolution grants the owner context, so a read-only promise at the client is a guarantee the engine does not hold, and one call bypassing the library has full authority. |
| Default to the owner context | Matches the resolver's existing behavior exactly; zero-config local use. | Loses on escalation: a consumer that asked for a read gets write and execute authority, silently, on a store selected from the working directory. |
| Refuse only where the requested tool is a write or a run | Reads stay zero-config; the dangerous verbs are gated. | Loses on escalation for the same reason at one remove — the session is already owner-context, so the gate lives in the client and the transport still hands the caller an ungated engine. |

## Criteria

1. **Escalation on omission** — what authority a caller ends up holding when they forget
   the credential entirely. *This criterion decided it, outright.* Silent escalation to
   full write and execute authority is the worst available default, and it is also the most
   likely one to be hit, since forgetting is the common case and typing is the rare one.
   No amount of convenience on the other side of the ledger competes.
2. **Truthfulness of the client-side guarantee** — whether a promise the library makes is
   one the engine actually enforces. This eliminated the read-only default, which is
   otherwise the most attractive option here.
3. **Ceremony per local read** — lines of configuration for a trivial case. The chosen
   option scores worst, and that is the cost accepted.

## Consequences

Every local consumer writes one extra line, even for a read that touches one table on one
machine. The friction is real and it lands on the shape that was meant to be the easiest.

The owner context stays fully available, so no capability is lost — it is spelled. An audit
of a consumer's source answers what authority it runs under by reading one argument, rather
than by reasoning about what the resolver does with an absent value.

Reversing this is cheap in code and expensive in the field: relaxing the rule later would
silently widen authority for every consumer already passing the flag, since they would keep
working and new ones would stop being asked.

## Revisit triggers

- The resolver stops mapping an unset token to the owner context, at which point the
  escalation this refusal exists to prevent no longer exists.
- The process transport gains a party that injects a credential, making it resemble the
  other three shapes.
- Local consumers are observed passing the owner flag as boilerplate on reads that need no
  write authority, which means the flag has become noise rather than a statement.
