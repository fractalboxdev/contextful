# 0014 — The commercial layer reaches the engine through the same surfaces every client uses

**Status:** accepted 2026-09-18
**Decides:** `topology.bound-application.refusal.public-surface`, `topology.bound-application.refusal.paid-feature`

## Context

The engine is the data plane: it ingests, stores, retrieves, remembers and enforces
policy, and it runs with the agent, offline, on one node, with no signup. A commercial
layer sits beside it and sells what that description excludes — people working together,
organization identity, who sees what across a team, an editing interface, anything
metered.

Two shortcuts present themselves once both halves exist in one repository. The first is a
private route: the commercial layer needs a capability the public surfaces do not yet
express, and an internal endpoint or an undocumented header delivers it in an afternoon.
The second is a license check on a capability the engine already has, which converts an
existing data-plane feature into revenue with a one-line predicate.

Both shortcuts change what the open engine is. A team that runs the engine against its
own bucket is supposed to hold the whole data plane; after either shortcut it holds a
subset, and the subset's boundary is wherever the commercial layer found it convenient to
draw. That boundary is invisible from inside a self-hosted deployment — nothing in the
corpus says a capability exists but is withheld — so the claim "a team loses nothing"
stops being checkable by reading the specification.

A private route also removes the pressure that keeps public surfaces adequate. The tool
protocol, the manifest and plan format, the capability-token format and the sync protocol
are good in proportion to how much load they carry; a back channel diverts exactly the
load that would have exposed their gaps.

## Decision

The commercial layer reaches the engine through the tool protocol, the manifest and plan
format, the capability-token format and the sync protocol — the surfaces every client
reaches it through. Anything the commercial layer does, an operator scripts against the
bare engine. A private route, an undocumented header or a privileged build reserved for
the commercial layer raises `PrivateBackChannel`, naming the surface, and a capability the
commercial layer needs is built as a public engine surface first. A paid feature is
net-new — a sync service, identity, attestation, an administrative interface — and gating
an existing data-plane capability behind a license check raises `PaywalledDataPlane`,
naming the capability.

## Options considered

| Option | What it buys | What it costs |
| --- | --- | --- |
| **Public surfaces only; paid features are net-new** *(chosen)* | The open engine is the whole data plane, and the claim is checkable from the corpus | A capability the commercial layer needs ships as a supported public surface first, which is slower and occasionally exposes a surface before it is ready to carry outside callers |
| A privileged internal API for the commercial layer | Fastest path to any capability; no public design cost | Lost on wholeness: the open engine becomes a subset of itself, with a boundary no self-hosted reader can see, and the public surfaces stop being exercised by their most demanding consumer |
| Paywall an existing data-plane capability behind a license check | Immediate revenue with no new code | Lost outright on wholeness: it withdraws a capability a self-hosted team already relies on, and every later capability becomes a candidate for the same withdrawal |
| Ship two engine builds, one with the commercial hooks compiled in | Keeps the license check out of the open build | Lost on wholeness and on verification: the build the commercial layer runs is not the build anyone else can inspect, so a behavioral difference cannot be reproduced |

## Criteria

1. **Wholeness of the data plane** — whether a self-hosted team holds everything the
   corpus states. *(the one that decided it)* A privileged path makes the open engine a
   subset of itself, and the subset's boundary is undiscoverable from inside a
   deployment. Speed of delivery and revenue timing are real and were weighed; they are
   recoverable, and a boundary that readers cannot see is not, because nothing later
   surfaces it.
2. **Exercise of the public surfaces** — whether the most demanding consumer uses them.
3. **Reproducibility** — whether a behavior the commercial layer shows can be reproduced
   against the open build.
4. **Time to deliver a capability** — how long a needed capability takes to reach the
   commercial layer.

## Consequences

The public surfaces stay adequate because the commercial layer depends on them, and an
operator scripting against the bare engine can reconstruct anything the commercial layer
does at the data plane. A self-hosted deployment needs no account and no remote check.

The cost accepted is latency on the commercial roadmap. A capability arrives as a
designed, documented, supported public surface before the commercial layer can use it,
which is slower than an internal shortcut, and it occasionally means a surface is public
before its shape has settled — a surface exposed early is one the corpus is then obliged
to keep.

Revenue is confined to net-new work. A quarter where the net-new surface is thin cannot be
rescued by metering something the engine already does, so pricing pressure lands on
building rather than on gating.

## Revisit triggers

- A capability the commercial layer needs has no expressible form in any of the four
  public surfaces, and designing one would change a surface every client depends on.
- Two consecutive net-new features turn out to be unimplementable without privileged
  access to internal state, indicating the surface set itself is short a member rather
  than the rule being wrong.
- A public surface is exposed solely for the commercial layer and acquires no other
  caller over a full support cycle, which makes it a private route with public spelling.
