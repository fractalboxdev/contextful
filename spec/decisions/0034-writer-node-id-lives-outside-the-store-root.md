# 0034 — A writer node id resolves from the environment or a state directory outside the store root

**Status:** accepted 2026-09-18
**Decides:** `sync.merge.refusal.node-id-shape`, `sync.merge.refusal.node-id-under-control-plane-config`, `sync.lease.refusal.unresolved-node-id`

## Context

The node id is a machine's writer identity, and three mechanisms key on it. Run
directories carry it as a path segment, which is what keeps two machines writing one
logical run from colliding on identical part filenames. Request-ledger files carry it in
the filename, which is what gives the merge an ownership arm for that tier. And a lease
holder is identified by node id alone, so re-entrancy — the same holder re-acquiring its
own lease — is scoped to one machine.

That last one turns a duplicated id into a correctness failure rather than a cosmetic one.
Two machines presenting one identity both pass the holder check, both treat an existing
lease as their own, and one lease is granted to two writers. The exclusion the lease
exists to provide is gone, and nothing reports it, because from each machine's side the
lease object says exactly what it expects.

The ordinary way ingestion moves off a laptop is copying the store directory to a server.
That copy path is the constraint this decision is written against: anything persisted
inside the store root travels with the store, and the id is the one value that must not.

A control-plane configuration is the other tempting home for it, since deployment settings
live there and an operator would naturally write the id beside the bucket settings. A
control-plane snapshot is applied to every machine reconciling it, so an id declared there
makes all of them the same node — the same failure as the copy, arriving by a different
route and affecting more machines at once.

The id is also interpolated into a directory name and into a filename, so its character
set is not a matter of taste. A value carrying a separator, a parent reference or a
control character produces a key that escapes its intended position, which the prefix
confinement would then have to catch.

## Decision

A machine's writer node id resolves once per process: the environment variable, then the
project configuration key, then a random identifier generated once and persisted outside
the store root, at the state directory the platform gives the process owner. A node id
outside `^[A-Za-z0-9._-]{1,64}$` raises `SyncNodeIdInvalid`. A node id declared in a
control-plane configuration raises `SyncNodeIdShared`. The bucket lease refuses the
fallback node id `local` by name, raising `LeaseNodeIdLocal`, keeping that machine on the
per-machine lease and logging the environment variable that fixes it.

## Options considered

| Option | What it buys | What it costs |
| --- | --- | --- |
| **Resolve from the environment, then project configuration, then a persisted id outside the store root** *(chosen)* | Copying a store carries no identity with it. A machine keeps one stable id across process restarts, so its run tier and its ownership arm stay coherent. | A container with no writable state directory resolves to the fallback and stays on the per-machine lease, so multi-writer coordination is opt-in through the environment rather than automatic. |
| A node-id file at the store root | One place to look, travels with the store so a moved store keeps its history coherent, and needs no platform state directory. | Lost on copy safety. That is exactly the path by which two machines present one holder, and the lease's re-entrancy then grants one lease to two writers. |
| Declare the id under a control-plane configuration block | Deployment identity lives with the rest of the deployment settings, visible and reviewable. | Lost on the same criterion, at larger scale. A control-plane snapshot is applied to every machine reconciling it, making all of them the same node at once. |
| Generate a fresh id per process | No persistence at all, and two machines can never collide. | Lost on ownership. Run directories are keyed by node, so a per-process id fragments one machine's own tier across restarts and defeats the merge's ownership arm, whose deletions then never reach a consumer. |
| Let the bucket lease accept the fallback id | A container with no state directory still gets multi-writer coordination. | Lost on safety. Every such container resolves to the same fallback, so accepting it is accepting the duplicate-identity failure by default. |

## Criteria

1. **What happens when a store is copied to a second host** — whether identity travels
   with data. *This criterion decided it.* The copy is the ordinary way a deployment moves
   from a laptop to a server, so a design that fails on it fails on the common path rather
   than an exotic one, and the failure it produces — one lease held by two writers — is the
   single failure the lease exists to prevent.
2. **Coherence of a machine's own tier across restarts** — whether the ownership arm keyed
   on the id keeps naming the same set of objects.
3. **Safety of the default when nothing resolves** — where an unresolvable identity lands.
4. **Legality as a path component** — whether the value can be interpolated into a
   directory name and a filename without escaping.
5. **Operator convenience** — how much configuration a multi-writer deployment costs. This
   one was outranked; the whole cost is one environment variable, stated in the log line
   that declines the bucket lease.

## Consequences

A store is copyable without thought. Both machines generate or read their own ids, their
run tiers stay disjoint, and their ownership arms name what each of them actually wrote.
A machine that cannot persist an id degrades to the per-machine lease and says so,
naming the variable that would promote it — a visible, conservative degradation rather
than an invisible collision.

The accepted cost is that multi-writer coordination is opt-in on exactly the deployment
shape most likely to want it. A container image with a read-only filesystem and no state
volume resolves to the fallback every time, so an operator who does nothing gets
single-machine semantics from a fleet of identical containers, discoverable in the log
rather than in a failure.

Expensive to reverse: the id is interpolated into keys already written. Changing where it
resolves from changes which objects a machine claims to own, and a machine that acquires a
new id no longer owns its own history in the merge, so its earlier deletions stop
propagating.

## Revisit triggers

- Container deployments with no writable state path become the dominant shape, arguing for
  a derivation from an orchestrator-supplied identity rather than a persisted file.
- A store-copy workflow is introduced that deliberately carries identity — a promotion or
  a handover — which needs a defined way to transfer or retire an id rather than to avoid
  one.
- Ownership arms are re-keyed on something other than the node id, which would remove the
  tier-coherence requirement and permit a per-process value.
