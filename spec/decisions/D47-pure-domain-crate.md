# D47 — The domain crate holds types and ports with no I/O, and every edge points toward it

**Status:** accepted

## Context

`contextful-core` defines the shared vocabulary — `Schema`, `Batch`, `Record`, `Cursor`, the typed error enum — and the port traits every adapter implements. It links into all three profiles, so each of its dependencies is paid by the read replica, the daemon and the control plane at once. The pull toward impurity is constant and always locally sensible.

## Decision

`topology.package` keeps the centre pure. `contextful-core` performs no I/O and declares no async runtime, component host, HTTP client or columnar-format implementation; one raises `DomainCrateImpurity`, naming the dependency and the feature that pulled it. Every adapter crate depends on the centre and the centre on none of them; an outward edge raises `TopologyDependencyInversion` with its manifest line.

## Options

| Option | Lost on | Cost |
| --- | --- | --- |
| A pure centre of types and ports, all edges inward *(chosen)* | — | Adapters restate small conversions; a port change touches every adapter in one commit. |
| A centre carrying its own runtime and format implementation | Omission | The replica and control plane would link a runtime and format neither uses. |
| A shared utility crate both directions depend on | Direction reportability | A cycle would re-enter legally at every hop, with no single offending edge. |
| Ports beside their adapters, no centre | Omission | The error enum crossing both halves would have no neutral home. |

## Consequences

- The audit answers mechanically what the centre declares and whether any edge runs outward.
- A new target is an adapter crate behind existing ports.
- One convenience dependency in the centre silently ends the edge profile's budget, so the refusal names the pulling feature.

## Revisit

- Two adapters convert the same shape differently.
- A port change is deferred more than once because the wide commit is unmanageable.
- The edge profile's budget stops being the binding constraint.
