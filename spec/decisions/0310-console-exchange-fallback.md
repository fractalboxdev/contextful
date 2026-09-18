# 0310 — A refused per-reader mint falls back to a shared credential where one exists and surfaces the refusal where none does

**Status:** accepted 2026-09-18
**Decides:** `console.ground.refusal.mint-refused`

## Context

A store on exchange authentication receives a credential per reader. The console server
posts the reader's verified assertion to that store's exchange route, carries what is minted
through the session's reads, and caches it against the pair of reader and assertion under
its own lifetime, so a session exchanges once rather than once per call. A store not on
exchange authentication is reached with a shared credential the deployment configured.

A mint can be refused. The exchange route can be unreachable, the store can decline this
particular assertion, the route can be mid-rollout and not yet answering. Each of those
leaves the turn with no per-reader credential and a reader waiting.

Two properties collide at that point. Attribution says reads should be traceable to the
person who made them, and the shared credential provides no such trace. But a reader cannot
diagnose any of this: to them a store that answers nothing and a store that holds nothing
look identical, and an empty store is a normal state this surface deliberately answers
plainly. A refusal that renders as emptiness is therefore a refusal that gets mistaken for
a fact about the data.

Rollout matters too. Moving a store onto exchange authentication is a change to the store's
own routes, and during that change the route can decline while the shared credential still
works.

## Decision

A refused mint raises `ConsoleTokenExchangeRefused`. A store carrying a shared credential
falls back to it and the session continues under that credential. A store carrying none
surfaces the refusal to the reader rather than reading as empty. The refusal is recorded in
both cases, so a session served under the fallback is marked as one.

## Options considered

| Option | What it buys | What it costs |
| --- | --- | --- |
| **Fall back where a shared credential exists; surface the refusal where none does** *(chosen)* | A reader never mistakes a refusal for an empty store, and a store can move onto exchange authentication without a window in which its surface is dead. | Two authentication postures coexist on one surface, so a store carrying a shared credential can serve a reader under it after a mint refusal, and an audit reading per-reader attribution has a gap the refusal record is the only marker of. |
| Fail the session outright on any refused mint | One posture, complete attribution, nothing to explain. | Lost on rollout: a store mid-change has no working surface for the duration of the window, and the window is exactly when an operator is watching for breakage that is expected. |
| Fall back silently to the shared credential in every case | Simplest reader experience; the surface always works. | Lost on distinguishability and on reach: a store with no shared credential would still read as empty under a refusal, and a store with one would quietly serve a reader under a wider scope than the mint would have given them, with nothing recorded. |
| Retry the mint on a backoff and block the turn | Rides out a transient refusal without changing posture. | Lost on distinguishability: a stalled turn and a slow store look identical to a reader, and the reader's only action — waiting — is the one that cannot tell them apart. A persistent refusal ends in a timeout that explains nothing. |

## Criteria

1. **Whether a refusal can be mistaken for an empty store** — the reader's ability to tell
   "nothing is stored" from "nothing was served". **This criterion decided.** This surface
   answers emptiness plainly and asks readers to trust that answer; a failure mode that
   borrows that answer's shape corrodes every legitimate use of it.
2. **Whether the fallback widens what a reader reaches** — and whether the widening is
   recorded where an audit looks.
3. **Continuity across a posture change** — whether a store can be rolled onto exchange
   authentication without downtime.
4. **Attribution completeness** — whether every read names the reader who caused it. This is
   the criterion the decision concedes.

## Consequences

A store moves onto exchange authentication as a gradual change rather than a cutover, since
the fallback covers the window and the refusal record measures how wide it is. A reader on a
store with no shared credential gets a sentence describing an unreachable store, in the same
register as every other error on this surface, rather than an empty result they would have
believed. The refusal record becomes the operational signal that a store's exchange route is
misbehaving, visible before anyone reports missing data.

The cost accepted: attribution has a gap. Reads served under the fallback carry the shared
credential's identity and a wider scope than the reader was to be minted for, and the only
thing tying them back to the reader is the refusal record sitting beside them. An audit that
reads the credential alone will attribute those reads to the deployment. A store that never
notices its exchange route is failing therefore serves correctly and attributes wrongly for
as long as nobody reads the refusals.

## Revisit triggers

- Refusal records accumulate on a store in steady state, meaning the fallback is carrying
  normal traffic rather than a rollout window.
- Attribution becomes a compliance requirement for a deployment, at which point that
  deployment needs the fail-outright posture and the fallback becomes configurable.
- The shared credential is retired as a concept, which removes the branch this decision is
  built on.
