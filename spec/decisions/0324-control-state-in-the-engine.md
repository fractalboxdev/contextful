# 0324 — The engine owns the control-plane state model, and an unreachable store produces an explicit failure rather than a local writer

**Status:** accepted 2026-09-18
**Decides:** `build.structure-tree.refusal.weak-conditional-backend`, `build.structure-tree.refusal.silent-local-writer`

## Context

Control-plane state is the configuration the system runs on: persisted collaborative
documents, their validation, immutable version claims, and the pointer to the current
version. It is small, it is edited by people rather than by pipelines, and every other part
of the system reads it as authoritative. Its correctness property is singular — at any
moment there is exactly one answer to what the current version is.

That property is a statement about writers. One engine assigning every version and
materializing every manifest means a version ordering exists by construction. Two
independent writers means two orderings that have to be reconciled by something, and
nothing in the system is positioned to reconcile them: a manifest is read by whoever loads
it next, with no evidence of what the other writer intended.

The tempting second writer arrives as a fallback. A surface adapter starts, finds no
credential or an uninitialized store, and has a working local path that would let an
operator proceed. Taking it produces a configuration that looks edited and is invisible to
everything else, and the operator discovers the split later, from behavior that does not
match what they wrote.

Independent writers are otherwise legitimate — two operators, two adapters — and they
serialize with object-store conditional replacement: the loser retries from the document
that won rather than from the one it read. That mechanism has a hard prerequisite. A
backend offering only last-writer-wins accepts both writes and reports success to both,
which means the losing edit is gone and no party can detect that it lost. The race is not
merely unresolved; it is unobservable.

## Decision

The engine holds the control-plane state model and assigns every version, and a surface
adapter reaches it through the authenticated, store-scoped API. An absent credential or an
uninitialized store raises `ControlStateUnreachable`; a local writer is never substituted.
Scoped service mode over a backend lacking strong conditional replacement raises
`ConditionalWriteUnsupported` at startup. A surface holding its own model of engine-owned
state re-derives it on every read rather than storing a copy.

## Options considered

| Option | What it buys | What it costs |
| --- | --- | --- |
| **One engine-assigned version, conditional replacement between independent owners, explicit failure when the store is unreachable** *(chosen)* | Exactly one ordering of versions, a losing writer that knows it lost and retries from the winner, and a misconfiguration that surfaces at startup rather than as divergent behavior later. | A deployment on a backend with weak conditional replacement has no supported scoped service mode, and an operator must provision credentials before the first edit rather than discovering the gap later. |
| A local writer as a fallback when credentials are absent | The surface starts and an operator proceeds without provisioning anything. | Lost on writer count: a silent second writer produces two versions nobody can order, and the operator's edits are invisible to every other reader until the divergence shows up as behavior. |
| Last-writer-wins on the configuration document | Runs on any object store, with no capability probe and no retry path. | Lost on edit loss: a backend without strong conditional replacement cannot detect the race it loses, so an edit disappears with both writers reporting success. |
| An advisory lock or a lease in front of the document | Serializes writers on backends that cannot replace conditionally, reusing a familiar mechanism. | Lost on writer count under failure: a lease expiring while its holder is mid-write admits a second writer with no way to order the two results, so the guarantee is weaker exactly when it is needed. |
| A surface-side cache of engine-owned state | Fewer reads, and a surface that keeps answering when the engine is slow. | Lost on the single-answer property: a stored copy is a second version with its own staleness, which is why an adapter re-derives on every read instead. |

## Criteria

1. **How many writers a configuration value can have** — whether a second ordering can come
   into existence. *This criterion decided it.* Every failure mode here traces back to two
   writers, and the ones that arrive silently — a fallback path, an overwriting backend —
   are unrecoverable because nobody learns that the split happened. Availability and
   backend portability are the honest advantages of the rejected options and are worth less
   than an edit that cannot be lost without notice.
2. **Whether two owners can lose an edit** — whether the backend can detect the race it
   loses. This is what makes conditional replacement a prerequisite rather than an
   optimization.
3. **When a misconfiguration surfaces** — at startup, or later as divergent behavior.
4. **Backend portability** — how many object stores support the mode. Outranked, and the
   accepted cost.

## Consequences

Version assignment and manifest materialization have one site, so no adapter reconciles
state and no reader has to ask which copy is current. Two independent owners editing at
once is an ordinary, safe operation with a defined retry. A deployment either has a working
control plane or fails to start with a named error, which is the difference between a
misconfiguration found in the first minute and one found in the first incident.

The accepted cost is deployment reach and operator sequencing. A backend without strong
conditional replacement supports no scoped service mode at all — a deployment blocked on a
platform property rather than on anything in the tree — and an operator provisions
credentials before the first edit rather than starting and filling them in later.

## Revisit triggers

- A required deployment target lacks strong conditional replacement, forcing a choice
  between an external coordination service and dropping scoped service mode there.
- Edit contention rises to where retry-from-the-winner is observed to starve a writer,
  which would argue for an ordering mechanism above the document.
- Re-deriving engine-owned state on every read becomes the dominant read cost on a surface,
  which re-opens caching under an invalidation the engine drives.
