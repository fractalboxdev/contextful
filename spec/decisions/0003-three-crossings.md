# 0003 — Exactly three contracts cross between the read path and the run path

**Status:** accepted 2026-09-18
**Decides:** `topology.compose.refusal.crossing`

## Context

One workspace compiles two internal halves. The read path holds data at rest and its
retrieval — the context store, the catalog, the query and ranking surface, memory queries,
bucket sync. The run path holds execution — the journal, the scheduler, cursor commit,
awakeables, and every connector invoked as a journaled step. Neither half is a shippable
unit, so the boundary between them is a design constraint rather than a packaging one, and
nothing about the build makes it visible on its own.

The edge profile is what the boundary exists for. A build linking no execution core, no
scheduler and no component host still serves reads of data a full daemon produced, under
the identical part-and-manifest layout and the identical connector contract for native
pulls. That is only possible while the read path's dependencies on the run path are a
short, enumerable list — every additional one is a piece of the run path the read replica
has to link or stub.

The boundary is also where capability tokens move and where connector-produced records
arrive. A general call surface between the halves would put those two facts in the same
place as everything else, and a security review would have to cover the whole surface
rather than three versioned contracts with conformance suites.

## Decision

Exactly three contracts cross between the halves: the connector interface world, the
columnar-part-plus-manifest layout, and capability tokens. Each carries its own version
and a conformance suite both sides run, and a member added to any of the three passes a
security review before it is admitted. A call path carrying state between the halves
outside those three raises `TopologyUndeclaredCrossing`, naming the two crate endpoints. A
feature that appears to need a fourth crossing is a design question rather than an
implementation detail.

## Options considered

| Option | What it buys | What it costs |
| --- | --- | --- |
| **Three named, versioned contracts and nothing else** *(chosen)* | The run path stays droppable, so a read replica is a real build target. A security review covers three versioned surfaces with conformance suites rather than a call list. | A feature naturally wanting a fourth crossing is routed through one of the three or deferred, and a run-path signal fitting none of them travels as manifest data. |
| A general internal RPC surface between the halves | Any new interaction is cheap and needs no design conversation. | Lost on review surface: every call becomes a place a capability can enter, and nothing forces an addition through a review. The set of things crossing stops being enumerable. |
| Direct crate dependencies from the read path onto the run path | Simplest to write; the compiler resolves everything with no contract to version. | Lost on composability: the run path stops being droppable and the edge profile is forfeited, taking the function-class target with it. |
| Two crossings, with capability tokens folded into the manifest layout | One fewer contract to version. | Lost on review surface: token format and delegation would version with the on-disk layout, so an authority change would ride a storage release. |

## Criteria

1. **Composability** — how much of the system still works when the run path is absent.
2. **Review surface** — how much area a security review has to cover to be confident a
   capability cannot enter unnoticed. **This criterion decided.** Three contracts are few
   enough that each can carry a version, a conformance suite both sides run, and a rule
   that a new member is reviewed before admission; a general surface makes that impossible
   in principle rather than merely expensive.
3. **Authoring friction** — how hard it is to add a legitimate new interaction. The chosen
   option is the worst of the four here and lost this criterion knowingly.
4. **Independent versioning** — whether the three concerns release on their own schedules.

## Consequences

The edge profile stays achievable: no scheduler, no component host, and reads that honor
the same layout a full daemon writes. Each crossing versions independently, so a connector
interface change and a token format change do not have to ship together. The refusal names
crate endpoints, so an undeclared crossing is reported as a pair rather than as a diffuse
architectural complaint.

The cost accepted: the boundary is held by review and by the contract list rather than by a
compiler, so a quiet fourth crossing compiles cleanly and is caught only when the audit
runs. A feature whose natural shape is a fourth crossing is either bent through one of the
three or deferred, and a run-path signal fitting none of them has to travel as manifest
data, which is a slower and less expressive channel than a direct call.

Reversing this — adding a general call surface later — is cheap to write and very expensive
to undo, since every call written against it becomes a dependency of the read path on the
run path.

## Revisit triggers

- Manifest data is repeatedly used as a message channel for run-path signals that have no
  storage meaning.
- A shipped feature is deferred more than once for lack of a crossing, and the deferral is
  not resolved by bending it through an existing one.
- A conformance suite for one of the three grows to cover interactions unrelated to that
  crossing's stated concern, indicating the three have absorbed a fourth.
