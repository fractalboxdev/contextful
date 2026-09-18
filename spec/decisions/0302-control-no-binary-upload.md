# 0302 — Connector artifacts are published on a signed path rather than through the operator surface

**Status:** accepted 2026-09-18
**Decides:** `control.edit.refusal.uploaded-connector-binary`

## Context

The operator surface edits configuration. It picks a connector and a pinned version from
the registry so a replay reaches the same artifact, renders that connector's own declared
schema for its settings, shows the capability grants the connector requests, and records
what the operator grants or narrows. Every edit and every apply on that surface carries one
admin capability held as a server-side secret.

A connector is executable. The daemon loads it, the dispatcher starts it inside the pool,
and its capability grants — the outbound allowlist, the environment names it reads, the
clock it sees — are exactly the boundary that keeps a pull from reaching further than it
should. Those grants constrain what the artifact does; they do not constrain which bytes
the artifact is.

That is where the surface's authority and the daemon's authority meet. The admin
capability is scoped to changing configuration: which connector runs, on what schedule,
with which narrowed grants. If the same capability can also introduce new executable bytes,
then holding it is equivalent to running arbitrary code on every host that arms the
resulting version. One credential then governs two things of very different value, and the
weaker justification — an operator needs to adjust a schedule — sets the bar for the
stronger one.

The registry already exists, versions are already pinned by id, and publication into it
already runs its own path with its own signing.

## Decision

The operator surface references registered connectors by id and version. An artifact
uploaded through it raises `ConnectorUploadRefused`. Publishing a connector into the
registry runs a separate signed path, and the configuration surface reaches artifacts only
by naming ones that path has already admitted.

## Options considered

| Option | What it buys | What it costs |
| --- | --- | --- |
| **Reference by id and version; publication runs a separate signed path** *(chosen)* | The configuration credential grants configuration and nothing more. What runs on a host is decided by the signing path, whose custody is separate and narrower. | Publishing a connector needs a second path an operator cannot complete from the portal, so the shortest route from a written connector to a running one has a hand-off in it. |
| Accept an upload and verify a digest at load time | Catches a corrupted or truncated artifact; the operator stays in one surface. | Lost on authority: a digest proves the bytes arrived intact, not that anyone with signing custody approved them. The surface still admits new executable bytes on the strength of a configuration credential. |
| Accept an upload into a staging area pending a separate promotion | Keeps an approval step while letting the operator start in the portal. | Lost on authority, with a delay attached: the same credential still puts bytes on the path, and a promotion step that reviews an already-uploaded artifact is reviewed less carefully than one that has to be published deliberately. |
| Widen the surface but require a second admin capability for uploads | One portal, two credentials, explicit separation. | Lost on custody: a second secret held by the same server-side component is one component away from the first, and the separation exists in policy rather than in the path. It also doubles the surface that has to be audited for the stronger capability. |

## Criteria

1. **What the surface's own credential implies** — whether holding the configuration
   capability implies the ability to run chosen code on every host that arms the result.
   **This criterion decided.** An upload path converts configuration authority into
   execution authority over the daemon, and every rejected option leaves that conversion in
   place, differing only in how much ceremony surrounds it.
2. **Custody separation** — whether the people and machinery that approve executable bytes
   are distinguishable from those that adjust a schedule.
3. **Replay fidelity** — whether a past applied version resolves to the same artifact later.
   A pinned registry id does; an uploaded blob's identity depends on what the surface
   retained.
4. **Operator convenience** — the number of surfaces a person touches to get a connector
   running. This points the other way and is the accepted cost.

## Consequences

The admin capability becomes safe to grant more widely: an operator who can change
schedules and narrow grants cannot change what executes. Audit of what runs reduces to the
registry's publication record, which is one log with one custody story rather than two.
Replay is exact, since an applied version names a pinned id rather than bytes that may or
may not still be reachable.

The cost accepted: an operator cannot go from a written connector to a scheduled one
without leaving the portal. A deployment that wants a fast internal iteration loop pays for
it every cycle, and a first-time setup has an extra hand-off in it where a single surface
would have had none.

Reversing this is expensive, because every host that has armed a version under the current
rule did so on the assumption that configuration authority stops short of execution.

## Revisit triggers

- The signed publication path becomes unavailable to a class of deployment that still needs
  to run its own connectors, so the refusal blocks a supported configuration rather than an
  unsafe one.
- Capability grants tighten far enough that an arbitrary artifact's reach is genuinely
  bounded by them, removing the authority gap the refusal exists to close.
- Connector iteration cycles become a measured bottleneck for a deployment where the
  configuration credential and the signing custody are held by the same person anyway.
