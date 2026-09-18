# 0322 — Enforcement, control-plane state, credential resolution, statement guarding and visibility each have exactly one implementation, reached by a call

**Status:** accepted 2026-09-18
**Decides:** `build.structure-tree.refusal.duplicated-capability`

## Context

Five capabilities decide what a caller may see and do: enforcement, control-plane state,
credential resolution, statement guarding and visibility. Each of them accretes over time
in a way that is invisible from outside. A refusal is added for an input nobody anticipated.
An audit record is emitted on a path that turned out to matter. A default is set to fail
closed after a case where failing open was wrong. None of that is legible in the
capability's outputs — it is legible only in its code.

The pressure to duplicate is constant and reasonable-sounding. A surface needs an answer
the engine already computes, the engine is a hop away, and the behavior looks small enough
to restate. The restatement is written against the capability's observable behavior,
because that is what the author can see: the same inputs produce the same outputs on the
cases they tried.

What the copy inherits is the outputs. What it does not inherit is everything the original
accreted: the refusals, the audit records, the fail-closed defaults. And a copy comes with
its own tests, written by the same author from the same understanding, which is the thing
that is incomplete. The copy's suite is green precisely on the cases the author thought of,
which are the cases they got right.

Divergence then arrives by drift rather than by edit. The engine's behavior moves — a new
refusal, a tightened default — and the copy does not, because nothing connects them. The
surface answers a question the engine would have refused, and no test anywhere reds.

A reachability problem is real in one shape. A surface running as a worker that holds the
sole handle to a storage binding, with no engine process on the request path, cannot make
the call at all.

## Decision

Enforcement, control-plane state, credential resolution, statement guarding and visibility
each resolve to one implementation inside the engine. A surface reaches one of them by
calling the engine route, or by querying the registered view that has the policy compiled
into it; a surface carrying its own copy raises `CapabilityDuplicated` at review, naming the
surface and the engine route that answers the same question. A surface genuinely unable to
call the engine implements an adapter against the engine's ports rather than restating the
contract. Four questions gate a change adding behavior to a surface, and a yes on any one
returns it.

## Options considered

| Option | What it buys | What it costs |
| --- | --- | --- |
| **One implementation per capability, reached by a call or a registered view** *(chosen)* | Every refusal, audit record and fail-closed default the capability holds applies wherever it is reached, and a change to the capability reaches every caller at once. | A surface with no engine process on its request path either adds a network hop or writes an adapter, and an adapter's derived artifact needs a staleness check in the gate — real work a hand-copied constant avoids. |
| A surface-side copy with a suite mirroring the engine's | No hop, no adapter, no coupling between a surface's release and the engine's. | Lost on failure mode. The copy's tests encode the copy's author's understanding, which is the thing that is wrong, so the suite stays green while the copy regresses on whatever property nobody thought to restate. |
| Sharing code as a library across languages | One implementation with no runtime hop, and drift caught at compile time. | Lost on reachability where the surface is a worker holding the only handle to a storage binding with no engine process on the request path; that case takes an adapter to the engine's ports regardless. |
| Allowing a copy where the capability is judged simple | Removes the hop in exactly the cases where it buys least. | Lost on failure mode again, and on the judgement itself: simplicity is assessed from the outputs, which is where an accreted refusal is least visible, so the rule would permit copies precisely where the author underestimated the capability. |

## Criteria

1. **Failure mode of a divergence** — whether a copy that drifts fails loudly or silently.
   *This criterion decided it.* These five capabilities decide access, so their failure is
   a caller seeing something they should not, and silence makes that indefinite. Effort and
   latency are the honest advantages of a copy and are recoverable costs; an unreported
   authorization divergence is not.
2. **Reachability from every surface** — whether the rule has a path for a surface off the
   engine's request path.
3. **Where a behavior change lands** — whether one edit reaches every caller.
4. **Latency and coupling** — the hop a call costs and the release coupling it creates.
   Outranked by the first criterion.

## Consequences

A behavioral change to any of the five lands in one place and reaches every surface. Review
has a decidable question — whether the engine performs it, whether the surface restates a
policy the registered view compiles in, whether a second state model mirrors an engine-owned
one, whether this surface would disagree silently after the engine's behavior moved — rather
than a judgement about how much duplication is acceptable.

The accepted cost falls on surfaces off the engine's request path. They add a hop or write
an adapter against the engine's ports, and an adapter that carries a derived artifact owes
the gate a staleness check. That is real work a hand-copied constant avoids, and it is
charged to the surfaces least able to avoid it.

## Revisit triggers

- The hop is measured to break a latency budget on a surface that cannot use a registered
  view, which would force either a new port shape or an accepted local computation.
- A capability is split such that part of it is genuinely stateless and side-effect-free,
  making a shared library across languages viable for that part.
- Adapters accumulate to the point where their derived artifacts, not the copies this rule
  forbids, become the main source of drift.
