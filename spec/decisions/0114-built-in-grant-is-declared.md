# 0114 — A compiled-in source that reaches an outside vendor declares its limiter grant in the pipeline's source configuration

**Status:** accepted 2026-09-18
**Decides:** `connector.meter.refusal.built-in-grant-absent`, `connector.meter.refusal.grant-unhonorable`

## Context

Two authoring paths implement one trait: a sandboxed component, and a native sibling
compiled into the binary for a hot path. Dispatch carries no branch naming which is called.
A sandboxed component declares its limiter capability in its own manifest, which the host
reads before it loads anything. A compiled-in source has no manifest. It has code, and code
is not a file a reviewer settles a grant from.

That leaves the compiled-in path with no place to state that its vendor traffic is metered,
and no way for anything to tell the difference between a source that touches no shared
quota — a filesystem walk, a store-reading derivation — and one that hammers a vendor API
with nothing coordinating it. Silence means both, which means silence means the second one
whenever it matters.

The pipeline's own source configuration is the file that already exists for exactly this
source, already carries host vocabulary, and is already read at build. It is where an
operator binds the source's other properties. Putting the grant there under the same key
the manifest path uses makes the declaration one concept with two spellings rather than two
concepts.

A declaration also has to be honorable. Reservation happens at the mediation point — the
engine's shared client for a compiled-in source. A native source that reaches its data
some other way has no mediation point, so a grant declared on it attaches nowhere. It would
read as metered in the configuration, produce no reservations, and consume the quota it
claims to respect.

## Decision

A compiled-in source that reaches an outside vendor declares its grant in the pipeline's
own source configuration under the same key the manifest path uses, and the build raises
`ConnectorGrantMissing` without it. A compiled-in source that does not reach its data
through the mediated client raises `ConnectorGrantUnhonorable` on a declared grant at load,
rather than accepting a grant it could not honor. Every compiled-in read records whether it
ran metered; the absence is stated once per run on the run record, and validation warns per
pipeline naming the connector and the block to add.

## Options considered

| Option | What it buys | What it costs |
| --- | --- | --- |
| **Declare in the pipeline's source configuration, refuse omission for a vendor-reaching source** *(chosen)* | Omission is distinguishable from a source touching no shared quota; the declaration sits in a file already read at build. | A compiled-in source authored and bound by the same operator carries a declaration duplicating what the code already knows. |
| Treat an omitted grant as a warning everywhere | Nothing is ever blocked; adoption is frictionless. | Lost on default direction for a vendor-reaching source: unmetered becomes the default, and a warning on a pipeline that works is filtered within a week. |
| Infer the grant from the source's code | No duplication; the declaration cannot drift from the code. | Lost on reviewability: the grant set becomes a property of a compiled binary rather than of a file an operator or an agent reads, which is the property the declared-capability model rests on. |
| Accept a grant on an unmediated source | One uniform rule; no second check. | Lost on honorability: the reservation attaches nowhere, and the operator reads a manifest as metered while the source spends the quota freely. |
| Require a manifest for compiled-in sources too | Full symmetry with the component path. | Lost on cost against benefit: a manifest for in-binary code adds a distribution artifact with a pin and a digest for bytes that ship with the engine. |

## Criteria

1. **Whether an omitted declaration is distinguishable from a source that touches no shared
   quota.** *This criterion decided it.* The compiled-in path is the fast path, which means
   it is the path the highest-volume sources take. A quiet default of unmetered there is
   the failure the metering capability exists to prevent, arriving exactly where volume is
   highest.
2. **Whether a declared grant can actually be honored.** Whether the reservation attaches
   to a real mediation point.
3. **Reviewability of the grant set.** Whether a reader settles it from a file.
4. **Duplication cost.** How much the declaration restates what the code states.

## Consequences

Validation names the connector and the block to add, so the fix is a paste. The run record
carries a per-run statement of whether compiled-in reads ran metered, which the
accountability surface reads directly. Both surfaces are gated on the project having bound
a quota at all, so a deployment that coordinates nothing is not nagged about coordination.

The accepted cost is duplication. For a compiled-in source, the author of the code and the
operator binding it are often the same person, and the declaration restates what the code
plainly does. That redundancy buys a property the code does not: a statement in a file, at
build time, that a reviewer can check without reading the source.

The refusal turns on whether a source reaches an outside vendor, which the build determines
from the source's own declaration of its egress. A source that misdeclares that — reaching
a vendor while claiming not to — defeats the check, and nothing here detects it.

## Revisit triggers

- A compiled-in source is found reaching a vendor without declaring egress, which would mean
  the property the refusal keys on is not self-enforcing.
- The number of compiled-in vendor-reaching sources grows to where the per-pipeline
  declaration is a recurring operator error rather than a one-line paste.
- The mediation point widens to cover a path a native source currently uses directly, which
  would shrink the set of grants that are unhonorable.
