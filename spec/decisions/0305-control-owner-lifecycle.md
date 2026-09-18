# 0305 — The configuration owner answers unconfigured storage and an uninitialized store as distinct refusals

**Status:** accepted 2026-09-18
**Decides:** `control.apply.refusal.owner-storage`, `control.apply.refusal.uninitialized-store`

## Context

The configuration owner persists the editable document and the applied versions. Hosted, it
takes object storage with strongly conditional writes, since the apply path advances a
pointer by compare-and-swap and the document persists under conditional replacement.
Filesystem storage serves a single-process local path, where the same conditionality comes
from having one writer.

Two different things can be missing when an edit or an apply arrives. The owner may have no
storage configured at all — a deployment that was started without its control source
declared, or with a backing store it cannot reach. Or the storage may be fine and the store
named in the request may simply not exist yet, because no guarded import has run against it.

These look identical from inside a single failure path and they are opposite in what they
ask of a person. The first is a deployment fault: the fix is configuration on the host, it
affects every store equally, and retrying will not help. The second is an ordinary lifecycle
state: the fix is to import the store, it affects that store alone, and the deployment is
healthy. Collapsing them gives an operator one message and two possible actions, with no
way to choose.

There is a further temptation at the second case. An edit against a store that does not
exist could create one. Store ids arrive from a URL, from a bookmark and from a typed field,
so the store that gets created on first edit is frequently the one nobody meant to name.

## Decision

The hosted configuration owner takes strongly conditional object storage; filesystem storage
serves a single-process local path. Missing owner configuration raises
`ConfigOwnerUnconfigured`, answered as `503`. An edit or an apply against a store that has
not been initialized raises `StoreNotInitialized`, answered as `409`. Every store takes an
explicit guarded import, an empty store included; no store comes into existence as a side
effect of an edit.

## Options considered

| Option | What it buys | What it costs |
| --- | --- | --- |
| **Two refusals with two status codes, and an explicit import per store** *(chosen)* | A caller reads the deployment's health and the store's readiness separately, and the status code alone routes an operator to the right action. A store exists because somebody imported it. | Standing up a deployment has one extra step per store, including for an empty one where the import does nothing but create the store. |
| One combined "not available" status for both | One path to implement, one message to write, no classification to get wrong. | Lost on actionability: an operator cannot tell whether to configure storage on the host or import a store, and the two are handled by different people on different timescales. A retry is correct for one and useless for the other. |
| Create a store implicitly on first edit | Zero-step onboarding; a new store starts collecting configuration immediately. | Lost on identity: a mistyped id becomes a real store that accepts edits, versions and a pointer, so the deployment accumulates stores nobody meant to create and the operator's typo is indistinguishable from intent. |
| Answer the uninitialized case as a not-found | Familiar shape; cheap. | Lost on meaning: the route exists and the caller is authorized, so the absence is a lifecycle state rather than a missing resource, and a not-found invites a client to stop rather than to import. |

## Criteria

1. **Whether a caller can tell a deployment fault from a store that is not ready** — the two
   demand different actions from different people. **This criterion decided.** Everything
   else is cheaper under the combined answer, but the combined answer makes the most common
   operator question — what do I do now — unanswerable from the response.
2. **Identity integrity of a store** — whether a store can come into existence without
   somebody deciding it should.
3. **Retry semantics carried by the answer** — `503` invites a retry after the host is
   fixed; `409` invites an import and will never clear on its own.
4. **Onboarding steps** — the number of deliberate actions between a fresh deployment and a
   usable store. This points the other way and is the accepted cost.

## Consequences

Monitoring separates cleanly: `ConfigOwnerUnconfigured` is a deployment-health signal that
fires across every store at once, while `StoreNotInitialized` is per store and expected
during onboarding. The set of stores a deployment holds is exactly the set somebody
imported, so a listing is trustworthy and a typo produces a refusal rather than a phantom.
The guarded import stays the one place a store's initial posture is decided.

The cost accepted: every store takes an explicit import, including an empty one, so
onboarding is one step longer per store and a scripted deployment has to perform it. An
operator who expects a store to spring into being from a URL gets a refusal instead, which
reads as friction precisely in the moment they are least oriented.

Reversing toward implicit creation is cheap to implement and hard to undo afterwards, since
the phantom stores it creates are indistinguishable from real ones.

## Revisit triggers

- Onboarding a deployment with many stores makes the per-store import the dominant setup
  cost.
- `ConfigOwnerUnconfigured` and `StoreNotInitialized` are observed being handled by the same
  runbook step, meaning the distinction is not being used.
- A storage backend appears whose conditional-write behavior is neither strongly conditional
  nor single-writer, so the two-posture description no longer covers what is deployed.
