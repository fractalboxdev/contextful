# 0007 — The domain crate holds types and ports with no I/O, and every dependency edge points toward it

**Status:** accepted 2026-09-18
**Decides:** `topology.package.refusal.domain-crate`, `topology.package.refusal.dependency-direction`

## Context

Thirteen crates compose the workspace, and `contextful-core` sits at the centre of them. It
defines the vocabulary both halves share — `Schema`, `Field`, `DataType`, `Batch`,
`Record`, `Cursor`, `CursorKind`, the pipeline specification, and the typed error enum that
crosses every port boundary — together with the port traits every adapter implements:
`Source`, `Destination`, `Transform`, `Catalog`, `Clock`, `LeaseStore`, `RateLimiter`,
`EntityCatalog`, `RequestLedgerSink`, and the derive family.

The centre links into all three profiles by construction, which makes every one of its
dependencies a dependency of the read replica, the daemon and the control plane at once. A
columnar-format implementation or an async runtime in the centre is therefore paid for by
the profile whose whole purpose is to link neither.

The pull toward impurity is constant and always locally sensible. A `Batch` type wants to
know how to serialize itself. A `Cursor` wants to read a clock. A `Catalog` port wants a
default implementation. Each of those pulls an adapter concern into the centre, and each
one, once there, is a dependency no profile can drop.

The same pressure produces inverted edges. An adapter has a helper the centre could use,
so the centre depends on the adapter "just for that", and the dependency graph acquires a
cycle that the audit can no longer report as a single offending edge.

## Decision

`contextful-core` holds pure domain types and the port traits every adapter implements, and
performs no I/O. It declares no async runtime, no component host, no HTTP client and no
columnar-format implementation; doing so raises `DomainCrateImpurity`, naming the
dependency and the feature that pulled it. Every adapter crate depends on the domain crate
and the domain crate depends on none of them; an edge running the other way raises
`TopologyDependencyInversion`, naming both crates and the edge's manifest line. A port's
implementation therefore changes without the port moving.

## Options considered

| Option | What it buys | What it costs |
| --- | --- | --- |
| **A pure centre of types and ports, all edges inward** *(chosen)* | A profile omits a heavy dependency entirely rather than linking it unused. An adapter is replaceable without the port's definition moving. | An adapter restates small conversions the centre could have supplied, and a port change touches every adapter in one commit. |
| A domain crate carrying its own async runtime and columnar-format implementation | Convenience at every call site; no conversion boilerplate in the adapters. | Lost on omission: the centre links into all three profiles, so the read replica and the control plane both pay for a runtime and a format implementation neither uses. |
| A shared utility crate both directions depend on | Kills the duplication the pure centre causes; one place for common helpers. | Lost on direction: the cycle re-enters through it, and the audit stops being able to name a single offending edge — the graph is legal at every hop and circular overall. |
| Ports defined beside their adapters, with no central crate | No centre to keep pure; each area owns its own abstraction. | Lost on omission again, and on the typed error enum: the error crossing both halves would have no home that both halves can depend on without one depending on the other. |

## Criteria

1. **Omission** — whether a profile can leave a heavy dependency out of its link entirely.
   **This criterion decided.** A dependency in the centre links into all three profiles,
   so the centre's purity is what makes the edge profile's budget reachable at all; every
   other criterion is about convenience by comparison.
2. **Direction reportability** — whether a violation can be named as one edge with a
   manifest line. A single inward direction gives that; a shared utility crate does not.
3. **Port stability** — whether swapping an adapter moves the port.
4. **Duplication** — small conversions restated across adapters. The chosen option is the
   worst here and lost this criterion deliberately.

## Consequences

The dependency audit answers two questions mechanically: what does the centre declare, and
does any edge run outward. A new target is an adapter crate behind existing ports rather
than a branch inside the engine. Profile feature bundles mean what they say, since nothing
is pulled in transitively through the centre.

The cost accepted: adapters carry duplicated conversion code the centre could have written
once, and that duplication grows with the number of adapters. A port change is a wide
commit — every implementor moves together — which makes port evolution deliberately
awkward and occasionally delays a change that is obviously right.

Reversing the purity rule is cheap to do and hard to notice: one convenience dependency in
the centre silently ends the edge profile's budget story, which is why the refusal names
the feature that pulled it rather than only the dependency.

## Revisit triggers

- Conversion duplication across adapters becomes a source of behavior divergence rather
  than mere repetition — two adapters convert the same shape differently.
- A port change is deferred more than once purely because the wide commit is unmanageable.
- The edge profile's budget stops being the binding constraint, removing the reason
  omission outranks convenience.
